use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ConfigurableAnalyzer, ContextKey,
    MarketContext, Role,
};
use crate::config::AnalyzerConfig;
use crate::types::gravity::{PriceGravityWell, WellSide, WellSource};
use crate::types::market::TrendStructure;
use std::collections::BTreeSet;
use std::f64::consts::LN_2;

// ==================== GravityExtra ====================
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct GravityExtra {
    pub wells: Vec<PriceGravityWell>,
    pub sigma: f64,
    pub total_support: f64,
    pub total_resistance: f64,
    pub effective_magnet: f64,
    pub ma_converging: bool,
    pub active_well_count: usize,
}

// ==================== WellSourceInput ====================
struct WellSourceInput {
    dist_opt: Option<f64>,
    source: WellSource,
    hits: u32,
    last_ts: i64,
}

// ==================== GravityAnalyzer ====================
pub struct GravityAnalyzer {
    config: AnalyzerConfig,
}

impl ConfigurableAnalyzer for GravityAnalyzer {
    fn with_config(config: AnalyzerConfig) -> Self {
        Self { config }
    }

    fn config(&self) -> &AnalyzerConfig {
        &self.config
    }
}

impl Analyzer for GravityAnalyzer {
    type Extra = GravityExtra;

    fn name(&self) -> &'static str {
        "gravity_v3"
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::Gravity
    }

    fn dependencies(&self) -> Vec<ContextKey> {
        vec![
            ContextKey::VolPercentile,
            ContextKey::VolAtrRatio,
            ContextKey::IsMomentumTsunami,
            ContextKey::RegimeStructure,
        ]
    }

    fn analyze(
        &self,
        ctx: &mut MarketContext,
    ) -> Result<AnalysisResult<Self::Extra>, AnalysisError> {
        let last_price = ctx.global.last_price;
        let now = ctx.global.timestamp;

        if last_price <= f64::EPSILON {
            return Ok(AnalysisResult::new(self.kind()).with_score(0.0));
        }

        // 1. 读取缓存数据
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .copied()
            .unwrap_or(50.0);
        let atr_ratio = ctx
            .get_cached::<f64>(ContextKey::VolAtrRatio)
            .copied()
            .unwrap_or(0.005);

        let cfg = &self.config.gravity;
        let sigma = atr_ratio * (cfg.sigma_atr_mult() + (vol_p / 120.0));
        let confluence_gate = sigma * cfg.confluence_gate_mult();

        // 2. 获取角色特征（修复移动错误）
        let trend_role = ctx.get_role(Role::Trend)?;
        let filter_role = ctx.get_role(Role::Filter).unwrap_or_else(|_| trend_role);
        let t_space = &trend_role.feature_set.space;
        let f_space = &filter_role.feature_set.space;

        let ma_converging = t_space.ma_converging.unwrap_or(false);

        // 3. 读取上一次的井位（用于状态继承）
        let prev_wells: Vec<PriceGravityWell> = ctx
            .get_cached::<Vec<PriceGravityWell>>(ContextKey::SpaceGravityWells)
            .cloned()
            .unwrap_or_default();

        let mut wells: Vec<PriceGravityWell> = Vec::new();

        // 4. 处理 Trend 和 Filter 来源的井
        self.add_well_source(
            WellSourceInput {
                dist_opt: t_space.dist_to_resistance,
                source: WellSource::TrendResistance,
                hits: t_space.res_hit_count,
                last_ts: t_space.res_last_hit,
            },
            &mut wells,
            sigma,
            ma_converging,
            now,
            last_price,
            confluence_gate,
        );

        self.add_well_source(
            WellSourceInput {
                dist_opt: f_space.dist_to_resistance,
                source: WellSource::FilterResistance,
                hits: f_space.res_hit_count,
                last_ts: f_space.res_last_hit,
            },
            &mut wells,
            sigma,
            ma_converging,
            now,
            last_price,
            confluence_gate,
        );

        self.add_well_source(
            WellSourceInput {
                dist_opt: t_space.dist_to_support.map(|d| -d),
                source: WellSource::TrendSupport,
                hits: t_space.sup_hit_count,
                last_ts: t_space.sup_last_hit,
            },
            &mut wells,
            sigma,
            ma_converging,
            now,
            last_price,
            confluence_gate,
        );

        self.add_well_source(
            WellSourceInput {
                dist_opt: f_space.dist_to_support.map(|d| -d),
                source: WellSource::FilterSupport,
                hits: f_space.sup_hit_count,
                last_ts: f_space.sup_last_hit,
            },
            &mut wells,
            sigma,
            ma_converging,
            now,
            last_price,
            confluence_gate,
        );

        if let Some(ratio) = t_space.ma20_dist_ratio {
            self.add_well_source(
                WellSourceInput {
                    dist_opt: Some(1.0 / (ratio + 1.0) - 1.0),
                    source: WellSource::Ma20,
                    hits: 0,
                    last_ts: 0,
                },
                &mut wells,
                sigma,
                ma_converging,
                now,
                last_price,
                confluence_gate,
            );
        }

        // 5. 处理 Entry 周期井（如果启用）
        if cfg.enable_entry_wells {
            if let Ok(entry_role) = ctx.get_role(Role::Entry) {
                let e_space = &entry_role.feature_set.space;
                self.add_well_source(
                    WellSourceInput {
                        dist_opt: e_space.dist_to_resistance,
                        source: WellSource::EntryResistance,
                        hits: e_space.res_hit_count,
                        last_ts: e_space.res_last_hit,
                    },
                    &mut wells,
                    sigma,
                    ma_converging,
                    now,
                    last_price,
                    confluence_gate,
                );
                self.add_well_source(
                    WellSourceInput {
                        dist_opt: e_space.dist_to_support.map(|d| -d),
                        source: WellSource::EntrySupport,
                        hits: e_space.sup_hit_count,
                        last_ts: e_space.sup_last_hit,
                    },
                    &mut wells,
                    sigma,
                    ma_converging,
                    now,
                    last_price,
                    confluence_gate,
                );
            }
        }

        // 6. 继承上一周期的状态
        Self::inherit_well_state(&mut wells, &prev_wells, last_price, confluence_gate * 1.5);

        // 7. 磁力转换（海啸模式）
        let is_tsunami = ctx
            .get_cached::<bool>(ContextKey::IsMomentumTsunami)
            .copied()
            .unwrap_or(false);
        let regime = ctx
            .get_cached::<TrendStructure>(ContextKey::RegimeStructure)
            .copied();

        if is_tsunami {
            Self::apply_magnet_conversion(&mut wells, regime);
        }

        // 8. 磁力确认与磨损处理
        let buffer = (sigma * 0.5).max(0.001);
        let effective_magnet = Self::process_magnet_confirmation(
            &mut wells,
            last_price,
            now,
            buffer,
            cfg.magnet_confirm_ms(),
            cfg.min_hold_ms(),
            regime,
        );

        // 9. 计算总强度
        let total_res = Self::composite_strength(
            wells
                .iter()
                .filter(|w| w.side == WellSide::Resistance && w.is_active),
            cfg.secondary_well_weight(),
            cfg.max_strength_cap(),
        );
        let total_sup = Self::composite_strength(
            wells
                .iter()
                .filter(|w| w.side == WellSide::Support && w.is_active),
            cfg.secondary_well_weight(),
            cfg.max_strength_cap(),
        );

        let raw_score = if is_tsunami {
            match regime {
                Some(TrendStructure::StrongBullish | TrendStructure::Bullish) => {
                    (total_sup + effective_magnet) * 40.0
                }
                Some(TrendStructure::StrongBearish | TrendStructure::Bearish) => {
                    -(total_res + effective_magnet) * 40.0
                }
                _ => (total_sup - total_res) * 40.0,
            }
        } else {
            (total_sup - total_res) * 40.0
        };

        let final_score = (raw_score * if is_tsunami { 0.7 } else { 1.0 }).clamp(-100.0, 100.0);

        // 10. 缓存井位和 sigma
        ctx.set_cached(ContextKey::SpaceGravityWells, wells.clone());
        ctx.set_cached(ContextKey::GravitySigma, sigma);

        let active_well_count = wells.iter().filter(|w| w.is_active).count();
        let extra = GravityExtra {
            wells,
            sigma,
            total_support: total_sup,
            total_resistance: total_res,
            effective_magnet,
            ma_converging,
            active_well_count,
        };

        Ok(AnalysisResult::new(self.kind())
            .with_score(final_score)
            .with_extra(extra))
    }
}

