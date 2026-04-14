use std::{sync::Arc, time::Duration};

use agent::{
    agent::{
        technical::{TechnicalAgent, TechnicalAgentArgs, TechnicalAgentMessage},
        Model,
    },
    tool::ScoreQueryTool,
};
use anyhow::{Context, Result};
use api_client::http::binance::ArchiveProvider;
use common::{
    config::{Appconfig, ProxyConfig},
    utils::CooledProxyPool,
    Symbol,
};
use notify::telegram::BotApp;
use quant::analyzer::{
    context::{
        gravity::GravityAnalyzer, regime::MarketRegimeAnalyzer,
        volatility::VolatilityEnvironmentAnalyzer, volume::VolumeStructureAnalyzer,
    },
    signal::{fakeout_detector::FakeoutDetector, momentum_resonance::ResonanceAnalyzer},
    AnalysisEngine, Analyzer, Config,
};
use ractor::{cast, Actor};
use rig::tool::ToolSet;
use service::{
    analysis::AnalysisService,
    integrity::{context::FeatureContextManager, DataIntegrityManager},
};
use storage::postgres::Storage;
use tokio::sync::mpsc;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = Appconfig::global();
    let symbols = Symbol::all();

    // ========== 基础设施层 ==========
    let proxy_pool = Arc::new(create_proxy_pool(&config.proxy));
    let storage = Arc::new(init_storage(&config.database).await?);
    let archive = Arc::new(ArchiveProvider::new(proxy_pool.clone()));

    // ========== 核心分析层 ==========
    let ctx_manager = Arc::new(FeatureContextManager::new(&symbols));
    let analyzers: Vec<Box<dyn Analyzer>> = vec![
        Box::new(VolatilityEnvironmentAnalyzer),
        Box::new(MarketRegimeAnalyzer),
        Box::new(GravityAnalyzer),
        Box::new(VolumeStructureAnalyzer),
        Box::new(ResonanceAnalyzer),
        Box::new(FakeoutDetector),
    ];
    let engine = Arc::new(AnalysisEngine::new(Config::default(), analyzers));
    let analysis_service = Arc::new(AnalysisService::new(engine.clone()));

    let integrity = Arc::new(DataIntegrityManager::new(
        symbols.clone(),
        ctx_manager.clone(),
        proxy_pool.clone(),
        storage.clone(),
        archive.clone(),
        analysis_service.clone(),
    ));
    integrity.start();
    info!("Data integrity manager started");

    // 修改通道类型：只需发送 (String, Symbol)
    let (tg_tx, tg_rx) = mpsc::channel::<(String, Symbol)>(256);
    // 命令通道保持不变（如果需要）
    let (cmd_tx, mut cmd_rx) = mpsc::channel(100);

    // 创建 BotApp（简化版仅需 proxy_pool 和 token）
    let bot = BotApp::new(config.telegram.token.clone(), proxy_pool.clone()).await?;

    tokio::spawn(async move {
        if let Err(e) = bot.run(tg_rx).await {
            error!("Telegram bot error: {:?}", e);
        }
    });
    info!("Telegram bot started");

    // 分析结果订阅转发至广播通道
    let mut analysis_rx = analysis_service.subscribe();
    let tg_sender = tg_tx.clone();

    tokio::spawn(async move {
        info!("Analysis notification worker started");
        while let Ok(event) = analysis_rx.recv().await {
            let msg = event.audit.to_markdown_v2();
            let _ = tg_sender.send((msg, event.audit.signal.symbol)).await;
        }
        info!("Analysis notification worker stopped");
    });

    // AI Agent 初始化
    let model = Model::openai(
        "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",
        "https://openrouter.ai/api/v1",
        "openai/gpt-5.4",
    )?;

    let score_tool = ScoreQueryTool::new(ctx_manager.clone(), engine.clone());
    let tool_set = ToolSet::builder().static_tool(score_tool).build();

    let agent_args = TechnicalAgentArgs {
        model,
        tx_out: tg_tx.clone(),
        tool_set,
    };
    let (agent_actor, _handle) = Actor::spawn(
        Some("TechnicalAgent".to_string()),
        TechnicalAgent,
        agent_args,
    )
    .await?;
    info!("AI Agent started");

    tokio::spawn(async move {
        while let Some((cmd, chat_id)) = cmd_rx.recv().await {
            info!("Received command: {} from {}", cmd, chat_id);
            let _ = cast!(agent_actor, TechnicalAgentMessage::Task(cmd, chat_id));
        }
    });

    info!("System ready, waiting for Ctrl+C...");
    tokio::signal::ctrl_c().await?;
    info!("Shutting down...");

    Ok(())
}

// ========== 辅助函数 ==========

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

fn create_proxy_pool(config: &ProxyConfig) -> CooledProxyPool {
    CooledProxyPool::new(config.socks_proxy_list.clone(), Duration::from_secs(300))
}

async fn init_storage(db_config: &common::config::DatabaseConfig) -> Result<Storage> {
    let storage = Storage::new(db_config).context("Failed to connect to database")?;
    storage
        .initialize_all()
        .await
        .context("Failed to initialize database schema")?;
    Ok(storage)
}
