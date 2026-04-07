use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{PriceGravityWell, TrendStructure, VolumeState};
use serde_json::json;

pub struct LevelProximityAnalyzer;

impl LevelProximityAnalyzer {
    #[inline]
    fn calculate_intensity(dist: f64, threshold: f64) -> f64 {
        if dist >= threshold || threshold <= 0.0 {
            0.0
        } else {
            let ratio = 1.0 - (dist / threshold);
            ratio * ratio
        }
    }
}

impl Analyzer for LevelProximityAnalyzer {
    fn name(&self) -> &'static str {
        "level_proximity"
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::SupportResistance
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        let last_price = ctx.global.last_price;

        // ========== 1. 纯净提取与计算块 ==========
        let (m_long_raw, m_short_raw, wells, threshold, ma_dist, regime_desc) = {
            let trend_data = ctx.get_role(Role::Trend)?;
            let filter_data = ctx.get_role(Role::Filter)?;

            let t_space = &trend_data.feature_set.space;
            let f_space = &filter_data.feature_set.space;

            // 读取解耦后的缓存数据
            let vol_p = ctx
                .get_cached::<f64>(ContextKey::VolPercentile)
                .unwrap_or(50.0);
            let atr_ratio = ctx
                .get_cached::<f64>(ContextKey::VolAtrRatio)
                .unwrap_or(0.005);
            let is_compressed = ctx
                .get_cached::<bool>(ContextKey::VolIsCompressed)
                .unwrap_or(false);

            let regime = ctx
                .get_cached::<TrendStructure>(ContextKey::RegimeStructure)
                .unwrap_or(TrendStructure::Range);

            // 安全读取成交量枚举
            let vol_state = ctx
                .get_cached::<serde_json::Value>(ContextKey::VolumeState)
                .and_then(|v| serde_json::from_value::<VolumeState>(v).ok());

            // 动态参数计算
            let radius_mult = if is_compressed { 0.45 } else { 0.75 };
            let current_threshold = atr_ratio * (radius_mult + (vol_p / 250.0));
            let confluence_gate = current_threshold * 0.3;

            let mut tmp_long = 1.0;
            let mut tmp_short = 1.0;
            let mut tmp_wells = Vec::with_capacity(4);

            // --- 核心逻辑：阻力探测 ---
            if let Some(dist_res) = t_space.dist_to_resistance {
                let intensity = Self::calculate_intensity(dist_res, current_threshold);
                if intensity > 0.0 {
                    let f_dist = f_space.dist_to_resistance.unwrap_or(f64::INFINITY);
                    let is_confluent = (dist_res - f_dist).abs() < confluence_gate;
                    let boost = if is_confluent { 1.5 } else { 1.0 };

                    tmp_wells.push(PriceGravityWell {
                        level: last_price * (1.0 + dist_res),
                        source: if is_confluent {
                            "MTF_Confluence_Res"
                        } else {
                            "Trend_Res"
                        }
                        .into(),
                        distance_pct: dist_res,
                        strength: intensity * boost,
                    });

                    // 【关键修改】: 判定逻辑替换 Squeeze
                    match (regime, &vol_state, is_compressed) {
                        (TrendStructure::StrongBullish, Some(VolumeState::Expand), _) => {
                            // 强多头放量突破阻力：阻力变助推
                            tmp_long *= 1.0 + (0.4 * intensity);
                            tmp_short *= 0.15;
                        }
                        // 震荡缩量 或 任何状态下的波动压缩：阻力有效性增强
                        (TrendStructure::Range, Some(VolumeState::Shrink), _) | (_, _, true) => {
                            tmp_short *= 1.0 + (1.2 * intensity * boost);
                            tmp_long *= 0.4;
                        }
                        _ => {
                            tmp_long *= 1.0 - (0.6 * intensity);
                        }
                    }
                }
            }

            // --- 核心逻辑：支撑探测 ---
            if let Some(dist_sup) = t_space.dist_to_support {
                let intensity = Self::calculate_intensity(dist_sup, current_threshold);
                if intensity > 0.0 {
                    let f_dist = f_space.dist_to_support.unwrap_or(f64::INFINITY);
                    let is_confluent = (dist_sup - f_dist).abs() < confluence_gate;
                    let boost = if is_confluent { 1.6 } else { 1.0 };

                    tmp_wells.push(PriceGravityWell {
                        level: last_price * (1.0 - dist_sup),
                        source: if is_confluent {
                            "MTF_Confluence_Sup"
                        } else {
                            "Trend_Res"
                        }
                        .into(),
                        distance_pct: -dist_sup,
                        strength: intensity * boost,
                    });

                    // 【关键修改】: 判定逻辑替换 Squeeze
                    match (regime, &vol_state, is_compressed) {
                        (TrendStructure::StrongBearish, Some(VolumeState::Expand), _) => {
                            // 强空头放量击穿支撑
                            tmp_short *= 1.1;
                            tmp_long *= 0.1;
                        }
                        // 趋势多头支撑位 或 波动压缩态支撑位：支撑有效性极高
                        (TrendStructure::Bullish, _, _)
                        | (TrendStructure::StrongBullish, _, _)
                        | (_, _, true) => {
                            tmp_long *= 1.0 + (1.3 * intensity * boost);
                            tmp_short *= 0.3;
                        }
                        _ => {
                            tmp_long *= 1.0 + (0.5 * intensity);
                            tmp_short *= 1.0 - (0.7 * intensity);
                        }
                    }
                }
            }

            // --- 空间几何 (均值回归) ---
            let m_dist = t_space.ma20_dist_ratio.unwrap_or(0.0);
            let ma_limit = atr_ratio * 3.5;
            if m_dist.abs() > ma_limit {
                if m_dist > 0.0 {
                    tmp_long *= 0.6;
                } else {
                    tmp_short *= 0.6;
                }
            }

            if t_space.ma_converging.unwrap_or(false) && is_compressed {
                tmp_long *= 1.15;
                tmp_short *= 1.15;
            }

            (
                tmp_long,
                tmp_short,
                tmp_wells,
                current_threshold,
                m_dist,
                format!("{:?}", regime),
            )
        };

        // ========== 2. 状态写入与持久化 ==========
        let m_long = m_long_raw.clamp(0.05, 4.0);
        let m_short = m_short_raw.clamp(0.05, 4.0);

        ctx.set_cached(ContextKey::MultLongSpace, json!(m_long));
        ctx.set_cached(ContextKey::MultShortSpace, json!(m_short));
        ctx.set_cached(ContextKey::SpaceGravityWells, json!(wells));

        let final_weight = m_long.max(m_short);
        ctx.set_multiplier(self.kind(), final_weight);

        let mut res = AnalysisResult::new(self.kind(), "LEVEL_PROX".into());
        res = if wells.is_empty() {
            res.because("价格处于开阔空间，无临近支撑阻力引力")
        } else {
            res.because("触发关键位置引力感应，调整多空博弈权重")
        };

        Ok(res.with_mult(final_weight).debug(json!({
            "m_long": m_long,
            "m_short": m_short,
            "threshold": threshold,
            "ma_dist": ma_dist,
            "well_count": wells.len(),
            "regime": regime_desc
        })))
    }
}
