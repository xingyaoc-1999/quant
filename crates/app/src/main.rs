use std::{collections::HashMap, sync::Arc, time::Duration};

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
use quant::risk_manager::RiskAssessment;
use quant::{analyzer::ConfigurableAnalyzer, config::SignalStabilityConfig};
use quant::{
    analyzer::{
        AnalysisEngine, AnalyzerWrapper, Config, FakeoutDetector, GravityAnalyzer,
        MarketRegimeAnalyzer, ResonanceAnalyzer, VolatilityEnvironmentAnalyzer,
        VolumeStructureAnalyzer,
    },
    config::AnalyzerConfig,
};
use quant::{position::Position, stats::SignalStats};
use ractor::{cast, Actor};
use rig::tool::ToolSet;
use service::{
    analysis::{AnalysisEvent, AnalysisService},
    integrity::{context::FeatureContextManager, DataIntegrityManager},
};
use storage::postgres::Storage;
use tokio::sync::{mpsc, Mutex as TokioMutex};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = Appconfig::global();
    let symbols = Symbol::all();

    let proxy_pool = Arc::new(create_proxy_pool(&config.proxy));
    let storage = Arc::new(init_storage(&config.database).await?);
    let archive = Arc::new(ArchiveProvider::new(proxy_pool.clone()));

    // 唯一 stats 实例
    let stats = Arc::new(TokioMutex::new(SignalStats::default()));

    let ctx_manager = Arc::new(FeatureContextManager::new(
        &symbols,
        SignalStabilityConfig::default(),
        stats.clone(),
    ));
    let analyzer_config = AnalyzerConfig::default();

    let analyzers: Vec<Box<dyn AnalyzerWrapper>> = vec![
        Box::new(VolatilityEnvironmentAnalyzer::with_config(
            analyzer_config.clone(),
        )),
        Box::new(VolumeStructureAnalyzer::with_config(
            analyzer_config.clone(),
        )),
        Box::new(GravityAnalyzer::with_config(analyzer_config.clone())),
        Box::new(MarketRegimeAnalyzer::with_config(analyzer_config.clone())),
        Box::new(FakeoutDetector::with_config(analyzer_config.clone())),
        Box::new(ResonanceAnalyzer::with_config(analyzer_config.clone())),
    ];

    let engine = Arc::new(AnalysisEngine::new(Config::default(), analyzers));
    let config_arc = Arc::new(analyzer_config.clone());

    let open_positions = Arc::new(TokioMutex::new(HashMap::<Symbol, Position>::new()));

    let analysis_service = Arc::new(AnalysisService::new(
        engine.clone(),
        ctx_manager.clone(),
        analyzer_config.clone(),
        open_positions.clone(),
        stats.clone(),
    ));

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

    let (tg_tx, tg_rx) = mpsc::channel::<AnalysisEvent>(256);

    let execute_order: Arc<
        dyn Fn(
                &RiskAssessment,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>>
            + Send
            + Sync,
    > = Arc::new(|assessment: &RiskAssessment| {
        let assessment = assessment.clone();
        Box::pin(async move {
            info!(
                "Executing order: {:?} entry={} size={:.2}%",
                assessment.direction,
                assessment.entry_levels.first().unwrap_or(&0.0),
                assessment.position_size_pct * 100.0
            );
            Ok("mock_order_id_123".to_string())
        })
    });

    let bot = BotApp::new(
        config.telegram.token.clone(),
        proxy_pool.clone(),
        storage.clone(),
        engine.clone(),
        ctx_manager.clone(),
        config_arc.clone(),
        execute_order,
    )
    .await?;

    tokio::spawn(async move {
        if let Err(e) = bot.run(tg_rx).await {
            error!("Telegram bot error: {:?}", e);
        }
    });
    info!("Telegram bot started");

    let mut analysis_rx = analysis_service.subscribe();
    let tg_sender = tg_tx.clone();

    tokio::spawn(async move {
        info!("Analysis notification worker started");
        while let Ok(event) = analysis_rx.recv().await {
            let _ = tg_sender.send(event).await;
        }
        info!("Analysis notification worker stopped");
    });

    // AI Agent 初始化（暂时注释）
    // let model = Model::openai(...)?;
    // ...

    info!("System ready, waiting for Ctrl+C...");
    tokio::signal::ctrl_c().await?;
    info!("Shutting down...");

    Ok(())
}

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
