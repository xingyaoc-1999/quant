use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{PriceGravityWell, TrendStructure, VolumeState, WellSide};
use serde_json::json;

pub struct LevelProximityAnalyzer;

impl LevelProximityAnalyzer {
    /// 计算非线性引力强度：采用平方衰减，贴近位点时强度爆发
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

        // ========== 1. 上下文环境提取 ==========
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
        let vol_state = ctx
            .get_cached::<serde_json::Value>(ContextKey::VolumeState)
            .and_then(|v| serde_json::from_value::<VolumeState>(v).ok());

        // 动态感应半径：压缩态下更灵敏，高波动下网撒得更宽
        let radius_mult = if is_compressed { 0.5 } else { 0.8 };
        let current_threshold = atr_ratio * (radius_mult + (vol_p / 200.0));
        let confluence_gate = current_threshold * 0.25; // 共振判定门槛

        // ========== 2. 空间探测逻辑 ==========
        let mut tmp_long: f64 = 1.0;
        let mut tmp_short: f64 = 1.0;
        let mut wells = Vec::new();

        let trend_data = ctx.get_role(Role::Trend)?;
        let filter_data = ctx.get_role(Role::Filter)?;
        let t_space = &trend_data.feature_set.space;
        let f_space = &filter_data.feature_set.space;

        // 闭包：处理单侧（支撑或压力）的位点逻辑
        let mut process_level = |dist_opt: Option<f64>, f_dist_opt: Option<f64>, side: WellSide| {
            if let Some(dist) = dist_opt {
                let intensity = Self::calculate_intensity(dist, current_threshold);
                let f_dist = f_dist_opt.unwrap_or(f64::INFINITY);
                let is_confluent = (dist - f_dist).abs() < confluence_gate;
                let boost = if is_confluent { 1.6 } else { 1.0 };

                // 记录逻辑：记录 3 倍半径内的位点，方便 AI 回复远端目标
                if dist < current_threshold * 3.0 {
                    wells.push(PriceGravityWell {
                        level: match side {
                            WellSide::Resistance => last_price * (1.0 + dist),
                            WellSide::Support => last_price * (1.0 - dist),
                        },
                        side,
                        source: if is_confluent {
                            "MTF_Confluence".into()
                        } else {
                            "Trend_Level".into()
                        },
                        distance_pct: if side == WellSide::Support {
                            -dist
                        } else {
                            dist
                        },
                        strength: intensity * boost,
                        is_active: intensity > 0.0,
                    });
                }
                return Some((intensity, boost));
            }
            None
        };

        let res_impact = process_level(
            t_space.dist_to_resistance,
            f_space.dist_to_resistance,
            WellSide::Resistance,
        );
        let sup_impact = process_level(
            t_space.dist_to_support,
            f_space.dist_to_support,
            WellSide::Support,
        );

        // ========== 3. 环境驱动的权重修正 ==========

        // 3.1 压力位影响
        if let Some((intensity, boost)) = res_impact {
            if intensity > 0.0 {
                match (regime, &vol_state) {
                    (TrendStructure::StrongBullish, Some(VolumeState::Expand)) => {
                        tmp_long *= 1.0 + (0.4 * intensity * boost); // 助推
                        tmp_short *= 1.0 - (0.5 * intensity);
                    }
                    (TrendStructure::Range, _) => {
                        tmp_short *= 1.0 + (1.2 * intensity * boost); // 强拦截
                        tmp_long *= 1.0 - (0.6 * intensity);
                    }
                    _ => tmp_long *= 1.0 - (0.4 * intensity),
                }
            }
        }

        // 3.2 支撑位影响
        if let Some((intensity, boost)) = sup_impact {
            if intensity > 0.0 {
                match (regime, &vol_state) {
                    (TrendStructure::StrongBearish, Some(VolumeState::Expand)) => {
                        tmp_short *= 1.0 + (0.4 * intensity * boost); // 助推跌破
                        tmp_long *= 1.0 - (0.5 * intensity);
                    }
                    (TrendStructure::StrongBullish | TrendStructure::Bullish, _) => {
                        tmp_long *= 1.0 + (1.3 * intensity * boost); // 回调买入点
                        tmp_short *= 1.0 - (0.4 * intensity);
                    }
                    _ => tmp_short *= 1.0 - (0.4 * intensity),
                }
            }
        }

        // 3.3 乖离率 (Mean Reversion)
        let ma_dist = t_space.ma20_dist_ratio.unwrap_or(0.0);
        let ma_limit = atr_ratio * 3.2;
        if ma_dist > ma_limit {
            tmp_long *= 0.6;
        } else if ma_dist < -ma_limit {
            tmp_short *= 0.6;
        }

        // ========== 4. 状态持久化与报告生成 ==========
        let m_long = tmp_long.clamp(0.1, 4.0);
        let m_short = tmp_short.clamp(0.1, 4.0);

        ctx.set_cached(ContextKey::MultLongSpace, json!(m_long));
        ctx.set_cached(ContextKey::MultShortSpace, json!(m_short));
        ctx.set_cached(ContextKey::SpaceGravityWells, json!(wells));

        let final_weight = m_long.max(m_short);
        ctx.set_multiplier(self.kind(), final_weight);

        let active_wells: Vec<_> = wells.iter().filter(|w| w.is_active).collect();
        let reason = if active_wells.is_empty() {
            if let Some(nearest) = wells.iter().min_by(|a, b| {
                a.distance_pct
                    .abs()
                    .partial_cmp(&b.distance_pct.abs())
                    .unwrap()
            }) {
                format!(
                    "真空期。远端最近位点: {:.2} ({})",
                    nearest.level,
                    if nearest.side == WellSide::Resistance {
                        "压"
                    } else {
                        "支"
                    }
                )
            } else {
                "处于绝对真空区".to_string()
            }
        } else {
            format!(
                "引力场激活：{}压 / {}支，权重偏向 {:.2}",
                active_wells
                    .iter()
                    .filter(|w| w.side == WellSide::Resistance)
                    .count(),
                active_wells
                    .iter()
                    .filter(|w| w.side == WellSide::Support)
                    .count(),
                final_weight
            )
        };

        Ok(AnalysisResult::new(self.kind(), "LEVEL_PROX".into())
            .with_mult(final_weight)
            .because(reason)
            .debug(json!({
                "m_long": m_long, "m_short": m_short,
                "wells": wells, "threshold": current_threshold
            })))
    }
}
