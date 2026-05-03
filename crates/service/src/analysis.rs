use common::Symbol;
use quant::{
    analyzer::{AnalysisEngine, ContextKey},
    config::AnalyzerConfig,
    report::AnalysisAudit,
    risk_manager::{RiskAssessment, RiskManager},
    types::{futures::Role, market::TrendStructure},
    utils::math::dynamic_direction_threshold,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex as TokioMutex};
use tracing::info;

use crate::{integrity::context::FeatureContextManager, types::MarketEvent};

#[derive(Debug, Clone)]
pub struct AnalysisEvent {
    pub symbol: Symbol,
    pub message: String,
    pub assessment: Option<RiskAssessment>,
    pub timestamp: i64,
}

#[derive(Clone)]
pub struct AnalysisService {
    event_tx: broadcast::Sender<AnalysisEvent>,
    engine: Arc<AnalysisEngine>,
    config: AnalyzerConfig,
    manager: Arc<FeatureContextManager>,
    assessment_cache: Arc<TokioMutex<HashMap<Symbol, RiskAssessment>>>,
}

impl AnalysisService {
    pub fn new(
        engine: Arc<AnalysisEngine>,
        manager: Arc<FeatureContextManager>,
        config: AnalyzerConfig,
        assessment_cache: Arc<TokioMutex<HashMap<Symbol, RiskAssessment>>>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            event_tx,
            engine,
            config,
            manager,
            assessment_cache,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AnalysisEvent> {
        self.event_tx.subscribe()
    }

    pub async fn analyze(&self, symbol: Symbol) -> Option<AnalysisAudit> {
        let mut ctx = self.manager.get_market_context(symbol)?;

        let audit = self.engine.run(&mut ctx);

        let net_score = audit.signal.net_score;

        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .copied()
            .unwrap_or(50.0);

        let regime = ctx
            .get_cached::<TrendStructure>(ContextKey::RegimeStructure)
            .copied()
            .unwrap_or(TrendStructure::Range);

        let is_tsunami = ctx
            .get_cached::<bool>(ContextKey::IsMomentumTsunami)
            .copied()
            .unwrap_or(false);

        let taker_ratio = ctx
            .get_role(Role::Entry)
            .ok()
            .and_then(|r| r.taker_flow.taker_buy_ratio)
            .unwrap_or(0.5);

        let ma_dist = ctx
            .get_role(Role::Trend)
            .ok()
            .and_then(|r| r.feature_set.space.ma20_dist_ratio);

        let atr_ratio = ctx
            .get_cached::<f64>(ContextKey::VolAtrRatio)
            .copied()
            .unwrap_or(0.005);

        let funding_rate = ctx.get_cached::<f64>(ContextKey::FundingRate).copied();

        let risk_mgr = RiskManager::new(self.config.clone());

        let is_long_hint = net_score > 0.0;
        let estimated_confidence = risk_mgr.estimate_confidence(
            is_long_hint,
            regime,
            taker_ratio,
            vol_p,
            ma_dist,
            atr_ratio,
            net_score,
            is_tsunami,
            funding_rate,
        );

        let raw_direction = dynamic_direction_threshold(
            net_score,
            vol_p,
            regime,
            estimated_confidence,
            self.config.risk.direction_base_threshold,
        );

        let confirmed_direction = self.manager.filter_direction(symbol, raw_direction);

        self.manager.save_cross_cycle_state(symbol, &ctx);

        if confirmed_direction.is_some() {
            let mut audit = audit;
            audit.attach_risk(&ctx, &self.config);
            let assessment = audit.risk_assessment.clone();

            if let Some(ref assess) = assessment {
                self.assessment_cache
                    .lock()
                    .await
                    .insert(symbol, assess.clone());
            }

            let message = audit.to_markdown_v2(&ctx);

            let _ = self.event_tx.send(AnalysisEvent {
                symbol,
                message,
                assessment,
                timestamp: ctx.global.timestamp,
            });
            Some(audit)
        } else {
            None
        }
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
