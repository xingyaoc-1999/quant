use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};

use api_client::http::binance::ArchiveProvider;

use common::{
    config::{self, Appconfig, ProxyConfig},
    utils::CooledProxyPool,
    Symbol,
};

use notify::telegram::BotApp;

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

    // --- 4. 启动异步任务 ---
    info!("🚀 Starting Telegram Bot...");
    tokio::spawn(async move {
        if let Err(e) = tg_app
            .run(tx_in, rx_out, ctx_manager.clone(), storage.clone())
            .await
        {
            error!("❌ Telegram Bot Runtime Error: {:?}", e);
        }
    });

    info!("🚀 Starting Integrity Manager...");
    integrity_manager.start();

    let mut rx_cmd = rx_from_tg;
    tokio::spawn(async move {
        while let Some((cmd, chat_id)) = rx_cmd.recv().await {
            info!("📩 Received command from TG: {}", cmd);
            // 这里处理你的 CONFIG:SET_ROLE:BTC/USDT:MarketMaker 逻辑
            // 处理完后可以给 tx_to_tg 发个确认
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
