use crate::config::{AnalyzerConfig, EntryStrategy};
use crate::types::gravity::PriceGravityWell;
use crate::types::market::{TradeDirection, TrendStructure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ==================== Risk Assessment Output ====================

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RiskAssessment {
    pub direction: TradeDirection,
    pub position_size_pct: f64,
    pub stop_loss_levels: Vec<f64>,
    pub take_profit_levels: Vec<f64>,
    pub weighted_rr: f64,
    pub rr_levels: [f64; 3],
    pub confidence: f64,
    pub confidence_mult: f64,
    pub audit_tags: Vec<String>,
    pub allocation: [f64; 3],
    pub is_tsunami: bool,
    pub estimated_loss_pct: f64,
    pub margin_loss_pct: f64,
    pub max_loss_violated: bool,
    pub trailing_stop_activated: bool,
    pub dynamic_tp_activated: bool,
    pub entry_levels: Vec<f64>,
    pub entry_allocations: Vec<f64>,
}

// ==================== Risk Manager ====================

pub struct RiskManager {
    config: AnalyzerConfig,
    min_stop_dist_pct: f64,
}

impl RiskManager {
    pub fn new(config: AnalyzerConfig) -> Self {
        Self {
            config,
            min_stop_dist_pct: 0.0005,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn assess(
        &self,
        direction: Option<TradeDirection>,
        wells: &[PriceGravityWell],
        last_price: f64,
        atr_ratio: f64,
        average_atr: f64,
        vol_p: f64,
        regime: TrendStructure,
        is_tsunami: bool,
        taker_ratio: f64,
        ma20_dist: Option<f64>,
        net_score: f64,
        max_loss_pct: Option<f64>,
        funding_rate: Option<f64>,
        leverage: f64,
    ) -> Option<RiskAssessment> {
        let dir = direction?;
        let is_long = dir == TradeDirection::Long;

        // 资金费率极端情况拒绝
        if let Some(rate) = funding_rate {
            let cfg = &self.config.risk;
            if cfg.enable_funding_rate {
                if is_long && rate > 0.1 {
                    return None;
                }
                if !is_long && rate < -0.1 {
                    return None;
                }
            }
        }

        let mut tags = Vec::with_capacity(16);
        let atr_v = atr_ratio * last_price;

        let entry_strategy = self.select_entry_strategy(regime, vol_p, is_tsunami);

        let (entry_levels, entry_allocations) = self.calculate_entry_levels_with_strategy(
            wells,
            last_price,
            atr_v,
            is_long,
            is_tsunami,
            &entry_strategy,
            vol_p,
            regime,
            &mut tags,
        );

        let (mut sl, mut tp, alloc) = self.calculate_trade_structure(
            wells,
            last_price,
            atr_v,
            average_atr,
            is_long,
            is_tsunami,
            &mut tags,
        );

        let (wrr, rr_levels) = self.calculate_weighted_rr(last_price, &sl, &tp, &alloc);
        if wrr < self.config.risk.min_weighted_rr {
            return None;
        }

        let (trailing, dynamic) = self.apply_dynamic_adjustments(
            &mut sl, &mut tp, last_price, atr_v, is_long, is_tsunami, &regime, &mut tags,
        );

        let likelihoods = self.compute_likelihoods(
            is_long,
            &regime,
            taker_ratio,
            vol_p,
            wrr,
            ma20_dist,
            atr_ratio,
            net_score,
            is_tsunami,
            funding_rate,
            &mut tags,
        );
        let posterior = self.bayesian_update(self.config.risk.confidence_prior, &likelihoods);
        let conf_mult =
            (posterior * 2.4 - 0.4).clamp(self.config.risk.mult_min, self.config.risk.mult_max);
        tags.push(format!("CONF_MULT:{:.2}", conf_mult));

        let def_strength = self.get_defense_strength(wells, last_price, is_long);

        let (size, total_loss, margin_loss, violated) = self.calculate_final_position(
            def_strength,
            last_price,
            sl[0],
            conf_mult,
            vol_p,
            is_tsunami,
            max_loss_pct,
            leverage,
            &mut tags,
        )?; // 若无法满足风险限制则返回 None（拒绝交易）

        Some(RiskAssessment {
            direction: dir,
            position_size_pct: size,
            stop_loss_levels: sl,
            take_profit_levels: tp,
            weighted_rr: wrr,
            rr_levels,
            confidence: posterior,
            confidence_mult: conf_mult,
            audit_tags: tags,
            allocation: alloc,
            is_tsunami,
            estimated_loss_pct: total_loss,
            margin_loss_pct: margin_loss,
            max_loss_violated: violated,
            trailing_stop_activated: trailing,
            dynamic_tp_activated: dynamic,
            entry_levels,
            entry_allocations,
        })
    }

    // ------------------------------------------------------------
    // 辅助函数
    // ------------------------------------------------------------

    fn select_entry_strategy(
        &self,
        regime: TrendStructure,
        vol_p: f64,
        is_tsunami: bool,
    ) -> EntryStrategy {
        if is_tsunami {
            return EntryStrategy::Stop;
        }
        match regime {
            TrendStructure::StrongBullish | TrendStructure::StrongBearish if vol_p < 70.0 => {
                EntryStrategy::Stop
            }
            TrendStructure::Range => EntryStrategy::Limit,
            _ if vol_p > 70.0 => EntryStrategy::Hybrid,
            _ => self.config.risk.entry_strategy.clone(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn estimate_confidence(
        &self,
        is_long_hint: bool,
        regime: TrendStructure,
        taker_ratio: f64,
        vol_p: f64,
        ma20_dist: Option<f64>,
        atr_ratio: f64,
        net_score: f64,
        is_tsunami: bool,
        funding_rate: Option<f64>,
    ) -> f64 {
        let mut tags = Vec::new();
        let lrs = self.compute_base_likelihoods(
            is_long_hint,
            &regime,
            taker_ratio,
            vol_p,
            ma20_dist,
            atr_ratio,
            net_score,
            is_tsunami,
            funding_rate,
            &mut tags,
        );
        let posterior = self.bayesian_update(self.config.risk.confidence_prior, &lrs);
        (posterior * 2.4 - 0.4).clamp(self.config.risk.mult_min, self.config.risk.mult_max)
    }

    fn compute_base_likelihoods(
        &self,
        is_long: bool,
        regime: &TrendStructure,
        taker_ratio: f64,
        vol_p: f64,
        ma_dist: Option<f64>,
        atr_r: f64,
        net_score: f64,
        is_tsunami: bool,
        funding_rate: Option<f64>,
        tags: &mut Vec<String>,
    ) -> [f64; 8] {
        let mut lrs = [1.0; 8];
        let mut idx = 0;
        let cfg = &self.config.risk;

        let trend_ok = match regime {
            TrendStructure::StrongBullish | TrendStructure::Bullish => is_long,
            TrendStructure::StrongBearish | TrendStructure::Bearish => !is_long,
            _ => false,
        };
        if trend_ok {
            tags.push("TREND_OK".into());
            lrs[idx] = cfg.lr_trend_strong;
        } else {
            tags.push("TREND_WEAK".into());
            lrs[idx] = cfg.lr_trend_weak;
        }
        idx += 1;

        let threshold = 0.52 - ((vol_p - 50.0) / 200.0).clamp(-0.05, 0.05);
        let taker_ok =
            (is_long && taker_ratio > threshold) || (!is_long && taker_ratio < 1.0 - threshold);
        if taker_ok {
            tags.push("TAKER_FLOW_OK".into());
            lrs[idx] = cfg.lr_taker_aligned;
        } else {
            tags.push("TAKER_MISMATCH".into());
            lrs[idx] = cfg.lr_taker_mismatch;
        }
        idx += 1;

        if cfg.enable_funding_rate {
            if let Some(rate) = funding_rate {
                if (is_long && rate > cfg.funding_rate_threshold)
                    || (!is_long && rate < -cfg.funding_rate_threshold)
                {
                    tags.push(format!("FUNDING_CROWDED:{:.4}", rate));
                    lrs[idx] = cfg.funding_rate_penalty;
                    idx += 1;
                }
            }
        }

        if let Some(dist) = ma_dist {
            let limit = atr_r * cfg.ma20_extreme_mult;
            let exceed = (dist.abs() / limit.max(f64::EPSILON)).min(2.0);
            if exceed > 0.5 {
                tags.push(format!("MA_OVEREXTEND:{:.1}%", exceed * 100.0));
                lrs[idx] = if exceed < 1.0 {
                    0.85
                } else if exceed < 1.5 {
                    0.7
                } else {
                    0.55
                };
                idx += 1;
            }
        }

        if vol_p > 70.0 {
            tags.push("HIGH_VOL".into());
            lrs[idx] = 1.25;
        } else if vol_p < 20.0 {
            tags.push("LOW_VOL".into());
            lrs[idx] = 0.8;
        } else {
            lrs[idx] = 1.0;
        }
        idx += 1;

        lrs[idx] = 1.0 + (net_score / 100.0).clamp(-1.0, 1.0) * 0.6;
        idx += 1;

        if is_tsunami {
            lrs[idx] = cfg.lr_tsunami;
            idx += 1;
        }

        lrs
    }

    fn compute_likelihoods(
        &self,
        is_long: bool,
        regime: &TrendStructure,
        taker_ratio: f64,
        vol_p: f64,
        rr: f64,
        ma_dist: Option<f64>,
        atr_r: f64,
        net_score: f64,
        is_tsunami: bool,
        funding_rate: Option<f64>,
        tags: &mut Vec<String>,
    ) -> [f64; 9] {
        let base = self.compute_base_likelihoods(
            is_long,
            regime,
            taker_ratio,
            vol_p,
            ma_dist,
            atr_r,
            net_score,
            is_tsunami,
            funding_rate,
            tags,
        );
        let mut lrs = [1.0; 9];
        for (i, &val) in base.iter().enumerate() {
            lrs[i] = val;
        }
        let min_rr = self.config.risk.rr_min_acceptable;
        lrs[8] = if rr >= min_rr * 1.2 {
            1.4
        } else if rr >= min_rr {
            1.15
        } else if rr >= 1.5 {
            0.9
        } else {
            0.6
        };
        lrs
    }

    fn bayesian_update(&self, prior: f64, likelihoods: &[f64]) -> f64 {
        let prior = prior.clamp(0.01, 0.99);
        let log_odds_prior = (prior / (1.0 - prior)).ln();
        let log_lr: f64 = likelihoods.iter().map(|&lr| lr.clamp(0.1, 10.0).ln()).sum();
        let posterior = 1.0 / (1.0 + (-(log_odds_prior + log_lr)).exp());
        posterior.clamp(0.05, 0.95)
    }

    // ---------- 混合入场策略（已包含动态权重） ----------
    fn calculate_entry_levels_with_strategy(
        &self,
        wells: &[PriceGravityWell],
        last_price: f64,
        atr_v: f64,
        is_long: bool,
        is_tsunami: bool,
        strategy: &EntryStrategy,
        vol_p: f64,
        regime: TrendStructure,
        tags: &mut Vec<String>,
    ) -> (Vec<f64>, Vec<f64>) {
        match strategy {
            EntryStrategy::Limit => {
                let (levels, allocs) = self
                    .calculate_limit_entries(wells, last_price, atr_v, is_long, is_tsunami, tags);
                (levels.to_vec(), allocs.to_vec())
            }
            EntryStrategy::Stop => {
                let (levels, allocs) = self
                    .calculate_stop_entries(wells, last_price, atr_v, is_long, is_tsunami, tags);
                (levels.to_vec(), allocs.to_vec())
            }
            EntryStrategy::Hybrid => {
                let (limit_levels, limit_allocs) = self
                    .calculate_limit_entries(wells, last_price, atr_v, is_long, is_tsunami, tags);
                let (stop_levels, stop_allocs) = self
                    .calculate_stop_entries(wells, last_price, atr_v, is_long, is_tsunami, tags);

                // 动态计算限价单权重
                let limit_weight = if is_tsunami {
                    0.1
                } else {
                    match regime {
                        TrendStructure::Range => 1.0,
                        TrendStructure::StrongBullish | TrendStructure::StrongBearish => {
                            0.2 + (vol_p / 100.0).clamp(0.0, 0.4)
                        }
                        _ => 0.4 + (vol_p / 100.0).clamp(0.0, 0.3),
                    }
                };
                let stop_weight = 1.0 - limit_weight;

                let mut hybrid_levels = [0.0; 3];
                let mut hybrid_allocs = [0.0; 3];

                for i in 0..3 {
                    let l_w = limit_allocs[i] * limit_weight;
                    let s_w = stop_allocs[i] * stop_weight;
                    let total_w = l_w + s_w;
                    if total_w > f64::EPSILON {
                        hybrid_levels[i] = (limit_levels[i] * l_w + stop_levels[i] * s_w) / total_w;
                        hybrid_allocs[i] = total_w;
                    } else {
                        hybrid_levels[i] = (limit_levels[i] + stop_levels[i]) * 0.5;
                        hybrid_allocs[i] = 0.0;
                    }
                }

                let sum_a: f64 = hybrid_allocs.iter().sum();
                if sum_a > f64::EPSILON {
                    for a in &mut hybrid_allocs {
                        *a /= sum_a;
                    }
                } else {
                    hybrid_allocs = [1.0 / 3.0; 3];
                }

                // 按方向排序
                let mut paired: Vec<(f64, f64)> = (0..3)
                    .map(|i| (hybrid_levels[i], hybrid_allocs[i]))
                    .collect();
                if is_long {
                    paired
                        .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                } else {
                    paired
                        .sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                }
                let sorted_levels: Vec<f64> = paired.iter().map(|(l, _)| *l).collect();
                let sorted_allocs: Vec<f64> = paired.iter().map(|(_, a)| *a).collect();

                tags.push(format!("ENTRY_HYBRID_LW_{:.2}", limit_weight));
                (sorted_levels, sorted_allocs)
            }
        }
    }

    fn calculate_limit_entries(
        &self,
        wells: &[PriceGravityWell],
        last_price: f64,
        atr_v: f64,
        is_long: bool,
        is_tsunami: bool,
        tags: &mut Vec<String>,
    ) -> ([f64; 3], [f64; 3]) {
        let dir_sign = if is_long { 1.0 } else { -1.0 };
        let cfg = &self.config.risk;

        let mut defense_wells: Vec<_> = wells
            .iter()
            .filter(|w| {
                w.is_active
                    && if is_long {
                        w.level < last_price
                    } else {
                        w.level > last_price
                    }
            })
            .collect();
        defense_wells.sort_by(|a, b| {
            (a.level - last_price)
                .abs()
                .total_cmp(&(b.level - last_price).abs())
        });

        let count = defense_wells.len().min(3);
        let mut levels = [0.0; 3];
        let mut allocs = [0.0; 3];

        if count == 0 {
            tags.push("ENTRY_LIMIT_NO_DEFENSE".into());
            let step = atr_v * cfg.entry_atr_step_mult;
            levels[0] = last_price;
            allocs[0] = cfg.default_entry_allocations[0];
            levels[1] = last_price - dir_sign * step;
            allocs[1] = cfg.default_entry_allocations[1];
            levels[2] = last_price - dir_sign * step * 2.0;
            allocs[2] = cfg.default_entry_allocations[2];
        } else {
            // 弱防御保护：若仅有一个防御井且强度过低，回退到保守分配
            if count == 1 {
                let solo_w = defense_wells[0];
                if solo_w.strength < cfg.min_reliable_defense_strength {
                    tags.push("ENTRY_LIMIT_WEAK_DEFENSE".into());
                    let step = atr_v * cfg.entry_atr_step_mult;
                    levels[0] = solo_w.level;
                    allocs[0] = 0.3;
                    levels[1] = last_price - dir_sign * step;
                    allocs[1] = cfg.default_entry_allocations[1];
                    levels[2] = last_price - dir_sign * step * 2.0;
                    allocs[2] = cfg.default_entry_allocations[2];
                    if is_tsunami {
                        levels[0] = last_price;
                        tags.push("ENTRY_TSUNAMI_ADJUST".into());
                    }
                    return (levels, allocs);
                }
            }

            let total_strength: f64 = defense_wells[..count]
                .iter()
                .map(|w| w.strength.clamp(0.2, 3.0))
                .sum();
            for i in 0..count {
                let w = defense_wells[i];
                levels[i] = w.level;
                allocs[i] = w.strength.clamp(0.2, 3.0) / total_strength;
            }
            if count < 3 {
                let last_level = defense_wells[count - 1].level;
                let step = atr_v * cfg.entry_atr_step_mult;
                for i in count..3 {
                    levels[i] = last_level - dir_sign * step * (i - count + 1) as f64;
                }
                let remain = 0.2 * (3 - count) as f64;
                let well_total = 1.0 - remain;
                for i in 0..count {
                    allocs[i] *= well_total;
                }
                for i in count..3 {
                    allocs[i] = remain / (3 - count) as f64;
                }
                tags.push(format!("ENTRY_LIMIT_PARTIAL:{}", count));
            } else {
                tags.push("ENTRY_LIMIT_FULL".into());
            }
        }

        if is_tsunami && count > 0 {
            levels[0] = last_price;
            tags.push("ENTRY_TSUNAMI_ADJUST".into());
        }

        (levels, allocs)
    }

    fn calculate_stop_entries(
        &self,
        wells: &[PriceGravityWell],
        last_price: f64,
        atr_v: f64,
        is_long: bool,
        is_tsunami: bool,
        tags: &mut Vec<String>,
    ) -> ([f64; 3], [f64; 3]) {
        let dir_sign = if is_long { 1.0 } else { -1.0 };
        let offset = last_price * self.config.risk.stop_entry_offset_pct;

        let mut targets: Vec<_> = wells
            .iter()
            .filter(|w| {
                w.is_active
                    && if is_long {
                        w.level > last_price
                    } else {
                        w.level < last_price
                    }
            })
            .collect();
        targets.sort_by(|a, b| {
            (a.level - last_price)
                .abs()
                .total_cmp(&(b.level - last_price).abs())
        });

        let count = targets.len().min(3);
        let mut levels = [0.0; 3];
        let mut allocs = [0.0; 3];

        if count == 0 {
            tags.push("ENTRY_STOP_NO_TARGET".into());
            let step = atr_v * 0.5;
            levels[0] = last_price + dir_sign * offset;
            allocs[0] = 0.5;
            levels[1] = last_price + dir_sign * (step + offset);
            allocs[1] = 0.3;
            levels[2] = last_price + dir_sign * (step * 2.0 + offset);
            allocs[2] = 0.2;
        } else {
            for i in 0..count {
                levels[i] = targets[i].level + dir_sign * offset;
            }
            let total_strength: f64 = targets[..count]
                .iter()
                .map(|w| w.strength.clamp(0.2, 3.0))
                .sum();
            for i in 0..count {
                allocs[i] = targets[i].strength.clamp(0.2, 3.0) / total_strength;
            }
            if count < 3 {
                let last_level = levels[count - 1];
                let step = atr_v * 0.5;
                for i in count..3 {
                    levels[i] = last_level + dir_sign * step * (i - count + 1) as f64;
                    allocs[i] = 0.1;
                }
                let sum: f64 = allocs.iter().sum();
                if sum > 0.0 {
                    for a in &mut allocs {
                        *a /= sum;
                    }
                }
            }
            tags.push("ENTRY_STOP".into());
        }

        if is_tsunami && count > 0 {
            levels[0] = last_price + dir_sign * offset;
            tags.push("ENTRY_TSUNAMI_ADJUST".into());
        }

        (levels, allocs)
    }

    fn calculate_trade_structure(
        &self,
        wells: &[PriceGravityWell],
        last_price: f64,
        atr_v: f64,
        average_atr: f64, // 平均 ATR（绝对值）
        is_long: bool,
        is_tsunami: bool,
        tags: &mut Vec<String>,
    ) -> (Vec<f64>, Vec<f64>, [f64; 3]) {
        let dir_sign = if is_long { 1.0 } else { -1.0 };
        let cfg = &self.config.risk;

        let mut targets: Vec<_> = wells
            .iter()
            .filter(|w| {
                w.is_active
                    && if is_long {
                        w.level > last_price
                    } else {
                        w.level < last_price
                    }
            })
            .collect();
        targets.sort_unstable_by(|a, b| {
            (a.level - last_price)
                .abs()
                .total_cmp(&(b.level - last_price).abs())
        });

        let defense = wells
            .iter()
            .filter(|w| {
                w.is_active
                    && if is_long {
                        w.level < last_price
                    } else {
                        w.level > last_price
                    }
            })
            .min_by(|a, b| {
                (a.level - last_price)
                    .abs()
                    .total_cmp(&(b.level - last_price).abs())
            });

        let base_def = defense
            .map(|w| w.level)
            .unwrap_or_else(|| last_price * (1.0 - dir_sign * 0.015));
        let min_sl_dist = last_price * self.min_stop_dist_pct;
        let sl_levels: Vec<f64> = cfg
            .atr_sl_buffers
            .iter()
            .map(|&buf| {
                let raw = base_def - dir_sign * atr_v * buf;
                if is_long {
                    raw.min(last_price - atr_v * cfg.min_sl_atr_mult)
                        .min(last_price - min_sl_dist)
                } else {
                    raw.max(last_price + atr_v * cfg.min_sl_atr_mult)
                        .max(last_price + min_sl_dist)
                }
            })
            .collect();

        let tp1 = targets
            .first()
            .map(|w| w.level)
            .unwrap_or_else(|| last_price * (1.0 + dir_sign * 0.015));
        let tp2 = targets
            .get(1)
            .map(|w| w.level)
            .unwrap_or_else(|| last_price * (1.0 + dir_sign * 0.035));

        // 海啸止盈优化：使用平均 ATR 并考虑远方引力井
        let tp3 = if is_tsunami {
            let base_atr = average_atr.max(atr_v); // 避免波动骤降导致目标过近
            let atr_target = last_price + dir_sign * base_atr * cfg.tsunami_tp3_atr_mult;
            // 若存在第三个目标井，取更远的价位
            let well_target = targets.get(2).map(|w| w.level);
            match well_target {
                Some(wt) => {
                    if is_long {
                        atr_target.max(wt)
                    } else {
                        atr_target.min(wt)
                    }
                }
                None => atr_target,
            }
        } else {
            targets
                .get(2)
                .map(|w| w.level)
                .unwrap_or_else(|| last_price + dir_sign * atr_v * 3.0)
        };

        let allocation = if is_tsunami {
            tags.push("TSUNAMI_ALLOC".into());
            cfg.tsunami_allocation
        } else {
            self.dynamic_allocation(&targets, last_price, tags)
        };

        (sl_levels, vec![tp1, tp2, tp3], allocation)
    }

    fn dynamic_allocation(
        &self,
        targets: &[&PriceGravityWell],
        last_price: f64,
        tags: &mut Vec<String>,
    ) -> [f64; 3] {
        let n = targets.len().min(3);
        if n == 0 {
            tags.push("ALLOC_NO_TARGETS".into());
            return [1.0 / 3.0; 3];
        }

        let mut attractions = [0.0; 3];
        for (i, w) in targets.iter().take(3).enumerate() {
            let dist_pct = ((w.level - last_price).abs() / last_price).max(0.001);
            attractions[i] = w.strength.clamp(0.2, 3.0) / dist_pct;
        }

        if n == 1 {
            tags.push("ALLOC_SINGLE".into());
            return [1.0, 0.0, 0.0];
        }

        if n == 2 {
            attractions[1] += attractions[2];
            attractions[2] = 0.0;
            tags.push("ALLOC_MERGED:2".into());
        }

        let squared: [f64; 3] = [
            attractions[0].powi(2),
            attractions[1].powi(2),
            attractions[2].powi(2),
        ];
        let sum_sq: f64 = squared.iter().sum();
        if sum_sq > f64::EPSILON {
            [
                squared[0] / sum_sq,
                squared[1] / sum_sq,
                squared[2] / sum_sq,
            ]
        } else {
            [1.0 / 3.0; 3]
        }
    }

    fn apply_dynamic_adjustments(
        &self,
        sl: &mut Vec<f64>,
        tp: &mut Vec<f64>,
        last_price: f64,
        atr_v: f64,
        is_long: bool,
        is_tsunami: bool,
        regime: &TrendStructure,
        tags: &mut Vec<String>,
    ) -> (bool, bool) {
        let mut trailing = false;
        let mut dynamic = false;
        let cfg = &self.config.risk;

        let trail_sl = if is_long {
            last_price - atr_v * cfg.trailing_atr_mult
        } else {
            last_price + atr_v * cfg.trailing_atr_mult
        };
        let new_sl = if is_long {
            trail_sl.max(sl[0])
        } else {
            trail_sl.min(sl[0])
        };
        if (is_long && new_sl > sl[0]) || (!is_long && new_sl < sl[0]) {
            sl[0] = new_sl;
            trailing = true;
            tags.push(format!("TRAILING_SL:ATR_{:.1}x", cfg.trailing_atr_mult));
        }

        if (is_long && last_price > tp[0]) || (!is_long && last_price < tp[0]) {
            let tp1_sl = if is_long {
                last_price - atr_v
            } else {
                last_price + atr_v
            };
            let tp1_new = if is_long {
                tp1_sl.max(sl[0])
            } else {
                tp1_sl.min(sl[0])
            };
            if (is_long && tp1_new > sl[0]) || (!is_long && tp1_new < sl[0]) {
                sl[0] = tp1_new;
                trailing = true;
                tags.push("TRAILING_SL:TP1_BREACH".into());
            }
        }

        if is_tsunami {
            // 修复：动态止盈应向外推进目标价位
            let trail_tp = if is_long {
                last_price + atr_v * 3.0 // 向上推
            } else {
                last_price - atr_v * 3.0 // 向下推
            };
            let new_tp3 = if is_long {
                trail_tp.max(tp[2])
            } else {
                trail_tp.min(tp[2])
            };
            if (is_long && new_tp3 > tp[2]) || (!is_long && new_tp3 < tp[2]) {
                tp[2] = new_tp3;
                dynamic = true;
                tags.push("TSUNAMI_TRAILING_TP3".into());
            }
        }

        if !is_tsunami
            && matches!(
                regime,
                TrendStructure::StrongBullish | TrendStructure::StrongBearish
            )
        {
            dynamic = true;
            tags.push("DYNAMIC_TP".into());
        }

        (trailing, dynamic)
    }

    fn calculate_weighted_rr(
        &self,
        price: f64,
        sl: &[f64],
        tp: &[f64],
        alloc: &[f64; 3],
    ) -> (f64, [f64; 3]) {
        let min_dist = price * self.min_stop_dist_pct;
        let risks: Vec<f64> = sl
            .iter()
            .map(|&s| ((price - s).abs()).max(min_dist))
            .collect();
        let rewards: Vec<f64> = tp.iter().map(|&t| (t - price).abs()).collect();
        let rr_levels = [
            rewards[0] / risks[0],
            rewards[1] / risks[1],
            rewards[2] / risks[2],
        ];
        let w_risk = risks
            .iter()
            .enumerate()
            .map(|(i, r)| r * alloc[i])
            .sum::<f64>();
        let w_reward = rewards
            .iter()
            .enumerate()
            .map(|(i, r)| r * alloc[i])
            .sum::<f64>();
        let wrr = if w_risk > f64::EPSILON {
            w_reward / w_risk
        } else {
            0.0
        };
        (wrr, rr_levels)
    }

    fn get_defense_strength(
        &self,
        wells: &[PriceGravityWell],
        last_price: f64,
        is_long: bool,
    ) -> f64 {
        wells
            .iter()
            .filter(|w| {
                w.is_active
                    && if is_long {
                        w.level < last_price
                    } else {
                        w.level > last_price
                    }
            })
            .map(|w| w.strength)
            .next()
            .unwrap_or(1.0)
    }

    /// 计算最终仓位并检查风险上限。
    /// 若无法满足最大亏损限制（即最小仓位仍导致超标），返回 `None` 以拒绝交易。
    fn calculate_final_position(
        &self,
        def_strength: f64,
        last_price: f64,
        sl_level: f64,
        conf_mult: f64,
        vol_p: f64,
        is_tsunami: bool,
        max_loss_pct: Option<f64>,
        leverage: f64,
        tags: &mut Vec<String>,
    ) -> Option<(f64, f64, f64, bool)> {
        let cfg = &self.config.risk;
        let base =
            (def_strength / cfg.max_strength_cap).clamp(cfg.min_base_size, cfg.base_size_max);
        let vol_adj = match vol_p {
            v if v > 80.0 => 0.7,
            v if v > 60.0 => 0.85,
            v if v < 20.0 => 1.15,
            _ => 1.0,
        };
        let mut size = base * vol_adj * conf_mult;
        if is_tsunami {
            size *= 1.2;
            tags.push("TSUNAMI_MODE".into());
        }

        let sl_pct = (last_price - sl_level).abs() / last_price;
        let sl_pct = sl_pct.max(f64::EPSILON);

        let mut total_loss = size * sl_pct * leverage;
        let margin_loss = sl_pct * leverage;
        let mut violated = false;

        if let Some(max_l) = max_loss_pct {
            if total_loss > max_l {
                violated = true;
                size = max_l / (sl_pct * leverage);
                total_loss = max_l;
                tags.push(format!(
                    "RISK_CAPPED:{:.2}% (lev:{:.1}x)",
                    total_loss * 100.0,
                    leverage
                ));
            }
        }

        let final_size = size.clamp(cfg.min_position_size, cfg.max_position_size);
        let final_total_loss = final_size * sl_pct * leverage;

        // 最终检查：即使达到最小仓位，亏损仍超标 → 拒绝交易
        if let Some(max_l) = max_loss_pct {
            if final_total_loss > max_l {
                tags.push(format!(
                    "FINAL_LOSS_VIOLATED:{:.2}% (cannot reduce further)",
                    final_total_loss * 100.0
                ));
                return None;
            }
        }

        Some((final_size, final_total_loss, margin_loss, violated))
    }
}
