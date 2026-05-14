use binance_sdk::config::ConfigurationRestApi;
use binance_sdk::derivatives_trading_usds_futures::rest_api::RestApi;
use common::Symbol;
use quant::position::Position;
use service::analysis::AnalysisEvent;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast::Receiver;
use tracing::error;

pub struct BinanceExecutor {
    client: RestApi,
    positions: Arc<Mutex<HashMap<Symbol, Position>>>,
    event_rx: Receiver<AnalysisEvent>,
}

impl BinanceExecutor {
    pub fn new(
        api_key: String,
        secret_key: String,
        positions: Arc<Mutex<HashMap<Symbol, Position>>>,
        event_rx: Receiver<AnalysisEvent>,
    ) -> Self {
        let rest_conf = ConfigurationRestApi::builder()
            .api_key(api_key)
            .api_secret(secret_key)
            .base_path("https://testnet.binancefuture.com")
            .build()
            .expect("Failed to build REST configuration");

        let client = RestApi::new(rest_conf);

        Self {
            client,
            positions,
            event_rx,
        }
    }

    pub async fn run(&mut self) {
        while let Ok(event) = self.event_rx.recv().await {
            match event {
                AnalysisEvent::Signal {
                    symbol, assessment, ..
                } => {
                    if let Some(_assess) = assessment {
                        // TODO: 执行下单逻辑
                        let _ = symbol;
                    }
                }
                AnalysisEvent::SignalExpired { symbol, .. } => {
                    // TODO: 撤销订单逻辑
                    let _ = symbol;
                }
            }
        }
    }
}
