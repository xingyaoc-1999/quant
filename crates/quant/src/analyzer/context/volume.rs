use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{OIPositionState, PriceAction, PriceGravityWell, WellSide};
use serde_json::json;

// ================= 动态阈值基准常量 =================
const RVOL_BREAK_BASE: f64 = 1.2;
const RVOL_EXTREME_BASE: f64 = 1.8;
const EFF_HIGH_BASE: f64 = 1.2;
const EFF_LOW_BASE: f64 = 0.5;

const VOL_FACTOR_MIN: f64 = 0.6;
const VOL_FACTOR_MAX: f64 = 1.8;

const BACKGROUND_SCORE_BASE: f64 = 10.0;
const BACKGROUND_MULT: f64 = 1.1;

// 被动吸收判定阈值
const ABSORPTION_TAKER_BUY_MIN: f64 = 0.55; // 阻力位强力主动买入
const ABSORPTION_TAKER_SELL_MAX: f64 = 0.45; // 支撑位强力主动卖出
const ABSORPTION_OI_DELTA_MIN: f64 = 0.008; // OI 激增阈值 (0.8%)

pub struct VolumeStructureAnalyzer;

impl VolumeStructureAnalyzer {
    fn calculate_efficiency(p_action: &PriceAction, avg_volume: f64, atr: f64) -> f64 {
        let rvol = if avg_volume > f64::EPSILON {
            p_action.volume / avg_volume
        } else {
            1.0
        };

        if rvol < 0.2 {
            return 0.0;
        }

        let body_spread = (p_action.close - p_action.open).abs();
        let total_travel = (p_action.high - p_action.low).max(f64::EPSILON);
        let compactness = body_spread / total_travel;

        let normalized_move = if atr > f64::EPSILON {
            body_spread / atr
        } else {
            0.0
        };

        (normalized_move / rvol * compactness).min(5.0)
    }

    fn compute_vol_factor(vol_p: f64) -> f64 {
        (vol_p / 50.0).clamp(VOL_FACTOR_MIN, VOL_FACTOR_MAX)
    }
}

