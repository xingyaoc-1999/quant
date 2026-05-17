use chrono::Utc;
use common::Symbol;
use quant::audit::{
    build_analysis_details, write_audit_log, AuditEvent, AuditRecord, SignalSummary,
};
use quant::stats::SignalStats;
use quant::types::market::TradeDirection;
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
pub enum AnalysisEvent {
    Signal {
        symbol: Symbol,
        message: String,
        assessment: Option<RiskAssessment>,
        timestamp: i64,
    },
    SignalExpired {
        symbol: Symbol,
        reason: String,
    },
}

pub struct AnalysisService {
    event_tx: broadcast::Sender<AnalysisEvent>,
    engine: Arc<AnalysisEngine>,
    config: AnalyzerConfig,
    manager: Arc<FeatureContextManager>,
    open_positions: Arc<TokioMutex<HashMap<Symbol, Position>>>,
    audit_cache: Arc<TokioMutex<HashMap<Symbol, AnalysisAudit>>>,
    stats: Arc<TokioMutex<SignalStats>>,
    last_confirmed: TokioMutex<HashMap<Symbol, Option<TradeDirection>>>,
    last_btc_direction: TokioMutex<Option<TradeDirection>>,
    close_cooldown: TokioMutex<HashMap<Symbol, usize>>,
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
            last_confirmed: TokioMutex::new(HashMap::new()),
            last_btc_direction: TokioMutex::new(None),
            close_cooldown: TokioMutex::new(HashMap::new()),
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

        if audit.signal.is_rejected {
            let reason = format!("引擎拒绝: {}", audit.signal.reason);
            let _ = self
                .event_tx
                .send(AnalysisEvent::SignalExpired { symbol, reason });
            self.close_cooldown.lock().await.insert(symbol, 2);
            return;
        }

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

        let is_long_hint = audit.signal.raw_adjusted_score > 0.0;
        let mut estimated_confidence = risk_mgr.estimate_confidence(
            is_long_hint,
            regime,
            taker_ratio,
            vol_p,
            ma_dist,
            atr_ratio,
            audit.signal.raw_adjusted_score,
            is_tsunami,
            funding_rate,
        );

        let raw_direction = dynamic_direction_threshold(
            audit.signal.raw_adjusted_score,
            vol_p,
            regime,
            estimated_confidence,
            self.config.risk.direction_base_threshold,
        );

        // ===== 修改：强趋势或海啸时，无需重力井目标 =====
        let has_valid_targets = if is_tsunami
            || matches!(
                regime,
                TrendStructure::StrongBullish | TrendStructure::StrongBearish
            ) {
            true
        } else {
            audit.gravity_wells.iter().any(|w| {
                w.is_active
                    && if is_long_hint {
                        w.level > audit.snapshot.price
                    } else {
                        w.level < audit.snapshot.price
                    }
            })
        };
        // ===== 结束修改 =====

        let likely_stop = is_tsunami
            || (estimated_confidence > 1.2 && has_valid_targets)
            || (matches!(
                regime,
                TrendStructure::StrongBullish | TrendStructure::StrongBearish
            ) && vol_p < 70.0)
            || (vol_p > 70.0);

        let required_confirm = if likely_stop {
            self.manager.signal_config.confirm_bars_stop
        } else {
            self.manager.signal_config.confirm_bars
        };

        let confirmed_direction =
            self.manager
                .filter_direction(symbol, raw_direction, required_confirm);

        self.manager.save_cross_cycle_state(symbol, &ctx);

        // --- BTC 方向过滤 ---
        if self.config.risk.enable_btc_correlation_filter && symbol != Symbol::BTCUSDT {
            if let Some(btc_dir) = self.manager.get_btc_direction() {
                if let Some(my_dir) = confirmed_direction {
                    if my_dir != btc_dir {
                        let reason = format!(
                            "counter_btc: signal={:?}, btc_direction={:?}",
                            my_dir, btc_dir
                        );
                        warn!("[BTC FILTER] {} {}", symbol, reason);
                        let _ = self.event_tx.send(AnalysisEvent::SignalExpired {
                            symbol,
                            reason: reason.clone(),
                        });
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
                        self.close_cooldown.lock().await.insert(symbol, 2);
                        return;
                    }
                }
            }
        }