// ==================== 辅助方法实现 ====================
impl GravityAnalyzer {
    fn add_well_source(
        &self,
        input: WellSourceInput,
        wells: &mut Vec<PriceGravityWell>,
        sigma: f64,
        ma_converging: bool,
        now: i64,
        last_price: f64,
        confluence_gate: f64,
    ) {
        let dist_raw = match input.dist_opt {
            Some(d) => d,
            None => return,
        };

        let cfg = &self.config.gravity;
        let source = input.source;
        let weight = source.default_weight();
        let wear_scale = source.wear_scale(cfg);

        let wear_mult = Self::calculate_wear_multiplier(
            input.hits,
            input.last_ts,
            now,
            cfg.critical_hit_count(),
            cfg.steepness(),
            cfg.wear_restore_halflife_ms(),
        ) * wear_scale;

        let mut strength = Self::calculate_intensity(dist_raw.abs(), sigma) * weight * wear_mult;

        if ma_converging {
            strength *= cfg.convergence_boost();
        }

        if strength < cfg.min_well_strength() {
            return;
        }

        let current_level = last_price * (1.0 + dist_raw);
        let side = if dist_raw >= 0.0 {
            WellSide::Resistance
        } else {
            WellSide::Support
        };

        Self::merge_or_insert_well(
            wells,
            current_level,
            side,
            source,
            dist_raw,
            strength,
            input.hits,
            input.last_ts,
            confluence_gate,
            cfg.cross_side_merge_factor(),
            cfg.cross_side_strength_dampen(),
            cfg.active_well_threshold(),
        );
    }

