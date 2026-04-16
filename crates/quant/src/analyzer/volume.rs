use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ConfigurableAnalyzer, ContextKey,
    MarketContext, Role,
};
use crate::config::AnalyzerConfig;
use crate::types::futures::OIPositionState;
use crate::types::gravity::{PriceGravityWell, WellSide};
use crate::types::market::{MarketStressLevel, TrendStructure, VolumeState};
use crate::types::session::TradingSession;
use crate::utils::effiency::{calculate_efficiency, consistency_penalty};
use crate::utils::volatility::{compute_vol_factor, volatility_adaptation};

#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct VolumeExtra {
    pub efficiency: f64,
    pub rvol: f64,
    pub consistency: f64,
    pub triggered_well: Option<PriceGravityWell>,
    pub oi_delta_pct: Option<f64>,
    pub taker_ratio: Option<f64>,
    pub is_tsunami: bool,
    pub session: String,
    pub vol_adapt: f64,
    pub session_adj: f64,
}

pub struct VolumeStructureAnalyzer {
    config: AnalyzerConfig,
}

impl ConfigurableAnalyzer for VolumeStructureAnalyzer {
    fn with_config(config: AnalyzerConfig) -> Self {
        Self { config }
    }
    fn config(&self) -> &AnalyzerConfig {
        &self.config
    }
}

impl Analyzer for VolumeStructureAnalyzer {
    type Extra = VolumeExtra;

    fn name(&self) -> &'static str {
        "volume_structure_v7"
    }
    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::VolumeStructure
    }

    fn dependencies(&self) -> Vec<ContextKey> {
        vec![
            ContextKey::GravitySigma,
            ContextKey::SpaceGravityWells,
            ContextKey::VolPercentile,
            ContextKey::VolAtrRatio,
            ContextKey::IsMomentumTsunami,
            ContextKey::OiPositionState,
            ContextKey::RegimeStructure,
            ContextKey::MarketStressLevel,
        ]
    }

    fn analyze(
        &self,
        ctx: &mut MarketContext,
    ) -> Result<AnalysisResult<Self::Extra>, AnalysisError> {
        let last_price = ctx.global.last_price;
        let timestamp = ctx.global.timestamp;

        let role_data = ctx
            .get_role(Role::Filter)
            .or_else(|_| ctx.get_role(Role::Trend))?;
        let p_action = &role_data.feature_set.price_action;
        let avg_volume = role_data
            .feature_set
            .indicators
            .volume_ma_20
            .unwrap_or(p_action.volume)
            .max(f64::EPSILON);
        let oi_delta = role_data.oi_data.as_ref().map(|oi| oi.delta_ratio());
        let taker_ratio = role_data.taker_flow.taker_buy_ratio;
        let volume_state = role_data.feature_set.structure.volume_state;

        let sigma = ctx
            .get_cached::<f64>(ContextKey::GravitySigma)
            .copied()
            .unwrap_or(0.005);
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .copied()
            .unwrap_or(50.0);
        let wells = ctx
            .get_cached::<Vec<PriceGravityWell>>(ContextKey::SpaceGravityWells)
            .cloned()
            .unwrap_or_default();
        let atr = ctx
            .get_cached::<f64>(ContextKey::VolAtrRatio)
            .copied()
            .unwrap_or(0.005)
            * last_price;
        let is_tsunami = ctx
            .get_cached::<bool>(ContextKey::IsMomentumTsunami)
            .copied()
            .unwrap_or(false);
        let oi_state = ctx
            .get_cached::<OIPositionState>(ContextKey::OiPositionState)
            .copied();
        let stress_level = ctx
            .get_cached::<MarketStressLevel>(ContextKey::MarketStressLevel)
            .copied()
            .unwrap_or_default();

        let session = TradingSession::from_timestamp(timestamp);
        let session_adj = session.factor(&self.config.session);
        let vol_adapt = volatility_adaptation(vol_p);
        let stress_adj = self.stress_adjustment(stress_level);

        let vol_factor = compute_vol_factor(vol_p);
        let cfg = &self.config.volume;

        let thresholds = DynamicThresholds::new(cfg, vol_factor);
        let (efficiency, rvol) = calculate_efficiency(p_action, avg_volume, atr, &cfg.efficiency);
        let consistency = consistency_penalty(rvol, volume_state);

        let mut score = 0.0;
        let mut m_vol = 1.0;
        let mut res = AnalysisResult::new(self.kind());

        let is_up = p_action.close > p_action.open;
        let target_side = if is_up {
            WellSide::Resistance
        } else {
            WellSide::Support
        };

        let active_well = wells
            .iter()
            .filter(|w| w.is_active && w.side == target_side)
            .min_by(|a, b| {
                let dist_a = (a.level - last_price).abs() / last_price / (a.strength + 0.1);
                let dist_b = (b.level - last_price).abs() / last_price / (b.strength + 0.1);
                dist_a.total_cmp(&dist_b)
            });

        if let Some(well) = active_well {
            let dist_pct = (well.level - last_price).abs() / last_price;
            if dist_pct < sigma * 1.5 {
                let signal = self.evaluate_well_signal(
                    well,
                    is_up,
                    rvol,
                    efficiency,
                    &thresholds,
                    oi_state,
                    is_tsunami,
                    taker_ratio,
                    oi_delta,
                    cfg,
                );
                score = signal.score * consistency * vol_adapt * session_adj * stress_adj;
                m_vol = signal.multiplier;
                res = res.because(signal.reason);
            }
        }

        if score == 0.0 {
            let (trend_score, trend_mult, reason) = self.evaluate_trend_extension(
                ctx,
                p_action,
                efficiency,
                rvol,
                cfg,
                vol_adapt,
                session_adj,
                stress_adj,
            );
            score = trend_score * consistency;
            m_vol = trend_mult;
            res = res.because(reason);
        }

        ctx.set_cached(ContextKey::LastEfficiency, efficiency);
        ctx.set_cached(ContextKey::LastRVol, rvol);
        ctx.set_cached(
            ContextKey::VolumeState,
            if rvol > thresholds.rvol_break {
                VolumeState::Expand
            } else if rvol < 0.8 {
                VolumeState::Shrink
            } else {
                VolumeState::Normal
            },
        );

        let extra = VolumeExtra {
            efficiency,
            rvol,
            consistency,
            triggered_well: active_well.cloned(),
            oi_delta_pct: oi_delta.map(|d| d * 100.0),
            taker_ratio,
            is_tsunami,
            session: format!("{:?}", session),
            vol_adapt,
            session_adj,
        };

        Ok(res.with_score(score).with_mult(m_vol).with_extra(extra))
    }
}