        // --- 逆大周期过滤（适度宽松：仅强趋势拦截）---
        let higher_tf_trend = ctx
            .get_role(Role::Filter)
            .or_else(|_| ctx.get_role(Role::Trend))
            .ok()
            .and_then(|r| r.feature_set.structure.trend_structure);

        if let (Some(confirmed_dir), Some(htf_trend)) = (confirmed_direction, higher_tf_trend) {
            let signal_long = confirmed_dir == TradeDirection::Long;
            let htf_strong_bull = matches!(htf_trend, TrendStructure::StrongBullish);
            let htf_strong_bear = matches!(htf_trend, TrendStructure::StrongBearish);
            if (signal_long && htf_strong_bear) || (!signal_long && htf_strong_bull) {
                let reason = format!(
                    "counter_htf: signal={:?}, htf_trend={:?} (strong only)",
                    confirmed_dir, htf_trend
                );
                warn!("[HTF REJECT] {} {}", symbol, reason);
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
                return;
            }
        }

        // --- 信号失效检测 ---
        {
            let mut last_dir_map = self.last_confirmed.lock().await;
            let old_dir = last_dir_map.get(&symbol).copied().flatten();
            let new_dir = confirmed_direction;
            if let Some(old) = old_dir {
                let expired = match new_dir {
                    Some(new) => new != old,
                    None => true,
                };
                if expired {
                    let reason = match new_dir {
                        Some(d) => format!("方向反转 → {:?}", d),
                        None => "方向信号丢失".into(),
                    };
                    let _ = self
                        .event_tx
                        .send(AnalysisEvent::SignalExpired { symbol, reason });
                    self.close_cooldown.lock().await.insert(symbol, 2);
                }
            }
            last_dir_map.insert(symbol, new_dir);
        }

        // --- BTC 方向变化监听 ---
        if symbol == Symbol::BTCUSDT {
            let mut last_btc = self.last_btc_direction.lock().await;
            let old_btc = *last_btc;
            let new_btc = confirmed_direction;
            let btc_changed = match (old_btc, new_btc) {
                (None, Some(_)) => true,
                (Some(old), Some(new)) => old != new,
                _ => false,
            };
            if btc_changed {
                if let Some(new_dir) = new_btc {
                    let last_confirmed_map = self.last_confirmed.lock().await;
                    for (&sym, &dir) in last_confirmed_map.iter() {
                        if sym == Symbol::BTCUSDT {
                            continue;
                        }
                        if let Some(sym_dir) = dir {
                            if sym_dir != new_dir {
                                let reason = format!(
                                    "BTC方向变化后逆向失效: signal={:?}, btc_direction={:?}",
                                    sym_dir, new_dir
                                );
                                let _ = self.event_tx.send(AnalysisEvent::SignalExpired {
                                    symbol: sym,
                                    reason,
                                });
                                self.close_cooldown.lock().await.insert(sym, 2);
                            }
                        }
                    }
                }
            }
            *last_btc = new_btc;
        }

        // ===== RSI + 缩量过滤 =====
        if let Some(dir) = confirmed_direction {
            let rsi = ctx
                .get_role(Role::Trend)
                .ok()
                .and_then(|r| r.feature_set.indicators.rsi_14)
                .or_else(|| {
                    ctx.get_role(Role::Entry)
                        .ok()
                        .and_then(|r| r.feature_set.indicators.rsi_14)
                })
                .unwrap_or(50.0);
            let rvol = ctx
                .get_cached::<f64>(ContextKey::LastRVol)
                .copied()
                .unwrap_or(1.0);

            let is_short = dir == TradeDirection::Short;
            let is_long = dir == TradeDirection::Long;

            let rsi_penalty = (is_short && rsi < 40.0) || (is_long && rsi > 60.0);
            let low_volume = rvol < 1.0;

            if rsi_penalty {
                if low_volume {
                    let reason = format!(
                        "RSI={:.1} 且缩量(rvol={:.2})，{}空间有限",
                        rsi,
                        rvol,
                        if is_short { "做空" } else { "做多" }
                    );
                    warn!("[RSI+VOL REJECT] {} {}", symbol, reason);
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
                    return;
                } else {
                    estimated_confidence *= 0.7;
                    info!(
                        "[RSI+VOL] 降低置信度: rsi={:.1} rvol={:.2} 新置信度={:.2}",
                        rsi, rvol, estimated_confidence
                    );
                }
            }
        }

