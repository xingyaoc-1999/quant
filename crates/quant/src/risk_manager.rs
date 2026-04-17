use crate::config::AnalyzerConfig;
use crate::types::gravity::PriceGravityWell;
use crate::types::market::{TradeDirection, TrendStructure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ==================== 风险输出结构 ====================
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
    pub max_loss_violated: bool,
    pub trailing_stop_activated: bool,
    pub dynamic_tp_activated: bool,
    pub entry_levels: Vec<f64>,
    pub entry_allocations: Vec<f64>,
}

// ==================== 风控管理器 ====================
pub struct RiskManager {
    config: AnalyzerConfig,
}

impl RiskManager {
    pub fn new(config: AnalyzerConfig) -> Self {
        Self { config }
    }

    // ---------- 主入口 ----------
    pub fn assess(
        &self,
        direction: Option<TradeDirection>,
        wells: &[PriceGravityWell],
        last_price: f64,
        atr_ratio: f64,
        vol_p: f64,
        regime: TrendStructure,
        is_tsunami: bool,
        taker_ratio: f64,
        ma20_dist: Option<f64>,
        net_score: f64,
        max_loss_pct: Option<f64>,
        funding_rate: Option<f64>,
    ) -> Option<RiskAssessment> {
        let dir = direction?;
        let is_long = dir == TradeDirection::Long;
        let mut tags = Vec::with_capacity(16);
        let atr_v = atr_ratio * last_price;

        let (entry_levels, entry_allocations) =
            self.calculate_entry_levels(wells, last_price, atr_v, is_long, is_tsunami, &mut tags);

        let (mut sl, mut tp, alloc) = self
            .calculate_trade_structure(wells, last_price, atr_v, is_long, is_tsunami, &mut tags);

        let (wrr, rr_levels) = self.calculate_weighted_rr(last_price, &sl, &tp, &alloc);

        let (trailing, dynamic) = self.apply_dynamic_adjustments(
            &mut sl, &mut tp, last_price, atr_v, is_long, is_tsunami, &regime, &mut tags,
        );

        let likelihoods = self.compute_likelihoods(
            is_long,
            regime,
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
        let (size, est_loss, violated) = self.calculate_final_position(
            def_strength,
            last_price,
            sl[0],
            conf_mult,
            vol_p,
            is_tsunami,
            max_loss_pct,
            &mut tags,
        );

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
            estimated_loss_pct: est_loss,
            max_loss_violated: violated,
            trailing_stop_activated: trailing,
            dynamic_tp_activated: dynamic,
            entry_levels,
            entry_allocations,
        })
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
        let mut tags = Vec::new();
        let lrs = self.compute_base_likelihoods(
            is_long_hint,
            regime,
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

    // ---------- 基础似然计算（不含 RR） ----------
    fn compute_base_likelihoods(
        &self,
        is_long: bool,
        regime: TrendStructure,
        taker_ratio: f64,
        vol_p: f64,
        ma_dist: Option<f64>,
        atr_r: f64,
        net_score: f64,
        is_tsunami: bool,
        funding_rate: Option<f64>,
        tags: &mut Vec<String>,
    ) -> Vec<f64> {
        let mut lrs = Vec::with_capacity(7);
        let cfg = &self.config.risk;

        // 1. 趋势结构
        let trend_ok = match regime {
            TrendStructure::StrongBullish | TrendStructure::Bullish => is_long,
            TrendStructure::StrongBearish | TrendStructure::Bearish => !is_long,
            _ => false,
        };
        if trend_ok {
            tags.push("TREND_OK".into());
            lrs.push(cfg.lr_trend_strong);
        } else {
            tags.push("TREND_WEAK".into());
            lrs.push(cfg.lr_trend_weak);
        }

        // 2. Taker 流向
        let threshold = 0.52 - ((vol_p - 50.0) / 200.0).clamp(-0.05, 0.05);
        let taker_ok =
            (is_long && taker_ratio > threshold) || (!is_long && taker_ratio < 1.0 - threshold);
        if taker_ok {
            tags.push("TAKER_FLOW_OK".into());
            lrs.push(cfg.lr_taker_aligned);
        } else {
            tags.push("TAKER_MISMATCH".into());
            lrs.push(cfg.lr_taker_mismatch);
        }

        // 3. 资金费率
        if cfg.enable_funding_rate {
            if let Some(rate) = funding_rate {
                if (is_long && rate > cfg.funding_rate_threshold)
                    || (!is_long && rate < -cfg.funding_rate_threshold)
                {
                    tags.push(format!("FUNDING_CROWDED:{:.4}", rate));
                    lrs.push(cfg.funding_rate_penalty);
                }
            }
        }

        // 4. MA 乖离
        if let Some(dist) = ma_dist {
            let limit = atr_r * cfg.ma20_extreme_mult;
            let exceed = (dist.abs() / limit.max(f64::EPSILON)).min(2.0);
            if exceed > 0.5 {
                tags.push(format!("MA_OVEREXTEND:{:.1}%", exceed * 100.0));
                lrs.push(if exceed < 1.0 {
                    0.85
                } else if exceed < 1.5 {
                    0.7
                } else {
                    0.55
                });
            }
        }

        // 5. 波动率
        lrs.push(if vol_p > 70.0 {
            tags.push("HIGH_VOL".into());
            1.25
        } else if vol_p < 20.0 {
            tags.push("LOW_VOL".into());
            0.8
        } else {
            1.0
        });

        // 6. 净得分
        lrs.push(1.0 + (net_score / 100.0).clamp(-1.0, 1.0) * 0.6);

        // 7. 海啸
        if is_tsunami {
            lrs.push(cfg.lr_tsunami);
        }
        lrs
    }

    // ---------- 完整似然计算（含 RR） ----------
    fn compute_likelihoods(
        &self,
        is_long: bool,
        regime: TrendStructure,
        taker_ratio: f64,
        vol_p: f64,
        rr: f64,
        ma_dist: Option<f64>,
        atr_r: f64,
        net_score: f64,
        is_tsunami: bool,
        funding_rate: Option<f64>,
        tags: &mut Vec<String>,
    ) -> Vec<f64> {
        let mut lrs = self.compute_base_likelihoods(
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
        let min_rr = self.config.risk.rr_min_acceptable;
        lrs.push(if rr >= min_rr * 1.2 {
            1.4
        } else if rr >= min_rr {
            1.15
        } else if rr >= 1.5 {
            0.9
        } else {
            0.6
        });
        lrs
    }

    // ---------- 贝叶斯更新 ----------
    fn bayesian_update(&self, prior: f64, likelihoods: &[f64]) -> f64 {
        if likelihoods.is_empty() {
            return prior;
        }
        let mut log_odds = (prior / (1.0 - prior).max(f64::EPSILON)).ln();
        for &lr in likelihoods {
            log_odds += lr.clamp(0.1, 10.0).ln();
        }
        let posterior = 1.0 / (1.0 + (-log_odds).exp());
        posterior.clamp(0.05, 0.95)
    }

    // ---------- 分批建仓点位 ----------
    fn calculate_entry_levels(
        &self,
        wells: &[PriceGravityWell],
        last_price: f64,
        atr_v: f64,
        is_long: bool,
        is_tsunami: bool,
        tags: &mut Vec<String>,
    ) -> (Vec<f64>, Vec<f64>) {
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

        let mut levels = Vec::with_capacity(3);
        let mut allocs = Vec::with_capacity(3);

        if defense_wells.is_empty() {
            tags.push("ENTRY_NO_DEFENSE".into());
            let step = atr_v * cfg.entry_atr_step_mult;
            levels.push(last_price);
            allocs.push(cfg.default_entry_allocations[0]);
            levels.push(last_price - dir_sign * step);
            allocs.push(cfg.default_entry_allocations[1]);
            levels.push(last_price - dir_sign * step * 2.0);
            allocs.push(cfg.default_entry_allocations[2]);
        } else {
            let count = defense_wells.len().min(3);
            let mut total_strength = 0.0;
            for i in 0..count {
                let w = defense_wells[i];
                levels.push(w.level);
                total_strength += w.strength.clamp(0.2, 3.0);
            }
            for i in 0..count {
                let s = defense_wells[i].strength.clamp(0.2, 3.0);
                allocs.push(s / total_strength);
            }
            if count < 3 {
                let last_level = defense_wells[count - 1].level;
                let step = atr_v * cfg.entry_atr_step_mult;
                for i in count..3 {
                    levels.push(last_level - dir_sign * step * (i - count + 1) as f64);
                }
                let remain = 0.2 * (3 - count) as f64;
                let well_total = 1.0 - remain;
                for i in 0..count {
                    allocs[i] *= well_total;
                }
                for _ in count..3 {
                    allocs.push(remain / (3 - count) as f64);
                }
                tags.push(format!("ENTRY_PARTIAL_WELLS:{}", count));
            } else {
                tags.push("ENTRY_FULL_WELLS".into());
            }
        }

        if is_tsunami && !levels.is_empty() {
            levels[0] = last_price;
            tags.push("ENTRY_TSUNAMI_ADJUST".into());
        }
        (levels, allocs)
    }

    // ---------- 交易结构 ----------
    fn calculate_trade_structure(
        &self,
        wells: &[PriceGravityWell],
        last_price: f64,
        atr_v: f64,
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
        let sl_levels: Vec<f64> = cfg
            .atr_sl_buffers
            .iter()
            .map(|&buf| {
                let raw = base_def - dir_sign * atr_v * buf;
                if is_long {
                    raw.min(last_price - atr_v * cfg.min_sl_atr_mult)
                } else {
                    raw.max(last_price + atr_v * cfg.min_sl_atr_mult)
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
        let tp3 = if is_tsunami {
            last_price + dir_sign * atr_v * 5.0
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
            self.dynamic_allocation(&targets, tags)
        };
        (sl_levels, vec![tp1, tp2, tp3], allocation)
    }

    fn dynamic_allocation(
        &self,
        targets: &[&PriceGravityWell],
        tags: &mut Vec<String>,
    ) -> [f64; 3] {
        let actual = targets.len().min(3);
        if actual == 0 {
            tags.push("ALLOC_NO_TARGETS".into());
            return [1.0 / 3.0; 3];
        }
        if actual == 1 {
            tags.push("ALLOC_SINGLE".into());
            return [0.5, 0.3, 0.2];
        }
        let mut strengths = [0.0; 3];
        for (i, w) in targets.iter().take(3).enumerate() {
            strengths[i] = w.strength.clamp(0.2, 3.0);
        }
        if actual == 2 {
            strengths[1] += strengths[2];
            strengths[2] = 0.0;
            tags.push("ALLOC_MERGED:2".into());
        }
        let squared: Vec<f64> = strengths.iter().map(|&s| s * s).collect();
        let sum_sq: f64 = squared.iter().sum();
        if sum_sq > 0.0 {
            [
                squared[0] / sum_sq,
                squared[1] / sum_sq,
                squared[2] / sum_sq,
            ]
        } else {
            [1.0 / 3.0; 3]
        }
    }

    // ---------- 动态调整 ----------
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
            let trail_tp = if is_long {
                last_price - atr_v * 3.0
            } else {
                last_price + atr_v * 3.0
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

    // ---------- 加权盈亏比 ----------
    fn calculate_weighted_rr(
        &self,
        price: f64,
        sl: &[f64],
        tp: &[f64],
        alloc: &[f64; 3],
    ) -> (f64, [f64; 3]) {
        let risks: Vec<f64> = sl.iter().map(|&s| (price - s).abs()).collect();
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

    // ---------- 防御强度 ----------
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

    // ---------- 最终仓位 ----------
    fn calculate_final_position(
        &self,
        def_strength: f64,
        last_price: f64,
        sl_level: f64,
        conf_mult: f64,
        vol_p: f64,
        is_tsunami: bool,
        max_loss_pct: Option<f64>,
        tags: &mut Vec<String>,
    ) -> (f64, f64, bool) {
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
        let mut loss = size * sl_pct;
        let mut violated = false;
        if let Some(max_l) = max_loss_pct {
            if loss > max_l {
                violated = true;
                size = max_l / sl_pct;
                loss = max_l;
                tags.push(format!("RISK_CAPPED:{:.2}%", loss * 100.0));
            }
        }
        let final_size = size.clamp(cfg.min_position_size, cfg.max_position_size);
        let final_loss = final_size * sl_pct;
        if let Some(max_l) = max_loss_pct {
            if final_loss > max_l {
                violated = true;
                tags.push(format!("FINAL_LOSS_VIOLATED:{:.2}%", final_loss * 100.0));
            }
        }
        (final_size, final_loss, violated)
    }
}
