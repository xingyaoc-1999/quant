use chrono::Utc;
use common::Symbol;
use quant::{
    analyzer::{AnalysisEngine, ContextKey},
    config::AnalyzerConfig,
    position::Position,
    report::AnalysisAudit,
    risk_manager::{RiskAssessment, RiskManager},
    trailing::{refresh_take_profits, TrailingStop},
    types::{futures::Role, gravity::PriceGravityWell, market::TrendStructure},
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

pub struct AnalysisService {
    event_tx: broadcast::Sender<AnalysisEvent>,
    engine: Arc<AnalysisEngine>,
    config: AnalyzerConfig,
    manager: Arc<FeatureContextManager>,
    open_positions: Arc<TokioMutex<HashMap<Symbol, Position>>>,
    audit_cache: Arc<TokioMutex<HashMap<Symbol, AnalysisAudit>>>,
}

impl AnalysisService {
    pub fn new(
        engine: Arc<AnalysisEngine>,
        manager: Arc<FeatureContextManager>,
        config: AnalyzerConfig,
        open_positions: Arc<TokioMutex<HashMap<Symbol, Position>>>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            event_tx,
            engine,
            config,
            manager,
            open_positions,
            audit_cache: Arc::new(TokioMutex::new(HashMap::new())),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AnalysisEvent> {
        self.event_tx.subscribe()
    }

    pub async fn analyze(&self, symbol: Symbol) {
        let mut ctx = match self.manager.get_market_context(symbol) {
            Some(c) => c,
            None => return,
        };

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

        info!(
            "[ANALYZE] {} | net_score={:.2} | raw_direction={:?}",
            symbol.as_str(),
            net_score,
            raw_direction,
        );

        let confirmed_direction = self.manager.filter_direction(symbol, raw_direction);

        self.manager.save_cross_cycle_state(symbol, &ctx);

        // 新信号推送
        if let Some(_dir) = confirmed_direction {
            let mut audit = audit; // 捕获所有权
            if audit.attach_risk(&ctx, &self.config).is_some() {
                let message = audit.to_markdown_v2(&ctx);
                let assessment = audit.risk_assessment.clone();
                let _ = self.event_tx.send(AnalysisEvent {
                    symbol,
                    message,
                    assessment,
                    timestamp: ctx.global.timestamp,
                });
                // 缓存完整 Audit
                self.audit_cache.lock().await.insert(symbol, audit);
            }
        }

        // 更新已有持仓的动态止盈止损（无论是否有新信号）
        self.update_open_positions(symbol, &ctx).await;
    }

    async fn update_open_positions(&self, symbol: Symbol, ctx: &quant::analyzer::MarketContext) {
        let mut positions = self.open_positions.lock().await;
        let pos = match positions.get_mut(&symbol) {
            Some(p) => p,
            None => return,
        };

        let last_price = ctx.global.last_price;
        let atr_ratio = ctx
            .get_cached::<f64>(ContextKey::VolAtrRatio)
            .copied()
            .unwrap_or(0.005);
        let atr = atr_ratio * last_price;
        let wells = ctx
            .get_cached::<Vec<PriceGravityWell>>(ContextKey::SpaceGravityWells)
            .cloned()
            .unwrap_or_default();
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .copied()
            .unwrap_or(50.0);
        let is_tsunami = ctx
            .get_cached::<bool>(ContextKey::IsMomentumTsunami)
            .copied()
            .unwrap_or(false);
        let average_atr = atr; // 简化，未来可替换为历史ATR中位值

        // 初始化移动止损（首次）
        if pos.trailing_stop.is_none() {
            pos.trailing_stop = Some(TrailingStop::new(
                pos.direction,
                pos.entry_price,
                pos.stop_loss,
                self.config.risk.trailing_atr_mult,
            ));
        }

        let mut need_update = false;
        let mut new_sl = pos.stop_loss;
        let mut new_tps = [pos.take_profit1, pos.take_profit2];

        if let Some(ts) = pos.trailing_stop.as_mut() {
            if let Some(sl) = ts.update(last_price, atr) {
                if (sl - pos.stop_loss).abs() / last_price > self.config.risk.min_stop_dist_pct {
                    new_sl = sl;
                    need_update = true;
                }
            }
        }

        let risk_mgr = RiskManager::new(self.config.clone());
        if let Some(tps) = refresh_take_profits(
            &risk_mgr,
            &wells,
            last_price,
            atr,
            average_atr,
            pos.is_long(),
            is_tsunami,
            vol_p,
            &new_tps,
        ) {
            new_tps = tps;
            need_update = true;
        }

        if need_update {
            pos.stop_loss = new_sl;
            pos.take_profit1 = new_tps[0];
            pos.take_profit2 = new_tps[1];

            if let Some(audit) = self.audit_cache.lock().await.get_mut(&symbol) {
                if let Some(ref mut assessment) = audit.risk_assessment {
                    assessment.stop_loss_levels = vec![new_sl, new_tps[1]];
                    assessment.take_profit_levels = new_tps.to_vec();
                }
                let message = audit.to_markdown_v2(ctx);
                let _ = self.event_tx.send(AnalysisEvent {
                    symbol,
                    message,
                    assessment: audit.risk_assessment.clone(),
                    timestamp: Utc::now().timestamp_millis(),
                });
            }
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
