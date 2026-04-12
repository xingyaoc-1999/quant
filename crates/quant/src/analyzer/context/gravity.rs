use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{PriceGravityWell, TrendStructure, WellSide};

// ========== 算法常量配置 ==========
const SIGMA_ATR_MULT: f64 = 0.8;
const CONFLUENCE_GATE_MULT: f64 = 0.4;
const ACTIVE_WELL_THRESHOLD: f64 = 0.08;
const MAX_STRENGTH_CAP: f64 = 3.5;
const SECONDARY_WELL_WEIGHT: f64 = 0.3;
const CONVERGENCE_BOOST: f64 = 1.35; // 均线收敛时的强度加成

const CRITICAL_HIT_COUNT: f64 = 3.0;
const STEEPNESS: f64 = 2.0;

// 磁力井确认逻辑
const MAGNET_CONFIRM_MS: i64 = 180_000; // 3分钟确认
const MIN_HOLD_MS: i64 = 30_000; // 30秒过滤毛刺
const MIN_BUFFER_PCT: f64 = 0.001; // 0.1% 最低缓冲区

pub struct GravityAnalyzer;

impl GravityAnalyzer {
    #[inline]
    fn calculate_intensity(dist: f64, sigma: f64) -> f64 {
        if sigma <= f64::EPSILON {
            return 0.0;
        }
        let gauss = (-(dist * dist) / (2.0 * sigma * sigma)).exp();
        let long_range = 0.05 * (-dist / (10.0 * sigma)).exp();
        gauss.max(long_range)
    }

    fn calculate_wear_multiplier(hit_count: u32, last_hit_ts: i64, now: i64) -> f64 {
        if hit_count == 0 {
            return 1.0;
        }
        let h = hit_count as f64;
        let wear_factor = 1.0 / (1.0 + (STEEPNESS * (h - CRITICAL_HIT_COUNT)).exp());
        let recovery = ((now - last_hit_ts).max(0) as f64 / 3_600_000.0) * 0.05;
        (wear_factor + recovery).min(1.0)
    }

    fn calculate_composite_strength(side_wells: Vec<&PriceGravityWell>) -> f64 {
        if side_wells.is_empty() {
            return 0.0;
        }
        let mut strengths: Vec<f64> = side_wells.iter().map(|w| w.strength).collect();
        strengths.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let primary = strengths[0];
        let secondary_sum: f64 = strengths.iter().skip(1).sum();
        (primary + secondary_sum * SECONDARY_WELL_WEIGHT).min(MAX_STRENGTH_CAP)
    }
}

