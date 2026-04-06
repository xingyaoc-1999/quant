use std::{sync::Arc, time::Duration};

use agent::{
    agent::{
        technical::{TechnicalAgent, TechnicalAgentArgs, TechnicalAgentMessage},
        Model,
    },
    tool::ScoreQueryTool,
};
use anyhow::{Context, Result};
use quant::analyzer::{
    context::{
        level_proximity::LevelProximityAnalyzer, market_regime::MarketRegimeAnalyzer,
        volatility_environment::VolatilityEnvironmentAnalyzer,
        volume_structure::VolumeStructureAnalyzer,
    },
    AnalysisEngine, Analyzer, Config,
};
use rig::tool::ToolSet;

use api_client::http::binance::ArchiveProvider;
use common::{
    config::{Appconfig, ProxyConfig},
    utils::CooledProxyPool,
    Symbol,
};

use notify::telegram::BotApp;
use ractor::{cast, Actor};
use service::{context::FeatureContextManager, integrity_manager::DataIntegrityManager};
use storage::postgres::Storage;

use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // 1. 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cfg = Appconfig::global();

    // 2. 初始化存储层
    let storage = Storage::new(&cfg.database).context("Failed to connect to database")?;
    storage
        .initialize_all()
        .await
        .context("Failed to initialize database schema")?;
    let storage = Arc::new(storage);

    // 3. 初始化核心组件
    let symbols = Symbol::all();
    let proxy_pool = Arc::new(setup_proxy_pool(&cfg.proxy));

    // ctx_manager 是核心共享资源
    let ctx_manager = Arc::new(FeatureContextManager::new(&symbols));

    let archive_provider = Arc::new(ArchiveProvider::new(proxy_pool.clone()));

    // ✅ 修复：克隆 ctx_manager 传入数据完整性管理器
    let integrity_manager = Arc::new(DataIntegrityManager::new(
        symbols.clone(),
        ctx_manager.clone(),
        proxy_pool.clone(),
        storage.clone(),
        archive_provider.clone(),
    ));

    // 4. 初始化 Telegram Bot
    let (tx_to_tg, rx_out) = tokio::sync::mpsc::channel(100);
    let (tx_in, rx_from_tg) = tokio::sync::mpsc::channel(100);

    let tg_app = BotApp::new(cfg.telegram.token.clone(), proxy_pool.clone()).await?;

    info!("🚀 Starting Telegram Bot...");
    
    let ctx_for_tg = ctx_manager.clone();
    let storage_for_tg = storage.clone();
    let archive_for_tg = archive_provider.clone();

    tokio::spawn(async move {
        if let Err(e) = tg_app
            .run(tx_in, rx_out, ctx_for_tg, archive_for_tg, storage_for_tg)
            .await
        {
            error!("Telegram Bot Runtime Error: {:?}", e);
        }
    });

    // 5. 启动后台服务
    info!("🚀 Starting Integrity Manager...");
    integrity_manager.start();

    // 6. 初始化 AI Agent 与 分析引擎
    let model = Model::openai(
        "sk-KL85Y5XsOM7kcm7qzSSFUUJ5iqEAcU3kiV4rAGsWPC6rFlp7",
        "https://aiberm.com/v1",
        "openai/gpt-5.4",
    )?;
    let analyzers: Vec<Box<dyn Analyzer>> = vec![
        Box::new(VolatilityEnvironmentAnalyzer), // 1. 环境基调
        Box::new(MarketRegimeAnalyzer),          // 2. 确定方向
        Box::new(VolumeStructureAnalyzer),       // 3. 能量确认
        Box::new(LevelProximityAnalyzer),        // 4. 空间位置
    ];
    let engine = AnalysisEngine::new(Config::default(), analyzers);

    // ✅ 修复：克隆 ctx_manager 给 Tool 使用
    let score_query = ScoreQueryTool::new(ctx_manager.clone(), Arc::new(engine));

    let tool_set = ToolSet::builder().static_tool(score_query).build();

    let agent_args = TechnicalAgentArgs {
        model,
        tx_out: tx_to_tg,
        tool_set,
    };

    // 7. 启动 Actor
    let (agent_actor, _agent_handle) = Actor::spawn(
        Some("TechnicalAgent".to_string()),
        TechnicalAgent,
        agent_args,
    )
    .await?;

    // 8. 消息路由循环
    let mut rx_cmd = rx_from_tg;
    let actor_for_router = agent_actor.clone();

    tokio::spawn(async move {
        while let Some((cmd, chat_id)) = rx_cmd.recv().await {
            info!("📩 Received command from TG: {}", cmd);
            // 路由指令到 Agent Actor
            let _ = cast!(actor_for_router, TechnicalAgentMessage::Task(cmd, chat_id));
        }
    });

    // 9. 优雅停机等待
    tokio::signal::ctrl_c().await?;
    info!("👋 Shutdown signal received, exiting...");

    Ok(())
}

fn setup_proxy_pool(config: &ProxyConfig) -> CooledProxyPool {
    let cooldown = Duration::from_secs(300);
    CooledProxyPool::new(config.socks_proxy_list.clone(), cooldown)
}
