use std::{sync::Arc, time::Duration};

use agent::agent::{
    technical::{TechnicalAgent, TechnicalAgentArgs, TechnicalAgentMessage},
    Model,
};
use anyhow::{Context, Result};
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
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cfg = Appconfig::global();

    let storage = Storage::new(&cfg.database).context("Failed to connect to database")?;
    storage
        .initialize_all()
        .await
        .context("Failed to initialize database schema")?;
    let storage = Arc::new(storage);

    let symbols = Symbol::all();
    let proxy_pool = Arc::new(setup_proxy_pool(&cfg.proxy));
    let ctx_manager = Arc::new(FeatureContextManager::new(&symbols));

    let archive_provider = Arc::new(ArchiveProvider::new(proxy_pool.clone()));
    let integrity_manager = Arc::new(DataIntegrityManager::new(
        symbols.clone(),
        ctx_manager.clone(),
        proxy_pool.clone(),
        storage.clone(),
        archive_provider.clone(),
    ));

    let (tx_to_tg, rx_out) = tokio::sync::mpsc::channel(100);
    let (tx_in, rx_from_tg) = tokio::sync::mpsc::channel(100);

    let tg_app = BotApp::new(cfg.telegram.token.clone(), proxy_pool.clone()).await?;

    info!("🚀 Starting Telegram Bot...");
    tokio::spawn(async move {
        if let Err(e) = tg_app
            .run(
                tx_in,
                rx_out,
                ctx_manager.clone(),
                archive_provider.clone(),
                storage.clone(),
            )
            .await
        {
            error!("Telegram Bot Runtime Error: {:?}", e);
        }
    });

    info!("🚀 Starting Integrity Manager...");
    integrity_manager.start();
    let model = Model::openai(
        "sk-KL85Y5XsOM7kcm7qzSSFUUJ5iqEAcU3kiV4rAGsWPC6rFlp7",
        "https://aiberm.com/v1",
        "openai/gpt-5.4",
    )?;
    let tool_set = ToolSet::builder().static_tool(score_query).build();
    let agent_args = TechnicalAgentArgs {
        model,
        tx_out: tx_to_tg,
        tool_set,
    };

    let (agent_actor, _agent_handle) = Actor::spawn(
        Some("TechnicalAgent".to_string()),
        TechnicalAgent,
        agent_args,
    )
    .await?;

    let mut rx_cmd = rx_from_tg;
    let actor_for_router = agent_actor.clone();

    tokio::spawn(async move {
        while let Some((cmd, chat_id)) = rx_cmd.recv().await {
            info!("📩 Received command from TG: {}", cmd);

            let _ = cast!(actor_for_router, TechnicalAgentMessage::Task(cmd, chat_id));
        }
    });

    tokio::signal::ctrl_c().await?;
    info!("👋 Shutdown signal received, exiting...");

    Ok(())
}

fn setup_proxy_pool(config: &ProxyConfig) -> CooledProxyPool {
    let cooldown = Duration::from_secs(300);

    CooledProxyPool::new(config.socks_proxy_list.clone(), cooldown)
}