impl Analyzer for GravityAnalyzer {
    fn name(&self) -> &'static str {
        "level_proximity_pro_v4_final"
    }
    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::SupportResistance
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        let last_price = ctx.global.last_price;
        let now = ctx.global.timestamp;

        if last_price <= 0.0 {
            return Ok(AnalysisResult::new(self.kind(), "LEVEL_PROX_V4".into()).with_score(0.0));
        }

        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .unwrap_or(50.0);
        let atr_ratio = ctx
            .get_cached::<f64>(ContextKey::VolAtrRatio)
            .unwrap_or(0.005);
        let sigma = atr_ratio * (SIGMA_ATR_MULT + (vol_p / 120.0));
        let confluence_gate = sigma * CONFLUENCE_GATE_MULT;

        let trend_role = ctx.get_role(Role::Trend)?;
        let filter_role = ctx.get_role(Role::Filter).unwrap_or(trend_role);
        let t_space = &trend_role.feature_set.space;
        let f_space = &filter_role.feature_set.space;

        // 提取均线收敛状态
        let ma_converging = t_space.ma_converging.unwrap_or(false);

        // 2. 继承旧能级状态
        let prev_wells: Vec<PriceGravityWell> = ctx
            .get_cached::<Vec<PriceGravityWell>>(ContextKey::SpaceGravityWells)
            .unwrap_or_default();

        let mut wells: Vec<PriceGravityWell> = Vec::new();

        // 3. 构造/合并能级
        let mut process_source = |dist_opt: Option<f64>,
                                  side_hint: Option<WellSide>,
                                  label: &str,
                                  weight: f64,
                                  hits: u32,
                                  last_ts: i64| {
            let dist_raw = match dist_opt {
                Some(d) => d,
                None => return,
            };
            let wear_mult = Self::calculate_wear_multiplier(hits, last_ts, now);
            let mut final_strength =
                Self::calculate_intensity(dist_raw.abs(), sigma) * weight * wear_mult;

            if ma_converging {
                final_strength *= CONVERGENCE_BOOST;
            }
            if final_strength < 0.02 {
                return;
            }
            let current_level = last_price * (1.0 + dist_raw);
            let side = side_hint.unwrap_or(if dist_raw >= 0.0 {
                WellSide::Resistance
            } else {
                WellSide::Support
            });

            let mut merged = false;
            for existing in wells.iter_mut() {
                let diff = (existing.level - current_level).abs() / last_price;
                if existing.side == side && diff < confluence_gate {
                    existing.strength += final_strength * 0.6;
                    if !existing.source.contains(label) {
                        if !existing.source.is_empty() {
                            existing.source.push('+');
                        }
                        existing.source.push_str(label);
                    }
                    if final_strength > ACTIVE_WELL_THRESHOLD {
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
                    source: label.to_string(),
                    distance_pct: dist_raw,
                    strength: final_strength,
                    is_active: final_strength > ACTIVE_WELL_THRESHOLD,
                    hit_count: hits,
                    last_hit_ts: last_ts,
                    magnet_activated: false,
                    last_tested_above: false,
                    last_tested_below: false,
                    cross_ts: 0,
                });
            }
        };

        process_source(
            t_space.dist_to_resistance,
            Some(WellSide::Resistance),
            "1D_R",
            1.2,
            t_space.res_hit_count,
            t_space.res_last_hit,
        );
        process_source(
            f_space.dist_to_resistance,
            Some(WellSide::Resistance),
            "4H_R",
            0.8,
            f_space.res_hit_count,
            f_space.res_last_hit,
        );
        process_source(
            t_space.dist_to_support.map(|d| -d),
            Some(WellSide::Support),
            "1D_S",
            1.2,
            t_space.sup_hit_count,
            t_space.sup_last_hit,
        );
        process_source(
            f_space.dist_to_support.map(|d| -d),
            Some(WellSide::Support),
            "4H_S",
            0.8,
            f_space.sup_hit_count,
            f_space.sup_last_hit,
        );
        if let Some(ratio) = t_space.ma20_dist_ratio {
            process_source(Some(1.0 / (ratio + 1.0) - 1.0), None, "1D_MA20", 1.0, 0, 0);
        }

        // 4. 状态继承与增强处理
        for well in wells.iter_mut() {
            if let Some(prev) = prev_wells.iter().find(|p| {
                p.side == well.side
                    && (p.level - well.level).abs() / last_price < confluence_gate * 1.5
            }) {
                well.magnet_activated = prev.magnet_activated;
                well.last_tested_above = prev.last_tested_above;
                well.last_tested_below = prev.last_tested_below;
                well.cross_ts = prev.cross_ts;
            }
        }

        let is_tsunami = ctx
            .get_cached::<bool>(ContextKey::IsMomentumTsunami)
            .unwrap_or(false);
        let regime = ctx.get_cached::<TrendStructure>(ContextKey::RegimeStructure);

        if is_tsunami {
            for well in wells.iter_mut() {
                match regime {
                    Some(TrendStructure::StrongBullish) | Some(TrendStructure::Bullish)
                        if well.side == WellSide::Resistance =>
                    {
                        well.side = WellSide::Magnet;
                        well.magnet_activated = true;
                    }
                    Some(TrendStructure::StrongBearish) | Some(TrendStructure::Bearish)
                        if well.side == WellSide::Support =>
                    {
                        well.side = WellSide::Magnet;
                        well.magnet_activated = true;
                    }
                    _ => {}
                }
            }
        }

        let buffer = (sigma * 0.5).max(MIN_BUFFER_PCT);
        let mut effective_magnet_strength = 0.0;

        for well in wells
            .iter_mut()
            .filter(|w| w.side == WellSide::Magnet && w.is_active)
        {
            let dist_pct = (well.level - last_price) / last_price;

            // 状态机核心更新
            if dist_pct < -buffer {
                if !well.last_tested_below {
                    well.last_tested_below = true;
                    well.last_tested_above = false;
                    well.cross_ts = now;
                }
            } else if dist_pct > buffer {
                if !well.last_tested_above {
                    well.last_tested_above = true;
                    well.last_tested_below = false;
                    well.cross_ts = now;
                }
            } else if (well.last_tested_below && dist_pct > -buffer * 0.5)
                || (well.last_tested_above && dist_pct < buffer * 0.5)
            {
                well.last_tested_below = false;
                well.last_tested_above = false;
                well.cross_ts = 0;
            }

            // 检查确认转换
            if well.cross_ts > 0 {
                let duration = now - well.cross_ts;
                let mut should_convert = false;
                if dist_pct > buffer && well.last_tested_below {
                    if duration >= MAGNET_CONFIRM_MS {
                        should_convert = true;
                    } else if duration < MIN_HOLD_MS {
                        well.cross_ts = 0;
                        well.last_tested_below = false;
                    }
                } else if dist_pct < -buffer && well.last_tested_above {
                    if duration >= MAGNET_CONFIRM_MS {
                        should_convert = true;
                    } else if duration < MIN_HOLD_MS {
                        well.cross_ts = 0;
                        well.last_tested_above = false;
                    }
                }

                if should_convert {
                    well.side = if matches!(
                        regime,
                        Some(TrendStructure::StrongBullish) | Some(TrendStructure::Bullish)
                    ) {
                        WellSide::Resistance
                    } else {
                        WellSide::Support
                    };
                    well.hit_count += 2;
                    well.magnet_activated = false;
                    well.cross_ts = 0;
                    continue;
                }
            }
            let base_weight = if dist_pct < -buffer {
                1.0
            } else if dist_pct.abs() <= buffer {
                0.5
            } else {
                0.2
            };
            effective_magnet_strength += well.strength * base_weight;
        }

        // 6. 最终评分输出
        let total_res = Self::calculate_composite_strength(
            wells
                .iter()
                .filter(|w| w.side == WellSide::Resistance && w.is_active)
                .collect(),
        );
        let total_sup = Self::calculate_composite_strength(
            wells
                .iter()
                .filter(|w| w.side == WellSide::Support && w.is_active)
                .collect(),
        );

        let raw_score = if is_tsunami {
            match regime {
                Some(TrendStructure::StrongBullish) | Some(TrendStructure::Bullish) => {
                    (total_sup + effective_magnet_strength) * 40.0
                }
                Some(TrendStructure::StrongBearish) | Some(TrendStructure::Bearish) => {
                    -(total_res + effective_magnet_strength) * 40.0
                }
                _ => (total_sup - total_res) * 40.0,
            }
        } else {
            (total_sup - total_res) * 40.0
        };

        let final_score = (raw_score * if is_tsunami { 0.7 } else { 1.0 }).clamp(-100.0, 100.0);

        ctx.set_cached(ContextKey::SpaceGravityWells, wells);
        ctx.set_cached(ContextKey::GravitySigma, sigma);

        Ok(
            AnalysisResult::new(self.kind(), "LEVEL_PROX_V4_FINAL".into())
                .with_score(final_score)
                .because(format!(
                    "S:{:.1} R:{:.1} M:{:.1} CVG:{}",
                    total_sup, total_res, effective_magnet_strength, ma_converging
                )),
        )
    }
}
