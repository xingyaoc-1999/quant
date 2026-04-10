use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{OIPositionState, PriceAction, PriceGravityWell, WellSide};
use serde_json::json;

// ================= 动态阈值基准常量 =================
// 这些值在 vol_p = 50 (历史中位波动) 时作为基准，实际阈值将随波动率动态调整
const RVOL_BREAK_BASE: f64 = 1.2; // 突破所需相对成交量基准
const RVOL_EXTREME_BASE: f64 = 1.8; // 爆量基准
const EFF_HIGH_BASE: f64 = 1.2; // 高效率基准
const EFF_LOW_BASE: f64 = 0.5; // 低效率基准

// 波动率调节系数范围
const VOL_FACTOR_MIN: f64 = 0.6;
const VOL_FACTOR_MAX: f64 = 1.8;

// 无关键位时的基础评分乘数
const BACKGROUND_SCORE_BASE: f64 = 10.0;
const BACKGROUND_MULT: f64 = 1.1;
// =====================================================

pub struct VolumeStructureAnalyzer;

impl VolumeStructureAnalyzer {
    /// 计算量价效率：单位成交量能推动多少位移
    fn calculate_efficiency(p_action: &PriceAction, avg_volume: f64, atr: f64) -> f64 {
        let rvol = if avg_volume > f64::EPSILON {
            p_action.volume / avg_volume
        } else {
            1.0
        };

        if rvol < 0.1 {
            return 0.0;
        }

        let body_spread = (p_action.close - p_action.open).abs();
        let total_travel = (p_action.high - p_action.low).max(f64::EPSILON);

        // 紧凑度：实体占全长的比例（比例越高，阻力越小）
        let compactness = body_spread / total_travel;
        // 归一化位移：相对于 ATR 的移动距离
        let normalized_move = if atr > f64::EPSILON {
            body_spread / atr
        } else {
            0.0
        };

        // 效率 = (位移 / 努力) * 紧凑度
        let raw_efficiency = (normalized_move / rvol) * compactness;
        raw_efficiency.min(5.0)
    }

    /// 根据波动率百分位计算动态阈值因子
    /// vol_p: 当前波动率在历史中的百分位 (0-100)
    /// 返回因子：低波动时因子 < 1.0 (阈值更严格)，高波动时因子 > 1.0 (阈值更宽松)
    fn compute_vol_factor(vol_p: f64) -> f64 {
        (vol_p / 50.0).clamp(VOL_FACTOR_MIN, VOL_FACTOR_MAX)
    }
}

impl Analyzer for VolumeStructureAnalyzer {
    fn name(&self) -> &'static str {
        "volume_structure_pro_v2"
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
            .unwrap_or_else(|| p_action.volume);

        // --- 核心上下文提取 ---
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

        // --- 动态阈值计算 ---
        let vol_factor = Self::compute_vol_factor(vol_p);
        // 低波动时 vol_factor < 1.0，阈值变大 → 触发条件更严格
        // 高波动时 vol_factor > 1.0，阈值变小 → 触发条件更宽松
        let rvol_break = RVOL_BREAK_BASE / vol_factor;
        let rvol_extreme = RVOL_EXTREME_BASE / vol_factor;
        let eff_high = EFF_HIGH_BASE / vol_factor;
        let eff_low = EFF_LOW_BASE * vol_factor; // 低效率阈值：低波时更严格（值更小）

        let mut m_vol = 1.0;
        let mut score = 0.0;
        let mut res = AnalysisResult::new(self.kind(), "VSA_PRO_V2".into());

        let is_up = p_action.close > p_action.open;
        let rvol = p_action.volume / (avg_volume + 1e-9);
        let efficiency = Self::calculate_efficiency(p_action, avg_volume, atr);

        // 寻找当前最相关的引力源（关键位）
        let active_well = wells.iter().filter(|w| w.is_active).min_by(|a, b| {
            let score_a = a.distance_pct.abs() / (a.strength + 0.1);
            let score_b = b.distance_pct.abs() / (b.strength + 0.1);
            score_a
                .partial_cmp(&score_b)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if let Some(well) = active_well {
            let dist_to_well = well.distance_pct.abs();
            let in_critical_zone = dist_to_well < sigma * 1.5;

            if in_critical_zone {
                match well.side {
                    WellSide::Resistance => {
                        // 场景 A: 强势突破 (高效率)
                        if is_up && rvol > rvol_break && efficiency > eff_high {
                            score = 45.0;
                            m_vol = 1.8;
                            res = res.because(format!(
                                "吸收突破: 价格高效贯穿阻力 {} (rvol={:.2}, eff={:.2})",
                                well.source, rvol, efficiency
                            ));
                        }
                        // 场景 B: 爆量滞涨 (低效率) -> 区分“派发”与“吸收”
                        else if is_up && rvol > rvol_extreme && efficiency < eff_low {
                            if matches!(oi_state, Some(OIPositionState::LongBuildUp)) && is_tsunami
                            {
                                score = 25.0;
                                m_vol = 1.3;
                                res = res.because("强力吸收: 阻力位多头持续接盘，预期推土机式突破");
                            } else {
                                score = -80.0;
                                m_vol = 0.4;
                                res = res
                                    .violate()
                                    .because("派发陷阱: 阻力位放量滞涨，供应完全压制买盘");
                            }
                        }
                    }
                    WellSide::Support => {
                        // 场景 C: 恐慌破位
                        if !is_up && rvol > rvol_break && efficiency > eff_high {
                            score = -45.0;
                            m_vol = 1.8;
                            res = res.because(format!(
                                "恐慌破位: 卖盘放量贯穿支撑 {} (rvol={:.2}, eff={:.2})",
                                well.source, rvol, efficiency
                            ));
                        }
                        // 场景 D: 爆量止跌 -> 区分“诱空”与“承接”
                        else if !is_up && rvol > rvol_extreme && efficiency < eff_low {
                            if matches!(oi_state, Some(OIPositionState::ShortBuildUp)) && is_tsunami
                            {
                                score = -25.0;
                                m_vol = 1.3;
                                res = res.because("压制性抛售: 支撑位空头强行向下挤压");
                            } else {
                                score = 85.0;
                                m_vol = 2.0;
                                res = res.because("吸筹承接: 支撑位放量止跌，大资金入场托盘");
                            }
                        }
                    }
                }
            }
        }

        // 无关键位时的背景评分
        if score == 0.0 {
            let trend_bias = if is_up {
                BACKGROUND_SCORE_BASE
            } else {
                -BACKGROUND_SCORE_BASE
            };
            // 背景评分也稍微考虑成交量放大程度
            if rvol > 1.0 {
                score = trend_bias * (rvol.min(2.0));
                m_vol = BACKGROUND_MULT;
            }
        }

        Ok(res.with_score(score).with_mult(m_vol).debug(json!({
            "eff": (efficiency * 100.0) as i32,
            "rvol": (rvol * 100.0) as i32,
            "vol_factor": vol_factor,
            "rvol_break": rvol_break,
            "rvol_extreme": rvol_extreme,
            "eff_high": eff_high,
            "eff_low": eff_low,
            "tsunami": is_tsunami,
            "oi": format!("{:?}", oi_state)
        })))
    }
}