    fn calculate_intensity(dist: f64, sigma: f64) -> f64 {
        if sigma <= f64::EPSILON {
            return 0.0;
        }
        let gauss = (-(dist * dist) / (2.0 * sigma * sigma)).exp();
        let long_tail_scale = 15.0 * sigma;
        let long_range = 0.03 * (-dist.abs() / long_tail_scale).exp();
        gauss.max(long_range)
    }

    fn calculate_wear_multiplier(
        hit_count: u32,
        last_hit_ts: i64,
        now: i64,
        critical_hit_count: u32,
        steepness: f64,
        halflife_ms: f64,
    ) -> f64 {
        if hit_count == 0 {
            return 1.0;
        }
        let h = hit_count as f64;
        let wear_factor = 1.0 / (1.0 + (steepness * (h - critical_hit_count as f64)).exp());

        let time_since = (now - last_hit_ts).max(0) as f64;
        let recovery = if time_since > 0.0 {
            let lambda = LN_2 / halflife_ms;
            1.0 - (-lambda * time_since).exp()
        } else {
            0.0
        };
        (wear_factor + (1.0 - wear_factor) * recovery).min(1.0)
    }

    #[allow(clippy::too_many_arguments)]
    fn merge_or_insert_well(
        wells: &mut Vec<PriceGravityWell>,
        level: f64,
        side: WellSide,
        source: WellSource,
        dist_raw: f64,
        strength: f64,
        hits: u32,
        last_ts: i64,
        confluence_gate: f64,
        cross_merge_factor: f64,
        cross_dampen: f64,
        active_threshold: f64,
    ) {
        let mut merged = false;
        for existing in wells.iter_mut() {
            let diff = (existing.level - level).abs() / level;
            if existing.side == side && diff < confluence_gate {
                existing.strength += strength * 0.6;
                existing.sources.insert(source);
                if strength > active_threshold {
                    existing.is_active = true;
                }
                merged = true;
                break;
            }

            if existing.side != side && diff < confluence_gate * cross_merge_factor {
                existing.strength *= cross_dampen;
            }
        }

        if !merged {
            let mut sources = BTreeSet::new();
            sources.insert(source);
            wells.push(PriceGravityWell {
                level,
                side,
                sources,
                distance_pct: dist_raw,
                strength,
                is_active: strength > active_threshold,
                hit_count: hits,
                last_hit_ts: last_ts,
                magnet_activated: false,
                last_tested_above: false,
                last_tested_below: false,
                cross_ts: 0,
            });
        }
    }

