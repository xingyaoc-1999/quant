use crate::config::{AnalyzerConfig, EntryStrategy};
use crate::types::gravity::PriceGravityWell;
use crate::types::market::{TradeDirection, TrendStructure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RiskAssessment {
    pub direction: TradeDirection,
    pub position_size_pct: f64,
    pub stop_loss_levels: Vec<f64>,
    pub stop_loss_allocations: Vec<f64>,
    pub take_profit_levels: Vec<f64>,
    pub weighted_rr: f64,
    pub confidence: f64,
    pub confidence_mult: f64,
    pub audit_tags: Vec<String>,
    pub allocation: [f64; 2],
    pub entry_strategy: EntryStrategy,
    pub stop_entry_offset_pct: Option<f64>,
    pub is_tsunami: bool,
    pub estimated_loss_pct: f64,
    pub margin_loss_pct: f64,
    pub max_loss_violated: bool,
    pub trailing_stop_activated: bool,
    pub dynamic_tp_activated: bool,
    pub entry_levels: Vec<f64>,
    pub entry_allocations: Vec<f64>,
}

pub struct RiskManager {
    config: AnalyzerConfig,
}

impl RiskManager {
    pub fn new(config: AnalyzerConfig) -> Self {
        Self { config }
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
        reject_reason: &mut Option<String>,
    ) -> Option<RiskAssessment> {
        let dir = direction?;
        let is_long = dir == TradeDirection::Long;

        // 资金费率硬拒绝
        if let Some(rate) = funding_rate {
            let cfg = &self.config.risk;
            if cfg.enable_funding_rate {
                let threshold = dynamic_funding_threshold(vol_p, cfg.funding_rate_threshold);
                if (is_long && rate > threshold) || (!is_long && rate < -threshold) {
                    *reject_reason = Some(format!("funding_rate {:.4} > {:.4}", rate, threshold));
                    return None;
                }
            }
        }

        let mut tags = Vec::with_capacity(16);
        let atr_v = atr_ratio * last_price;

        let (sl_levels, mut tp_levels, tp_alloc, sl_alloc) = self.calculate_trade_structure(
            wells,
            last_price,
            atr_v,
            average_atr,
            is_long,
            is_tsunami,
            vol_p,
            &mut tags,
        );

        let wrr = self.calculate_weighted_rr(
            last_price, last_price, &sl_levels, &sl_alloc, &tp_levels, &tp_alloc,
        );
        let min_wrr = dynamic_min_weighted_rr(vol_p, regime, self.config.risk.min_weighted_rr);
        if wrr < min_wrr {
            *reject_reason = Some(format!("wrr_too_low {:.2} < {:.2}", wrr, min_wrr));
            return None;
        }

        // 3. 贝叶斯置信度计算（需要 wrr）
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
        let prior = dynamic_confidence_prior(vol_p, regime, self.config.risk.confidence_prior);
        let posterior = self.bayesian_update(prior, &likelihoods);
        let (mult_min, mult_max) = dynamic_confidence_range(vol_p, regime);
        let conf_mult = (posterior * 2.4 - 0.4).clamp(mult_min, mult_max);
        tags.push(format!("CONF_MULT:{:.2}", conf_mult));

        // 4. 动态标志
        let trailing = is_tsunami
            || conf_mult > 1.2
            || matches!(
                regime,
                TrendStructure::StrongBullish | TrendStructure::StrongBearish
            );
        let dynamic_tp = !is_tsunami
            && matches!(
                regime,
                TrendStructure::StrongBullish | TrendStructure::StrongBearish
            );
        if dynamic_tp {
            tags.push("DYNAMIC_TP".into());
        }

        // 5. 检查有效目标并拒绝危险的海啸无目标信号
        let has_valid_targets = wells.iter().any(|w| {
            w.is_active
                && if is_long {
                    w.level > last_price
                } else {
                    w.level < last_price
                }
        });
        if is_tsunami && !has_valid_targets {
            *reject_reason = Some("tsunami_no_target".into());
            return None;
        }

        // 6. 确定入场策略
        let entry_strategy =
            self.select_entry_strategy(regime, vol_p, is_tsunami, has_valid_targets, conf_mult);

        // 7. 计算入场价位
        let (entry_levels, entry_allocations, used_offset_pct) = self
            .calculate_entry_levels_with_strategy(
                wells,
                last_price,
                atr_v,
                is_long,
                is_tsunami,
                &entry_strategy,
                &mut tags,
            );

        if entry_strategy == EntryStrategy::Stop {
            let real_entry = entry_levels.first().copied().unwrap_or(last_price);
            let tp_invalid = if is_long {
                tp_levels.first().map_or(false, |&tp| tp <= real_entry)
            } else {
                tp_levels.first().map_or(false, |&tp| tp >= real_entry)
            };
            if tp_invalid {
                let backup = if is_long {
                    tp_levels.get(1).filter(|&&tp| tp > real_entry).copied()
                } else {
                    tp_levels.get(1).filter(|&&tp| tp < real_entry).copied()
                };
                if let Some(new_tp1) = backup {
                    tp_levels[0] = new_tp1;
                } else {
                    *reject_reason = Some("stop_entry_without_valid_tp".into());
                    return None;
                }
            }
        }

        // 9. 重新计算加权盈亏比（使用真实入场价）
        let real_entry = entry_levels.first().copied().unwrap_or(last_price);
        let final_wrr = self.calculate_weighted_rr(
            last_price, real_entry, &sl_levels, &sl_alloc, &tp_levels, &tp_alloc,
        );
        let min_wrr = dynamic_min_weighted_rr(vol_p, regime, self.config.risk.min_weighted_rr);
        if final_wrr < min_wrr {
            *reject_reason = Some(format!("wrr_too_low {:.2} < {:.2}", final_wrr, min_wrr));
            return None;
        }

        // 10. 最终仓位计算（传入真实入场价以修正风险计算）
        let def_strength = self.get_defense_strength(wells, last_price, is_long);
        let dynamic_max_loss = dynamic_max_loss_pct(vol_p, max_loss_pct);

        let (size, total_loss, margin_loss, violated) = self.calculate_final_position(
            def_strength,
            last_price,
            real_entry, // 新增参数，用于准确计算风险
            &sl_levels,
            &sl_alloc,
            conf_mult,
            vol_p,
            is_tsunami,
            dynamic_max_loss,
            leverage,
            regime,
            &mut tags,
            reject_reason,
        )?;

        Some(RiskAssessment {
            direction: dir,
            position_size_pct: size,
            stop_loss_levels: sl_levels,
            stop_loss_allocations: sl_alloc,
            take_profit_levels: tp_levels,
            weighted_rr: final_wrr,
            confidence: posterior,
            confidence_mult: conf_mult,
            audit_tags: tags,
            allocation: tp_alloc,
            entry_strategy,
            stop_entry_offset_pct: used_offset_pct,
            is_tsunami,
            estimated_loss_pct: total_loss,
            margin_loss_pct: margin_loss,
            max_loss_violated: violated,
            trailing_stop_activated: trailing,
            dynamic_tp_activated: dynamic_tp,
            entry_levels,
            entry_allocations,
        })
    }

    fn select_entry_strategy(
        &self,
        regime: TrendStructure,
        vol_p: f64,
        is_tsunami: bool,
        has_valid_target: bool,
        conf_mult: f64,
    ) -> EntryStrategy {
        if conf_mult > 1.2 && has_valid_target {
            return EntryStrategy::Stop;
        }
        if is_tsunami {
            return EntryStrategy::Stop;
        }
        if !has_valid_target {
            return EntryStrategy::Limit;
        }
        match regime {
            TrendStructure::StrongBullish | TrendStructure::StrongBearish if vol_p < 70.0 => {
                EntryStrategy::Stop
            }
            TrendStructure::Range => EntryStrategy::Limit,
            _ if vol_p > 70.0 => EntryStrategy::Stop,
            _ => self.config.risk.entry_strategy.clone(),
        }
    }

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
        let lrs = self.compute_base_likelihoods_quiet(
            is_long_hint,
            &regime,
            taker_ratio,
            vol_p,
            ma20_dist,
            atr_ratio,
            net_score,
            is_tsunami,
            funding_rate,
        );
        let prior = dynamic_confidence_prior(vol_p, regime, self.config.risk.confidence_prior);
        let posterior = self.bayesian_update(prior, &lrs);
        let (mult_min, mult_max) = dynamic_confidence_range(vol_p, regime);
        (posterior * 2.4 - 0.4).clamp(mult_min, mult_max)
    }

    fn compute_base_likelihoods_quiet(
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
    ) -> [f64; 8] {
        let mut dummy = Vec::new();
        self.compute_base_likelihoods_inner(
            is_long,
            regime,
            taker_ratio,
            vol_p,
            ma_dist,
            atr_r,
            net_score,
            is_tsunami,
            funding_rate,
            &mut dummy,
        )
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
        self.compute_base_likelihoods_inner(
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
        )
    }

    fn compute_base_likelihoods_inner(
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
        const IDX_TREND: usize = 0;
        const IDX_TAKER: usize = 1;
        const IDX_FUNDING: usize = 2;
        const IDX_MA_EXTEND: usize = 3;
        const IDX_VOL: usize = 4;
        const IDX_NET_SCORE: usize = 5;
        const IDX_TSUNAMI: usize = 6;

        let mut lrs = [1.0; 8];
        let cfg = &self.config.risk;

        let trend_ok = matches!(
            regime,
            TrendStructure::StrongBullish | TrendStructure::Bullish
                if is_long
        ) || matches!(
            regime,
            TrendStructure::StrongBearish | TrendStructure::Bearish
                if !is_long
        );
        if trend_ok {
            tags.push("TREND_OK".into());
            lrs[IDX_TREND] = cfg.lr_trend_strong;
        } else {
            tags.push("TREND_WEAK".into());
            lrs[IDX_TREND] = cfg.lr_trend_weak;
        }

        let threshold = 0.52 - ((vol_p - 50.0) / 200.0).clamp(-0.05, 0.05);
        let taker_ok =
            (is_long && taker_ratio > threshold) || (!is_long && taker_ratio < 1.0 - threshold);
        if taker_ok {
            tags.push("TAKER_FLOW_OK".into());
            lrs[IDX_TAKER] = cfg.lr_taker_aligned;
        } else {
            tags.push("TAKER_MISMATCH".into());
            lrs[IDX_TAKER] = cfg.lr_taker_mismatch;
        }

        if cfg.enable_funding_rate {
            if let Some(rate) = funding_rate {
                if (is_long && rate > cfg.funding_rate_threshold)
                    || (!is_long && rate < -cfg.funding_rate_threshold)
                {
                    tags.push(format!("FUNDING_CROWDED:{:.4}", rate));
                    lrs[IDX_FUNDING] = cfg.funding_rate_penalty;
                } else {
                    lrs[IDX_FUNDING] = 1.0;
                }
            } else {
                lrs[IDX_FUNDING] = 1.0;
            }
        } else {
            lrs[IDX_FUNDING] = 1.0;
        }

        if let Some(dist) = ma_dist {
            let limit = atr_r * cfg.ma20_extreme_mult;
            let exceed = (dist.abs() / limit.max(f64::EPSILON)).min(2.0);
            if exceed > 0.5 {
                tags.push(format!("MA_OVEREXTEND:{:.1}%", exceed * 100.0));
                lrs[IDX_MA_EXTEND] = if exceed < 1.0 {
                    0.85
                } else if exceed < 1.5 {
                    0.7
                } else {
                    0.55
                };
            } else {
                lrs[IDX_MA_EXTEND] = 1.0;
            }
        } else {
            lrs[IDX_MA_EXTEND] = 1.0;
        }

        if vol_p > 70.0 {
            tags.push("HIGH_VOL".into());
            lrs[IDX_VOL] = 1.25;
        } else if vol_p < 20.0 {
            tags.push("LOW_VOL".into());
            lrs[IDX_VOL] = 0.8;
        } else {
            lrs[IDX_VOL] = 1.0;
        }

        lrs[IDX_NET_SCORE] = 1.0 + (net_score / 100.0).clamp(-1.0, 1.0) * 0.6;

        if is_tsunami {
            lrs[IDX_TSUNAMI] = cfg.lr_tsunami;
        } else {
            lrs[IDX_TSUNAMI] = 1.0;
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
        lrs[..8].copy_from_slice(&base);
        let min_rr = dynamic_min_weighted_rr(vol_p, *regime, self.config.risk.min_weighted_rr);
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

    fn calculate_entry_levels_with_strategy(
        &self,
        wells: &[PriceGravityWell],
        last_price: f64,
        atr_v: f64,
        is_long: bool,
        is_tsunami: bool,
        strategy: &EntryStrategy,
        tags: &mut Vec<String>,
    ) -> (Vec<f64>, Vec<f64>, Option<f64>) {
        match strategy {
            EntryStrategy::Limit => {
                let (levels, allocs) =
                    self.calculate_limit_entries(wells, last_price, atr_v, is_long, tags, strategy);
                (levels.to_vec(), allocs.to_vec(), None)
            }
            EntryStrategy::Stop => {
                let (levels, allocs, used_offset) = self.calculate_stop_entries(
                    wells, last_price, atr_v, is_long, is_tsunami, tags, strategy,
                );
                (levels.to_vec(), allocs.to_vec(), Some(used_offset))
            }
        }
    }

    fn calculate_limit_entries(
        &self,
        wells: &[PriceGravityWell],
        last_price: f64,
        atr_v: f64,
        is_long: bool,
        tags: &mut Vec<String>,
        strategy: &EntryStrategy,
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
            sort_entry_levels(&mut levels, &mut allocs, is_long, strategy);
            return (levels, allocs);
        } else if count == 1 {
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
                // 归一化分配，保证和为1.0
                let sum: f64 = allocs.iter().sum();
                if sum > 0.0 {
                    for a in &mut allocs {
                        *a /= sum;
                    }
                } else {
                    allocs = [0.5, 0.3, 0.2];
                }
                sort_entry_levels(&mut levels, &mut allocs, is_long, strategy);
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

        sort_entry_levels(&mut levels, &mut allocs, is_long, strategy);
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
        strategy: &EntryStrategy,
    ) -> ([f64; 3], [f64; 3], f64) {
        let dir_sign = if is_long { 1.0 } else { -1.0 };
        let cfg = &self.config.risk;
        let base_offset_pct = cfg.stop_entry_offset_pct;
        let atr_pct = atr_v / last_price;

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
        let mut used_offset_pct = base_offset_pct;

        if count == 0 {
            tags.push("ENTRY_STOP_NO_TARGET".into());
            let step = atr_v * 0.5;
            let dynamic_offset_pct = base_offset_pct + atr_pct * 0.5;
            used_offset_pct = dynamic_offset_pct;
            let offset_abs = last_price * dynamic_offset_pct;
            levels[0] = last_price + dir_sign * offset_abs;
            allocs[0] = 0.5;
            levels[1] = last_price + dir_sign * (step + offset_abs);
            allocs[1] = 0.3;
            levels[2] = last_price + dir_sign * (step * 2.0 + offset_abs);
            allocs[2] = 0.2;
            sort_entry_levels(&mut levels, &mut allocs, is_long, strategy);
            return (levels, allocs, used_offset_pct);
        }

        let total_strength: f64 = targets[..count]
            .iter()
            .map(|w| w.strength.clamp(0.2, 3.0))
            .sum();
        for i in 0..count {
            let w = targets[i];
            let dynamic_offset_pct = base_offset_pct + atr_pct * 0.3; // 移除了强度放大因子
            if i == 0 {
                used_offset_pct = dynamic_offset_pct;
            }
            let offset_abs = last_price * dynamic_offset_pct;
            levels[i] = w.level + dir_sign * offset_abs;
            allocs[i] = w.strength.clamp(0.2, 3.0) / total_strength;
        }
        if count < 3 {
            let last_level = levels[count - 1];
            let step = atr_v * 0.5;
            for i in count..3 {
                let dynamic_offset_pct = base_offset_pct + atr_pct * 0.5;
                let offset_abs = last_price * dynamic_offset_pct;
                levels[i] = last_level + dir_sign * (step * (i - count + 1) as f64 + offset_abs);
                allocs[i] = 0.1;
            }
            let sum: f64 = allocs.iter().sum();
            if sum > 0.0 {
                for a in &mut allocs {
                    *a /= sum;
                }
            }
            tags.push(format!("ENTRY_STOP_DYN_OFFSET:{:.4}", used_offset_pct));
        }

        // 海啸模式：重置分配，并同步调整价格
        if is_tsunami {
            let dynamic_offset_pct = base_offset_pct + atr_pct * 0.5;
            used_offset_pct = dynamic_offset_pct;
            let offset_abs = last_price * dynamic_offset_pct;
            let new_level = last_price + dir_sign * offset_abs;

            // 重新生成分配：海啸追单使用固定分配，不再沿用目标井分配
            levels[0] = new_level;
            allocs[0] = 0.5;
            if count >= 2 {
                levels[1] = targets[1].level + dir_sign * offset_abs;
                allocs[1] = 0.3;
                if count >= 3 {
                    levels[2] = targets[2].level + dir_sign * offset_abs;
                    allocs[2] = 0.2;
                } else {
                    // 补足第三个价位
                    let step = atr_v * 0.5;
                    levels[2] = new_level + dir_sign * (step * 2.0 + offset_abs);
                    allocs[2] = 0.2;
                }
            } else {
                // count == 1，只有一个目标井
                let step = atr_v * 0.5;
                levels[1] = new_level + dir_sign * (step + offset_abs);
                allocs[1] = 0.3;
                levels[2] = new_level + dir_sign * (step * 2.0 + offset_abs);
                allocs[2] = 0.2;
            }
            // 确保归一化
            let sum: f64 = allocs.iter().sum();
            if sum > 0.0 {
                for a in &mut allocs {
                    *a /= sum;
                }
            }
            tags.push("ENTRY_TSUNAMI_ADJUST".into());
        }

        sort_entry_levels(&mut levels, &mut allocs, is_long, strategy);
        (levels, allocs, used_offset_pct)
    }

    pub fn calculate_trade_structure(
        &self,
        wells: &[PriceGravityWell],
        last_price: f64,
        atr_v: f64,
        average_atr: f64,
        is_long: bool,
        is_tsunami: bool,
        vol_p: f64,
        tags: &mut Vec<String>,
    ) -> (Vec<f64>, Vec<f64>, [f64; 2], Vec<f64>) {
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

        let mut defenses: Vec<_> = wells
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
        defenses.sort_by(|a, b| {
            (a.level - last_price)
                .abs()
                .total_cmp(&(b.level - last_price).abs())
        });

        let mut sl_levels = Vec::with_capacity(2);
        let mut sl_alloc = Vec::with_capacity(2);
        let min_sl_dist = last_price * cfg.min_stop_dist_pct;

        let base_def = defenses
            .first()
            .map(|w| w.level)
            .unwrap_or_else(|| last_price * (1.0 - dir_sign * 0.015));

        let sl_scale = dynamic_atr_sl_scale(vol_p);
        let mut buffers = cfg
            .atr_sl_buffers
            .iter()
            .take(2)
            .copied()
            .collect::<Vec<_>>();
        while buffers.len() < 2 {
            buffers.push(1.0);
        }
        buffers.iter_mut().for_each(|b| *b *= sl_scale);

        for (i, &buf) in buffers.iter().enumerate() {
            let raw = base_def - dir_sign * atr_v * buf;
            let sl = if is_long {
                raw.min(last_price - atr_v * cfg.min_sl_atr_mult)
                    .min(last_price - min_sl_dist)
            } else {
                raw.max(last_price + atr_v * cfg.min_sl_atr_mult)
                    .max(last_price + min_sl_dist)
            };
            sl_levels.push(sl);
        }

        if defenses.len() >= 2 {
            let s1 = defenses[0].strength;
            let s2 = defenses[1].strength;
            if s1 < 0.5 {
                sl_alloc = vec![0.5, 0.5];
            } else {
                let total = s1 + s2;
                if total > 0.0 {
                    sl_alloc.push(s1 / total);
                    sl_alloc.push(s2 / total);
                } else {
                    sl_alloc = vec![0.5, 0.5];
                }
            }
        } else {
            sl_alloc = vec![0.5, 0.5];
        }
        let sum_sl: f64 = sl_alloc.iter().sum();
        if sum_sl > 0.0 {
            sl_alloc.iter_mut().for_each(|a| *a /= sum_sl);
        } else {
            sl_alloc = vec![0.5, 0.5];
        }

        let tp1 = targets
            .first()
            .map(|w| w.level)
            .unwrap_or_else(|| last_price * (1.0 + dir_sign * 0.015));

        let tp2 = if is_tsunami {
            let base_atr = average_atr.max(atr_v);
            let atr_target = last_price + dir_sign * base_atr * cfg.tsunami_tp2_atr_mult;
            let well_target = targets.get(1).map(|w| w.level);
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
                .get(1)
                .map(|w| w.level)
                .unwrap_or_else(|| last_price + dir_sign * atr_v * 3.0)
        };

        let mut tp_levels = [tp1, tp2];
        let mut tp_alloc = self.dynamic_allocation(&targets, last_price, tags);

        // 只对止损排序
        let n = sl_levels.len().min(2);
        if n > 0 {
            let mut sl_pairs: Vec<(f64, f64)> = sl_levels
                .iter()
                .take(n)
                .zip(sl_alloc.iter().take(n))
                .map(|(&l, &a)| (l, a))
                .collect();
            sl_pairs
                .sort_by(|a, b| ((last_price - a.0).abs()).total_cmp(&(last_price - b.0).abs()));
            for (i, (l, a)) in sl_pairs.into_iter().enumerate() {
                sl_levels[i] = l;
                sl_alloc[i] = a;
            }
        }
        // 止盈不再排序，避免分配错乱
        (sl_levels, tp_levels.to_vec(), tp_alloc, sl_alloc)
    }

    fn dynamic_allocation(
        &self,
        targets: &[&PriceGravityWell],
        last_price: f64,
        tags: &mut Vec<String>,
    ) -> [f64; 2] {
        let n = targets.len().min(2);
        if n == 0 {
            tags.push("ALLOC_NO_TARGETS".into());
            return [0.5, 0.5];
        }

        let mut attractions = [0.0; 2];
        for (i, w) in targets.iter().take(2).enumerate() {
            let dist_pct = ((w.level - last_price).abs() / last_price).max(0.001);
            attractions[i] = w.strength.clamp(0.2, 3.0) / dist_pct;
        }

        if n == 1 {
            tags.push("ALLOC_SINGLE".into());
            return [1.0, 0.0];
        }

        let squared = [attractions[0].powi(2), attractions[1].powi(2)];
        let sum_sq: f64 = squared.iter().sum();
        if sum_sq > f64::EPSILON {
            [squared[0] / sum_sq, squared[1] / sum_sq]
        } else {
            [0.5, 0.5]
        }
    }

    fn calculate_weighted_rr(
        &self,
        price: f64,       // 当前价（仅用于最小距离限制）
        entry_price: f64, // 实际成交参考价
        sl: &[f64],
        sl_alloc: &[f64],
        tp: &[f64],
        tp_alloc: &[f64; 2],
    ) -> f64 {
        if sl.len() < 2 || tp.len() < 2 {
            return 0.0;
        }
        let min_dist = price * self.config.risk.min_stop_dist_pct;
        let mut risks = Vec::with_capacity(2);
        for &s in sl.iter().take(2) {
            let risk = ((entry_price - s).abs()).max(min_dist);
            risks.push(risk);
        }
        let rewards: Vec<f64> = tp
            .iter()
            .take(2)
            .map(|&t| (t - entry_price).abs())
            .collect();

        let w_risk: f64 = risks.iter().zip(sl_alloc.iter()).map(|(r, a)| r * a).sum();
        let w_reward: f64 = rewards
            .iter()
            .zip(tp_alloc.iter())
            .map(|(r, a)| r * a)
            .sum();
        if w_risk > f64::EPSILON {
            w_reward / w_risk
        } else {
            0.0
        }
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

    fn calculate_final_position(
        &self,
        def_strength: f64,
        last_price: f64,
        entry_price: f64, // 新增：真实入场价，用于止损距离计算
        sl_levels: &[f64],
        sl_alloc: &[f64],
        conf_mult: f64,
        vol_p: f64,
        is_tsunami: bool,
        max_loss_pct: Option<f64>,
        leverage: f64,
        regime: TrendStructure,
        tags: &mut Vec<String>,
        reject_reason: &mut Option<String>,
    ) -> Option<(f64, f64, f64, bool)> {
        let cfg = &self.config.risk;
        let base =
            (def_strength / cfg.max_strength_cap).clamp(cfg.min_base_size, cfg.base_size_max);
        let vol_adj = dynamic_vol_position_adj(vol_p);
        let regime_adj = dynamic_regime_position_adj(regime);
        let mut size = base * vol_adj * regime_adj * conf_mult;
        if is_tsunami {
            size *= 1.2;
            tags.push("TSUNAMI_MODE".into());
        }

        let mut weighted_sl_pct = 0.0;
        let mut total_alloc = 0.0;
        for (i, &sl) in sl_levels.iter().enumerate() {
            if i >= sl_alloc.len() {
                break;
            }
            let sl_pct = (entry_price - sl).abs() / entry_price; // 用真实入场价
            weighted_sl_pct += sl_pct * sl_alloc[i];
            total_alloc += sl_alloc[i];
        }
        if total_alloc > 0.0 {
            weighted_sl_pct /= total_alloc;
        } else {
            weighted_sl_pct = 0.01;
        }
        let sl_pct = weighted_sl_pct.max(f64::EPSILON);

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

        if let Some(max_l) = max_loss_pct {
            if final_total_loss > max_l {
                *reject_reason = Some(format!(
                    "max_loss_exceeded ({:.2}% > {:.2}%)",
                    final_total_loss * 100.0,
                    max_l * 100.0
                ));
                tracing::warn!(
                    "Risk reject: price={:.2} weighted_sl_pct={:.4} reason=max_loss_exceeded final_loss={:.2}% max_loss={:.2}%",
                    last_price,
                    sl_pct,
                    final_total_loss * 100.0,
                    max_l * 100.0
                );
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

fn sort_entry_levels(
    levels: &mut [f64; 3],
    allocs: &mut [f64; 3],
    is_long: bool,
    strategy: &EntryStrategy,
) {
    let mut pairs: Vec<(f64, f64)> = levels
        .iter()
        .zip(allocs.iter())
        .map(|(&l, &a)| (l, a))
        .collect();

    let cmp: fn(&(f64, f64), &(f64, f64)) -> std::cmp::Ordering = match strategy {
        EntryStrategy::Limit => {
            if is_long {
                |a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                |a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
            }
        }
        EntryStrategy::Stop => {
            if is_long {
                |a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                |a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
            }
        }
    };

    pairs.sort_by(cmp);

    for (i, (l, a)) in pairs.into_iter().enumerate() {
        levels[i] = l;
        allocs[i] = a;
    }
}

fn dynamic_funding_threshold(vol_p: f64, base_threshold: f64) -> f64 {
    let vol_factor = if vol_p > 70.0 {
        1.6
    } else if vol_p > 50.0 {
        1.3
    } else if vol_p < 25.0 {
        0.7
    } else {
        1.0
    };
    (base_threshold * vol_factor).max(0.0005)
}

fn dynamic_min_weighted_rr(vol_p: f64, regime: TrendStructure, base_rr: f64) -> f64 {
    let vol_factor = if vol_p > 70.0 {
        1.3
    } else if vol_p < 25.0 {
        0.85
    } else {
        1.0
    };
    let regime_factor = match regime {
        TrendStructure::StrongBullish | TrendStructure::StrongBearish => 0.9,
        TrendStructure::Range => 1.1,
        _ => 1.0,
    };
    (base_rr * vol_factor * regime_factor).max(1.0)
}

fn dynamic_confidence_prior(vol_p: f64, regime: TrendStructure, base_prior: f64) -> f64 {
    let vol_factor = if vol_p > 70.0 {
        0.8
    } else if vol_p < 25.0 {
        1.1
    } else {
        1.0
    };
    let regime_factor = match regime {
        TrendStructure::StrongBullish | TrendStructure::StrongBearish => 1.1,
        TrendStructure::Range => 0.9,
        _ => 1.0,
    };
    (base_prior * vol_factor * regime_factor).clamp(0.3, 0.7)
}

fn dynamic_confidence_range(vol_p: f64, regime: TrendStructure) -> (f64, f64) {
    let (min_base, max_base): (f64, f64) = if vol_p > 70.0 {
        (0.3, 1.4)
    } else if vol_p < 25.0 {
        (0.5, 1.8)
    } else {
        (0.4, 1.6)
    };

    let (regime_mult_min, regime_mult_max): (f64, f64) = match regime {
        TrendStructure::StrongBullish | TrendStructure::StrongBearish => (1.1, 1.1),
        TrendStructure::Range => (0.9, 0.9),
        _ => (1.0, 1.0),
    };

    let min = (min_base * regime_mult_min).max(0.2);
    let max = (max_base * regime_mult_max).min(2.0);
    (min, max)
}

fn dynamic_atr_sl_scale(vol_p: f64) -> f64 {
    if vol_p > 70.0 {
        1.4
    } else if vol_p < 25.0 {
        0.8
    } else {
        1.0
    }
}

fn dynamic_vol_position_adj(vol_p: f64) -> f64 {
    if vol_p > 80.0 {
        0.7
    } else if vol_p > 60.0 {
        0.85
    } else if vol_p < 20.0 {
        1.15
    } else {
        1.0
    }
}

fn dynamic_regime_position_adj(regime: TrendStructure) -> f64 {
    match regime {
        TrendStructure::StrongBullish | TrendStructure::StrongBearish => 1.1,
        TrendStructure::Range => 0.9,
        _ => 1.0,
    }
}

fn dynamic_max_loss_pct(vol_p: f64, base_max_loss: Option<f64>) -> Option<f64> {
    base_max_loss.map(|v| {
        let factor = if vol_p > 70.0 {
            0.8
        } else if vol_p < 25.0 {
            1.2
        } else {
            1.0
        };
        v * factor
    })
}