// ========== 辅助结构与逻辑 ==========
struct DynamicThresholds {
    rvol_break: f64,
    rvol_extreme: f64,
    eff_high: f64,
    eff_low: f64,
    magnet_rvol_break: f64,
    magnet_eff_high: f64,
    magnet_rvol_shrink: f64,
    magnet_eff_low: f64,
}

impl DynamicThresholds {
    fn new(cfg: &crate::config::VolumeConfig, vol_factor: f64) -> Self {
        let rvol_break = cfg.rvol_break_base() / vol_factor;
        let rvol_extreme = cfg.rvol_extreme_base() / vol_factor;
        let eff_high = cfg.eff_high_base() / vol_factor;
        let eff_low = cfg.eff_low_base() * vol_factor;
        Self {
            rvol_break,
            rvol_extreme,
            eff_high,
            eff_low,
            magnet_rvol_break: rvol_break * cfg.magnet_threshold_ratio(), // 配置化系数
            magnet_eff_high: eff_high * cfg.magnet_threshold_ratio(),
            magnet_rvol_shrink: cfg.magnet_shrink_base() / vol_factor,
            magnet_eff_low: eff_low * cfg.magnet_threshold_ratio(),
        }
    }
}

struct WellSignal {
    score: f64,
    multiplier: f64,
    reason: String,
}

impl VolumeStructureAnalyzer {
    fn stress_adjustment(&self, level: MarketStressLevel) -> f64 {
        match level {
            MarketStressLevel::Dead => 0.5,
            MarketStressLevel::MeatGrinder => 0.7,
            MarketStressLevel::Acceleration => 0.9,
            _ => 1.0,
        }
    }

