use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{PriceGravityWell, TrendStructure, WellSide};
use serde_json::json;

pub struct LevelProximityAnalyzer;

impl LevelProximityAnalyzer {
    #[inline]
    fn calculate_intensity(dist: f64, sigma: f64) -> f64 {
        if sigma <= 0.0 {
            return 0.0;
        }
        let gauss = (-(dist * dist) / (2.0 * sigma * sigma)).exp();
        let long_range = 0.05 * (-(dist) / (10.0 * sigma)).exp(); // 长程弱引力
        gauss.max(long_range)
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
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .unwrap_or(50.0);
        let atr_ratio = ctx
            .get_cached::<f64>(ContextKey::VolAtrRatio)
            .unwrap_or(0.005);
        let regime = ctx
            .get_cached::<TrendStructure>(ContextKey::RegimeStructure)
            .unwrap_or(TrendStructure::Range);
        let sigma = atr_ratio * (0.8 + (vol_p / 120.0));
        let confluence_gate = sigma * 0.4;

        let mut wells: Vec<PriceGravityWell> = Vec::new();
        let mut tmp_long: f64 = 1.0;
        let mut tmp_short: f64 = 1.0;

        let trend_data = ctx.get_role(Role::Trend)?;
        let filter_data = ctx.get_role(Role::Filter)?;
        let t_space = &trend_data.feature_set.space;
        let f_space = &filter_data.feature_set.space;
        let mut process_source =
            |dist_opt: Option<f64>, side_hint: Option<WellSide>, label: &str, weight: f64| {
                let dist_raw = if let Some(d) = dist_opt {
                    d
                } else {
                    return;
                };
                let dist_abs = dist_raw.abs();
                let intensity = Self::calculate_intensity(dist_abs, sigma);
                let final_strength = intensity * weight;

                if final_strength < 0.02 && dist_abs > sigma * 4.0 {
                    return;
                }

                let current_level = last_price * (1.0 + dist_raw);
                let side = side_hint.unwrap_or_else(|| {
                    if dist_raw > 0.0 {
                        WellSide::Resistance
                    } else {
                        WellSide::Support
                    }
                });

                let mut merged = false;
                for existing in wells.iter_mut() {
                    let level_diff_pct = (existing.level - current_level).abs() / last_price;
                    if existing.side == side && level_diff_pct < confluence_gate {
                        existing.strength += final_strength * 0.6;
                        if !existing.source.contains(label) {
                            existing.source = format!("{}+{}", existing.source, label);
                        }
                        if final_strength > 0.08 {
                            existing.is_active = true;
                        }
                        merged = true;
                        break;
                    }
                }

                if !merged {
                    wells.push(PriceGravityWell {
                        level: current_level,
                        side,
                        source: label.into(),
                        distance_pct: dist_raw,
                        strength: final_strength,
                        is_active: final_strength > 0.08,
                    });
                }
            };
        process_source(
            t_space.dist_to_resistance,
            Some(WellSide::Resistance),
            "1D_Ext",
            1.0,
        );
        process_source(
            t_space.dist_to_support,
            Some(WellSide::Support),
            "1D_Ext",
            1.0,
        );
        process_source(t_space.ma20_dist_ratio, None, "1D_MA20", 1.2);
        process_source(t_space.ma50_dist_ratio, None, "1D_MA50", 1.4);
        process_source(t_space.ma200_dist_ratio, None, "1D_MA200", 1.8);
        process_source(
            f_space.dist_to_resistance,
            Some(WellSide::Resistance),
            "4H_Ext",
            0.7,
        );
        process_source(
            f_space.dist_to_support,
            Some(WellSide::Support),
            "4H_Ext",
            0.7,
        );
        process_source(f_space.ma20_dist_ratio, None, "4H_MA20", 0.8);
        process_source(f_space.ma50_dist_ratio, None, "4H_MA50", 1.0);
        // ========== 4. 变盘增强逻辑 ==========
        let is_converging = t_space.ma_converging.unwrap_or(false);
        if is_converging {
            for well in wells.iter_mut() {
                well.strength *= 1.35;
            }
        }
        let total_res = wells
            .iter()
            .filter(|w| w.side == WellSide::Resistance)
            .map(|w| w.strength)
            .fold(0.0, f64::max)
            .min(3.0);
        let total_sup = wells
            .iter()
            .filter(|w| w.side == WellSide::Support)
            .map(|w| w.strength)
            .fold(0.0, f64::max)
            .min(3.0);

        let raw_gravity_score = (total_sup - total_res) * 40.0;
        let mut final_score = 0.0;

        match regime {
            TrendStructure::StrongBullish | TrendStructure::Bullish => {
                if total_sup > 0.01 {
                    tmp_long *= 1.0 + (1.7 * total_sup);
                    tmp_short *= 1.0 - (0.5 * total_sup);
                    final_score = raw_gravity_score * 1.5;
                }
                if total_res > 0.01 {
                    tmp_long *= 1.0 + (0.4 * total_res);
                    tmp_short *= 1.0 - (0.4 * total_res);
                    if final_score == 0.0 {
                        final_score = raw_gravity_score * 0.3;
                    }
                }
            }
            TrendStructure::StrongBearish | TrendStructure::Bearish => {
                if total_res > 0.01 {
                    tmp_short *= 1.0 + (1.7 * total_res);
                    tmp_long *= 1.0 - (0.5 * total_res);
                    final_score = raw_gravity_score * 1.5;
                }
                if total_sup > 0.01 {
                    tmp_short *= 1.0 + (0.4 * total_sup);
                    tmp_long *= 1.0 - (0.4 * total_sup);
                    if final_score == 0.0 {
                        final_score = raw_gravity_score * 0.3;
                    }
                }
            }
            TrendStructure::Range => {
                tmp_short *= 1.0 + (1.4 * total_res);
                tmp_long *= 1.0 + (1.4 * total_sup);
                final_score = raw_gravity_score;
            }
        }

        if let Some(d) = t_space.ma20_dist_ratio {
            let limit = atr_ratio * 3.8;
            if d > limit {
                tmp_long *= 0.4;
                final_score = final_score.min(10.0);
            } else if d < -limit {
                tmp_short *= 0.4;
                final_score = final_score.max(-10.0);
            }
        }

        let m_long = tmp_long.clamp(0.2, 3.5);
        let m_short = tmp_short.clamp(0.2, 3.5);

        ctx.set_cached(ContextKey::MultLongSpace, json!(m_long));
        ctx.set_cached(ContextKey::MultShortSpace, json!(m_short));
        ctx.set_cached(ContextKey::SpaceGravityWells, json!(wells));
        ctx.set_cached(ContextKey::Sigma, json!(sigma));
        let max_well_strength = wells.iter().map(|w| w.strength).fold(0.0, f64::max);
        let signal_confidence = (1.0 + max_well_strength * 0.5).clamp(1.0, 2.5);

        let active_count = wells.iter().filter(|w| w.is_active).count();
        let reason = if active_count == 0 {
            "处于空间真空区".into()
        } else {
            let top = wells
                .iter()
                .max_by(|a, b| a.strength.partial_cmp(&b.strength).unwrap())
                .unwrap();
            format!(
                "引力场激活({}). 核心源: {}, 强度: {:.2}",
                active_count, top.source, top.strength
            )
        };

        Ok(AnalysisResult::new(self.kind(), "LEVEL_PROX".into())
            .with_score(final_score.clamp(-100.0, 100.0))
            .with_mult(signal_confidence) // <-- 这里现在代表模块信心
            .because(reason)
            .debug(json!({
                "m_long_bias": m_long,
                "m_short_bias": m_short,
                "signal_confidence": signal_confidence,
                "is_converging": is_converging
            })))
    }
}
