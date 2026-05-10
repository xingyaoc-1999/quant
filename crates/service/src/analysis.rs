use chrono::Utc;
use common::Symbol;
use quant::audit::{
    build_analysis_details, write_audit_log, AuditEvent, AuditRecord, SignalSummary,
};
use quant::stats::SignalStats;
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
use tracing::{info, warn};

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
    stats: Arc<TokioMutex<SignalStats>>,
}

impl AnalysisService {
    pub fn new(
        engine: Arc<AnalysisEngine>,
        manager: Arc<FeatureContextManager>,
        config: AnalyzerConfig,
        open_positions: Arc<TokioMutex<HashMap<Symbol, Position>>>,
        stats: Arc<TokioMutex<SignalStats>>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            event_tx,
            engine,
            config,
            manager,
            open_positions,
            audit_cache: Arc::new(TokioMutex::new(HashMap::new())),
            stats,
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

        let average_atr = ctx
            .get_role(Role::Filter)
            .or_else(|_| ctx.get_role(Role::Trend))
            .ok()
            .and_then(|r| r.feature_set.indicators.atr_median_20)
            .unwrap_or(atr_ratio * ctx.global.last_price);

        let mut reject_reason: Option<String> = None;

        if let Some(confirmed_dir) = confirmed_direction {
            let risk = risk_mgr.assess(
                Some(confirmed_dir),
                &audit.gravity_wells,
                audit.snapshot.price,
                atr_ratio,
                average_atr,
                vol_p,
                regime,
                is_tsunami,
                taker_ratio,
                ma_dist,
                net_score,
                Some(self.config.risk.max_loss_per_trade),
                funding_rate,
                10.0,
                &mut reject_reason,
            );

            if let Some(assessment) = risk {
                let mut audit = audit;
                audit.risk_assessment = Some(assessment.clone());
                let message = audit.to_markdown_v2(&ctx);
                let _ = self.event_tx.send(AnalysisEvent {
                    symbol,
                    message,
                    assessment: Some(assessment.clone()),
                    timestamp: ctx.global.timestamp,
                });

                let analysis = build_analysis_details(&audit.signal.sub_reports);
                let signal_summary = SignalSummary {
                    direction: format!("{:?}", assessment.direction),
                    entry_price: assessment.entry_levels.first().copied(),
                    stop_loss: assessment.stop_loss_levels.clone(),
                    take_profit: assessment.take_profit_levels.clone(),
                    weighted_rr: assessment.weighted_rr,
                    confidence: assessment.confidence,
                    tags: assessment.audit_tags.clone(),
                };
                let record = AuditRecord {
                    timestamp: Utc::now().timestamp_millis(),
                    event: AuditEvent::Signal,
                    symbol: symbol.as_str().to_string(),
                    signal: Some(signal_summary),
                    market_snapshot: Some(audit.snapshot.clone()),
                    analysis,
                    reject_reason: None,
                };
                write_audit_log(&record).await;

                self.stats.lock().await.add_signal(assessment.weighted_rr);
                self.audit_cache.lock().await.insert(symbol, audit);
            } else {
                let reason = reject_reason.unwrap_or_else(|| "unknown".into());
                warn!("Risk rejected for {}: {:?}", symbol, reason);

                let analysis = build_analysis_details(&audit.signal.sub_reports);
                let record = AuditRecord {
                    timestamp: Utc::now().timestamp_millis(),
                    event: AuditEvent::Reject,
                    symbol: symbol.as_str().to_string(),
                    signal: None,
                    market_snapshot: Some(audit.snapshot.clone()),
                    analysis,
                    reject_reason: Some(reason.clone()),
                };
                write_audit_log(&record).await;
                self.stats.lock().await.add_reject(symbol, reason);
            }
        }

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
        let average_atr = ctx
            .get_role(Role::Filter)
            .or_else(|_| ctx.get_role(Role::Trend))
            .ok()
            .and_then(|r| r.feature_set.indicators.atr_median_20)
            .unwrap_or(atr);
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
        let mut new_alloc = None;

        if let Some(ts) = pos.trailing_stop.as_mut() {
            if let Some(sl) = ts.update(last_price, atr) {
                if (sl - pos.stop_loss).abs() / last_price > self.config.risk.min_stop_dist_pct {
                    new_sl = sl;
                    need_update = true;
                }
            }
        }

        let risk_mgr = RiskManager::new(self.config.clone());
        if let Some((tps, alloc)) = refresh_take_profits(
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
            new_alloc = Some(alloc);
            need_update = true;
        }

        if need_update {
            pos.stop_loss = new_sl;
            pos.take_profit1 = new_tps[0];
            pos.take_profit2 = new_tps[1];

            if let Some(audit) = self.audit_cache.lock().await.get_mut(&symbol) {
                if let Some(ref mut assessment) = audit.risk_assessment {
                    if assessment.stop_loss_levels.len() >= 2 {
                        assessment.stop_loss_levels[0] = new_sl;
                    } else {
                        assessment.stop_loss_levels = vec![new_sl];
                    }
                    assessment.take_profit_levels = new_tps.to_vec();
                    if let Some(alloc) = new_alloc {
                        assessment.allocation = alloc;
                    }
                }

                let message = audit.to_markdown_v2(ctx);
                let _ = self.event_tx.send(AnalysisEvent {
                    symbol,
                    message,
                    assessment: audit.risk_assessment.clone(),
                    timestamp: Utc::now().timestamp_millis(),
                });

                let analysis = build_analysis_details(&audit.signal.sub_reports);
                let signal_summary = audit.risk_assessment.as_ref().map(|r| SignalSummary {
                    direction: format!("{:?}", r.direction),
                    entry_price: r.entry_levels.first().copied(),
                    stop_loss: r.stop_loss_levels.clone(),
                    take_profit: r.take_profit_levels.clone(),
                    weighted_rr: r.weighted_rr,
                    confidence: r.confidence,
                    tags: r.audit_tags.clone(),
                });
                let record = AuditRecord {
                    timestamp: Utc::now().timestamp_millis(),
                    event: AuditEvent::Update,
                    symbol: symbol.as_str().to_string(),
                    signal: signal_summary,
                    market_snapshot: None,
                    analysis,
                    reject_reason: None,
                };
                write_audit_log(&record).await;
                self.stats.lock().await.add_update();
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