impl Analyzer for VolumeStructureAnalyzer {
    fn name(&self) -> &'static str {
        "volume_structure_pro_v4"
    }
    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::VolumeProfile
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        let last_price = ctx.global.last_price;

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

        let sigma = ctx
            .get_cached::<f64>(ContextKey::GravitySigma)
            .unwrap_or(0.005);
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .unwrap_or(50.0);
        let wells = ctx
            .get_cached::<Vec<PriceGravityWell>>(ContextKey::SpaceGravityWells)
            .unwrap_or_default();
        let atr = ctx
            .get_cached::<f64>(ContextKey::VolAtrRatio)
            .map(|r| r * last_price)
            .unwrap_or(last_price * 0.01);
        let is_tsunami = ctx
            .get_cached::<bool>(ContextKey::IsMomentumTsunami)
            .unwrap_or(false);
        let oi_state = ctx.get_cached::<OIPositionState>(ContextKey::OiPositionState);

        let vol_factor = Self::compute_vol_factor(vol_p);
        let rvol_break = RVOL_BREAK_BASE / vol_factor;
        let rvol_extreme = RVOL_EXTREME_BASE / vol_factor;
        let eff_high = EFF_HIGH_BASE / vol_factor;
        let eff_low = EFF_LOW_BASE * vol_factor;

        let is_up = p_action.close > p_action.open;
        let rvol = p_action.volume / avg_volume;
        let efficiency = Self::calculate_efficiency(p_action, avg_volume, atr);

        let mut score = 0.0;
        let mut m_vol = 1.0;
        let mut res = AnalysisResult::new(self.kind(), "VSA_PRO_V4".into());

        let target_side = if is_up {
            WellSide::Resistance
        } else {
            WellSide::Support
        };
        let active_well = wells
            .iter()
            .filter(|w| w.is_active && w.side == target_side)
            .min_by(|a, b| {
                let score_a = a.distance_pct.abs() / (a.strength + 0.1);
                let score_b = b.distance_pct.abs() / (b.strength + 0.1);
                score_a
                    .partial_cmp(&score_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

        if let Some(well) = active_well {
            let in_critical_zone = well.distance_pct.abs() < sigma * 1.5;

            if in_critical_zone {
                match well.side {
                    WellSide::Resistance => {
                        if rvol > rvol_break && efficiency > eff_high {
                            score = 45.0;
                            m_vol = 1.8;
                            res =
                                res.because(format!("吸收突破: 价格高效贯穿阻力 {}", well.source));
                        } else if rvol > rvol_extreme && efficiency < eff_low {
                            if matches!(oi_state, Some(OIPositionState::LongBuildUp)) && is_tsunami
                            {
                                score = 25.0;
                                m_vol = 1.3;
                                res = res.because("强力吸收: 阻力位多头持续接盘，预期推土机");
                            } else {
                                score = -80.0;
                                m_vol = 0.4;
                                res = res.violate().because("派发陷阱: 阻力位放量滞涨，供应压制");
                            }
                        } else if taker_ratio.map_or(false, |tr| tr > ABSORPTION_TAKER_BUY_MIN)
                            && oi_delta.map_or(false, |d| d > ABSORPTION_OI_DELTA_MIN)
                            && efficiency < eff_low
                        {
                            score = -65.0;
                            m_vol = 1.6;
                            res = res
                                .violate()
                                .because("阻力被动吸收: 强力买盘被挂单截杀，警惕回撤");
                        }
                    }
                    WellSide::Support => {
                        if rvol > rvol_break && efficiency > eff_high {
                            score = -45.0;
                            m_vol = 1.8;
                            res =
                                res.because(format!("恐慌破位: 卖盘放量贯穿支撑 {}", well.source));
                        } else if rvol > rvol_extreme && efficiency < eff_low {
                            if matches!(oi_state, Some(OIPositionState::ShortBuildUp)) && is_tsunami
                            {
                                score = -25.0;
                                m_vol = 1.3;
                                res = res.because("压制性抛售: 支撑位空头强行挤压");
                            } else {
                                score = 85.0;
                                m_vol = 2.0;
                                res = res.because("吸筹承接: 支撑位放量止跌，大资金托盘");
                            }
                        } else if taker_ratio.map_or(false, |tr| tr < ABSORPTION_TAKER_SELL_MAX)
                            && oi_delta.map_or(false, |d| d > ABSORPTION_OI_DELTA_MIN)
                            && efficiency < eff_low
                        {
                            score = 65.0;
                            m_vol = 1.6;
                            res = res.because("被动吸收: 卖盘沉重但价格拒绝下跌，主力接盘");
                        }
                    }
                    WellSide::Magnet => {
                        if is_up {
                            if rvol > 0.8 && efficiency > 0.6 {
                                score = 55.0;
                                m_vol = 1.7;
                                res = res
                                    .because(format!("磁力推进: 价格逼近清算区 {}", well.source));
                            } else if rvol < 0.5 && efficiency < 0.3 {
                                score = 15.0;
                                m_vol = 1.2;
                                res = res.because(format!(
                                    "磁力试探: 量能不足，关注是否站稳 {}",
                                    well.source
                                ));
                            }
                        } else {
                            if rvol > 0.8 && efficiency > 0.6 {
                                score = -55.0;
                                m_vol = 1.7;
                                res = res
                                    .because(format!("磁力下压: 价格逼近清算区 {}", well.source));
                            } else if rvol < 0.5 && efficiency < 0.3 {
                                score = -15.0;
                                m_vol = 1.2;
                                res = res.because(format!(
                                    "磁力试探: 量能不足，关注是否跌破 {}",
                                    well.source
                                ));
                            }
                        }
                    }
                }
            }
        }

        // 背景评分
        if score == 0.0 {
            let trend_bias = if is_up {
                BACKGROUND_SCORE_BASE
            } else {
                -BACKGROUND_SCORE_BASE
            };
            if rvol > 1.0 {
                score = trend_bias * (rvol.min(2.0));
                m_vol = BACKGROUND_MULT;
            }
        }

        Ok(res.with_score(score).with_mult(m_vol).debug(json!({
            "eff": (efficiency * 100.0) as i32,
            "rvol": (rvol * 100.0) as i32,
            "well_target": active_well.map(|w| &w.source),
            "oi_delta_pct": oi_delta.map(|d| (d * 100.0) as i32),
            "taker_ratio": taker_ratio,
            "is_tsunami": is_tsunami,
        })))
    }
}