        let average_atr = ctx
            .get_role(Role::Filter)
            .or_else(|_| ctx.get_role(Role::Trend))
            .ok()
            .and_then(|r| r.feature_set.indicators.atr_median_20)
            .unwrap_or(atr_ratio * ctx.global.last_price);

        let mut reject_reason: Option<String> = None;

        if let Some(confirmed_dir) = confirmed_direction {
            // 冷却检查
            let mut cooldown_map = self.close_cooldown.lock().await;
            if let Some(remaining) = cooldown_map.get_mut(&symbol) {
                if *remaining > 0 {
                    *remaining -= 1;
                    let reason = format!("cooldown_after_close: remaining={}", *remaining);
                    warn!("[COOLDOWN] {} {}", symbol, reason);
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
                    return;
                }
            }

            // 区间边界计算
            let price = audit.snapshot.price;
            let range_high = ctx
                .get_role(Role::Trend)
                .ok()
                .and_then(|r| {
                    r.feature_set
                        .recent_highs
                        .iter()
                        .cloned()
                        .max_by(|a, b| a.partial_cmp(b).unwrap())
                })
                .unwrap_or(price);
            let range_low = ctx
                .get_role(Role::Trend)
                .ok()
                .and_then(|r| {
                    r.feature_set
                        .recent_lows
                        .iter()
                        .cloned()
                        .min_by(|a, b| a.partial_cmp(b).unwrap())
                })
                .unwrap_or(price);
            let range_width = range_high - range_low;

            if range_width > 0.0 {
                let position_in_range = (price - range_low) / range_width;
                let is_mid_range = position_in_range > 0.35 && position_in_range < 0.65;
                if regime == TrendStructure::Range && is_mid_range {
                    let reason = format!(
                        "mid_range_reject: price={:.2} range=[{:.2}, {:.2}] pos={:.2}",
                        price, range_low, range_high, position_in_range
                    );
                    warn!("[MID RANGE] {} {}", symbol, reason);
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
                    return;
                }

                let at_boundary = position_in_range > 0.8 || position_in_range < 0.2;
                if at_boundary && regime == TrendStructure::Range {
                    let rvol = ctx
                        .get_cached::<f64>(ContextKey::LastRVol)
                        .copied()
                        .unwrap_or(1.0);
                    let eff = ctx
                        .get_cached::<f64>(ContextKey::LastEfficiency)
                        .copied()
                        .unwrap_or(0.5);
                    if rvol < 1.2 && eff < 0.4 {
                        let reason = format!(
                            "breakout_no_volume: rvol={:.2} eff={:.2} at_boundary_pos={:.2}",
                            rvol, eff, position_in_range
                        );
                        warn!("[BREAKOUT REJECT] {} {}", symbol, reason);
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
                        return;
                    }
                }
            }

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
                audit.signal.raw_adjusted_score,
                Some(self.config.risk.max_loss_per_trade),
                funding_rate,
                10.0,
                &mut reject_reason,
            );

            if let Some(assessment) = risk {
                let min_conf = self.config.risk.min_confidence_mult;
                if assessment.confidence_mult < min_conf {
                    let reason = format!(
                        "low_confidence: conf_mult={:.2} < {:.2}",
                        assessment.confidence_mult, min_conf
                    );
                    warn!("[LOW CONFIDENCE] {} {}", symbol, reason);
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
                    return;
                }

                let mut audit = audit;
                audit.risk_assessment = Some(assessment.clone());
                let message = audit.to_markdown_v2(&ctx);
                let _ = self.event_tx.send(AnalysisEvent::Signal {
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
                self.config.risk.initial_protection_bars,
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
                let _ = self.event_tx.send(AnalysisEvent::Signal {
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