    fn inherit_well_state(
        wells: &mut [PriceGravityWell],
        prev_wells: &[PriceGravityWell],
        last_price: f64,
        inherit_gate: f64,
    ) {
        for well in wells.iter_mut() {
            if let Some(prev) = prev_wells.iter().find(|p| {
                p.side == well.side && (p.level - well.level).abs() / last_price < inherit_gate
            }) {
                well.magnet_activated = prev.magnet_activated;
                well.last_tested_above = prev.last_tested_above;
                well.last_tested_below = prev.last_tested_below;
                well.cross_ts = prev.cross_ts;
            }
        }
    }

    fn apply_magnet_conversion(wells: &mut [PriceGravityWell], regime: Option<TrendStructure>) {
        for well in wells.iter_mut() {
            match regime {
                Some(TrendStructure::StrongBullish | TrendStructure::Bullish)
                    if well.side == WellSide::Resistance =>
                {
                    well.side = WellSide::Magnet;
                    well.magnet_activated = true;
                }
                Some(TrendStructure::StrongBearish | TrendStructure::Bearish)
                    if well.side == WellSide::Support =>
                {
                    well.side = WellSide::Magnet;
                    well.magnet_activated = true;
                }
                _ => {}
            }
        }
    }

    fn process_magnet_confirmation(
        wells: &mut [PriceGravityWell],
        last_price: f64,
        now: i64,
        buffer: f64,
        confirm_ms: i64,
        min_hold_ms: i64,
        regime: Option<TrendStructure>,
    ) -> f64 {
        let mut effective_strength = 0.0;
        for well in wells
            .iter_mut()
            .filter(|w| w.side == WellSide::Magnet && w.is_active)
        {
            let dist_pct = (well.level - last_price) / last_price;

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

            if well.cross_ts > 0 {
                let duration = now - well.cross_ts;
                let mut should_convert = false;
                if dist_pct > buffer && well.last_tested_below {
                    if duration >= confirm_ms {
                        should_convert = true;
                    } else if duration < min_hold_ms {
                        well.cross_ts = 0;
                        well.last_tested_below = false;
                    }
                } else if dist_pct < -buffer && well.last_tested_above {
                    if duration >= confirm_ms {
                        should_convert = true;
                    } else if duration < min_hold_ms {
                        well.cross_ts = 0;
                        well.last_tested_above = false;
                    }
                }

                if should_convert {
                    well.side = if matches!(
                        regime,
                        Some(TrendStructure::StrongBullish | TrendStructure::Bullish)
                    ) {
                        WellSide::Resistance
                    } else {
                        WellSide::Support
                    };
                    well.hit_count += 2;
                    well.last_hit_ts = now;
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
            effective_strength += well.strength * base_weight;
        }
        effective_strength
    }

    fn composite_strength<'a>(
        wells: impl Iterator<Item = &'a PriceGravityWell>,
        secondary_weight: f64,
        max_cap: f64,
    ) -> f64 {
        let mut strengths: Vec<f64> = wells.map(|w| w.strength).collect();
        if strengths.is_empty() {
            return 0.0;
        }
        strengths.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let primary = strengths[0];
        let secondary_sum: f64 = strengths.iter().skip(1).sum();
        (primary + secondary_sum * secondary_weight).min(max_cap)
    }
}
