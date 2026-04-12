use common::Symbol;
use quant::{analyzer::AnalysisEngine, report::AnalysisAudit, risk_manager::TradeDirection};
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
}

impl AnalysisService {
    pub fn new(engine: Arc<AnalysisEngine>) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self { event_tx, engine }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AnalysisEvent> {
        self.event_tx.subscribe()
    }

    pub async fn analyze(
        &self,
        manager: &FeatureContextManager,
        symbol: Symbol,
    ) -> Option<AnalysisAudit> {
        let mut ctx = manager.get_market_context(symbol)?;
        let mut audit = self.engine.run(&mut ctx);

        // --- 修改点 1: 同步 TradeDirection 的变更 ---
        // 现在的逻辑：根据分数产生 Some(方向) 或 None
        let direction = if audit.signal.net_score > 15.0 {
            Some(TradeDirection::Long)
        } else if audit.signal.net_score < -15.0 {
            Some(TradeDirection::Short)
        } else {
            None
        };

        // --- 修改点 2: 匹配新的 attach_risk 签名 ---
        // 我们之前将 attach_risk 改为了接收 Option<TradeDirection>
        audit.attach_risk(&ctx, direction);

        info!(
            symbol = %symbol,
            score = audit.signal.net_score,
            direction = ?direction, // 使用 ? 打印 Option (显示 Some(Long) 或 None)
            has_risk = audit.risk_assessment.is_some(),
        );

        let _ = self.event_tx.send(AnalysisEvent {
            audit: audit.clone(),
        });

        Some(audit)
    }

    pub fn spawn_worker(
        self: Arc<Self>,
        mut event_rx: mpsc::Receiver<MarketEvent>,
        manager: Arc<FeatureContextManager>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!("Analysis worker started");

            while let Some(event) = event_rx.recv().await {
                match event {
                    MarketEvent::KlineClosed { symbol } => {
                        self.analyze(&manager, symbol).await;
                    }
                }
            }

            info!("Analysis worker stopped");
        })
    }
}
