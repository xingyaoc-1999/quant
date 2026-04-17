use common::Symbol;
use quant::{analyzer::AnalysisEngine, config::AnalyzerConfig, report::AnalysisAudit};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tracing::info;

use crate::{integrity::context::FeatureContextManager, types::MarketEvent};

#[derive(Debug, Clone)]
pub struct AnalysisEvent {
    pub audit: AnalysisAudit,
}

#[derive(Clone)]
pub struct AnalysisService {
    event_tx: broadcast::Sender<AnalysisEvent>,
    engine: Arc<AnalysisEngine>,
    config: AnalyzerConfig,
    manager: Arc<FeatureContextManager>,
}

impl AnalysisService {
    pub fn new(
        engine: Arc<AnalysisEngine>,
        manager: Arc<FeatureContextManager>,
        config: AnalyzerConfig,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            event_tx,
            engine,
            config,
            manager,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AnalysisEvent> {
        self.event_tx.subscribe()
    }

    pub async fn analyze(&self, symbol: Symbol) -> Option<AnalysisAudit> {
        let mut ctx = self.manager.get_market_context(symbol)?;
        let mut audit = self.engine.run(&mut ctx);
        self.manager.save_cross_cycle_state(symbol, &ctx);

        audit.attach_risk(&ctx, &self.config);

        let _ = self.event_tx.send(AnalysisEvent {
            audit: audit.clone(),
        });

        Some(audit)
    }

    pub fn spawn_worker(
        self: Arc<Self>,
        mut event_rx: mpsc::Receiver<MarketEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!("Analysis worker started");

            while let Some(event) = event_rx.recv().await {
                match event {
                    MarketEvent::KlineClosed { symbol } => {
                        self.analyze(symbol).await;
                    }
                }
            }
            info!("Analysis worker stopped");
        })
    }
}