    fn evaluate_well_signal(
        &self,
        well: &PriceGravityWell,
        is_up: bool,
        rvol: f64,
        efficiency: f64,
        thresh: &DynamicThresholds,
        oi_state: Option<OIPositionState>,
        is_tsunami: bool,
        taker_ratio: Option<f64>,
        oi_delta: Option<f64>,
        cfg: &crate::config::VolumeConfig,
    ) -> WellSignal {
        match well.side {
            WellSide::Resistance => {
                if rvol > thresh.rvol_break && efficiency > thresh.eff_high {
                    WellSignal {
                        score: 45.0,
                        multiplier: 1.8,
                        reason: format!("吸收突破: 价格高效贯穿阻力 {}", well.source_string()),
                    }
                } else if rvol > thresh.rvol_extreme && efficiency < thresh.eff_low {
                    if matches!(oi_state, Some(OIPositionState::LongBuildUp)) && is_tsunami {
                        WellSignal {
                            score: 25.0,
                            multiplier: 1.3,
                            reason: "强力吸收: 阻力位多头持续接盘".into(),
                        }
                    } else {
                        WellSignal {
                            score: -80.0,
                            multiplier: 1.8,
                            reason: "派发陷阱: 阻力位放量滞涨".into(),
                        }
                    }
                } else if taker_ratio.map_or(false, |tr| tr > cfg.absorption_taker_buy_min())
                    && oi_delta.map_or(false, |d| d > cfg.absorption_oi_delta_min())
                    && efficiency < thresh.eff_low
                {
                    WellSignal {
                        score: -55.0,
                        multiplier: 1.6,
                        reason: "阻力被动吸收: 强力买盘被挂单截杀".into(),
                    }
                } else {
                    WellSignal {
                        score: 0.0,
                        multiplier: 1.0,
                        reason: String::new(),
                    }
                }
            }
            WellSide::Support => {
                if rvol > thresh.rvol_break && efficiency > thresh.eff_high {
                    WellSignal {
                        score: -45.0,
                        multiplier: 1.8,
                        reason: format!("恐慌破位: 卖盘放量贯穿支撑 {}", well.source_string()),
                    }
                } else if rvol > thresh.rvol_extreme && efficiency < thresh.eff_low {
                    if matches!(oi_state, Some(OIPositionState::ShortBuildUp)) && is_tsunami {
                        WellSignal {
                            score: -25.0,
                            multiplier: 1.3,
                            reason: "压制性抛售: 支撑位空头强行挤压".into(),
                        }
                    } else {
                        WellSignal {
                            score: 85.0,
                            multiplier: 2.0,
                            reason: "吸筹承接: 支撑位放量止跌".into(),
                        }
                    }
                } else if taker_ratio.map_or(false, |tr| tr < cfg.absorption_taker_sell_max())
                    && oi_delta.map_or(false, |d| d > cfg.absorption_oi_delta_min())
                    && efficiency < thresh.eff_low
                {
                    WellSignal {
                        score: 55.0,
                        multiplier: 1.6,
                        reason: "被动吸收: 卖盘沉重但价格拒绝下跌".into(),
                    }
                } else {
                    WellSignal {
                        score: 0.0,
                        multiplier: 1.0,
                        reason: String::new(),
                    }
                }
            }
            WellSide::Magnet => {
                if is_up {
                    if rvol > thresh.magnet_rvol_break && efficiency > thresh.magnet_eff_high {
                        WellSignal {
                            score: 55.0,
                            multiplier: 1.7,
                            reason: format!("磁力推进: 价格逼近清算区 {}", well.source_string()),
                        }
                    } else if rvol < thresh.magnet_rvol_shrink && efficiency < thresh.magnet_eff_low
                    {
                        WellSignal {
                            score: 15.0,
                            multiplier: 1.2,
                            reason: format!(
                                "磁力试探: 量能不足，关注是否站稳 {}",
                                well.source_string()
                            ),
                        }
                    } else {
                        WellSignal {
                            score: 0.0,
                            multiplier: 1.0,
                            reason: String::new(),
                        }
                    }
                } else {
                    if rvol > thresh.magnet_rvol_break && efficiency > thresh.magnet_eff_high {
                        WellSignal {
                            score: -55.0,
                            multiplier: 1.7,
                            reason: format!("磁力下压: 价格逼近清算区 {}", well.source_string()),
                        }
                    } else if rvol < thresh.magnet_rvol_shrink && efficiency < thresh.magnet_eff_low
                    {
                        WellSignal {
                            score: -15.0,
                            multiplier: 1.2,
                            reason: format!(
                                "磁力试探: 量能不足，关注是否跌破 {}",
                                well.source_string()
                            ),
                        }
                    } else {
                        WellSignal {
                            score: 0.0,
                            multiplier: 1.0,
                            reason: String::new(),
                        }
                    }
                }
            }
        }
    }

    fn evaluate_trend_extension(
        &self,
        ctx: &MarketContext,
        p_action: &crate::types::market::PriceAction,
        efficiency: f64,
        rvol: f64,
        cfg: &crate::config::VolumeConfig,
        vol_adapt: f64,
        session_adj: f64,
        stress_adj: f64,
    ) -> (f64, f64, String) {
        let regime = ctx
            .get_cached::<TrendStructure>(ContextKey::RegimeStructure)
            .copied();
        let is_up = p_action.close > p_action.open;

        let is_strong_trend = match regime {
            Some(TrendStructure::StrongBullish | TrendStructure::Bullish) => is_up,
            Some(TrendStructure::StrongBearish | TrendStructure::Bearish) => !is_up,
            _ => false,
        };

        if is_strong_trend {
            let base = if is_up {
                cfg.trend_extension_base_score()
            } else {
                -cfg.trend_extension_base_score()
            };
            let mut score = base;
            let reason = if efficiency > cfg.trend_efficiency_threshold() && rvol > 1.2 {
                score *= cfg.trend_extension_eff_boost();
                "趋势延伸(高效): 脱离引力井，量价健康持续"
            } else if efficiency < 0.2 {
                score *= cfg.trend_weak_eff_penalty();
                "趋势延伸(低效): 脱离引力井但动能减弱，谨慎"
            } else {
                "趋势延伸: 脱离引力井，趋势结构维持"
            };
            score *= vol_adapt * session_adj * stress_adj;
            (score, cfg.trend_extension_mult(), reason.to_string())
        } else {
            let base = if is_up {
                cfg.background_score_base()
            } else {
                -cfg.background_score_base()
            };
            if rvol > 1.0 {
                let score = base * rvol.min(2.0) * vol_adapt * session_adj * stress_adj;
                (score, cfg.background_mult(), "背景放量".to_string())
            } else {
                (0.0, 1.0, String::new())
            }
        }
    }
}
