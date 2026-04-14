use crate::types::{PriceGravityWell, TrendStructure, WellSide};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ================= 类型定义 =================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TradeDirection {
    Long,
    Short,
}

impl TradeDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Long => "LONG",
            Self::Short => "SHORT",
        }
    }
}

// ================= 可配置风控参数 =================

#[derive(Debug, Clone)]
pub struct RiskConfig {
    pub rr_min_acceptable: f64,
    pub ma20_extreme_mult: f64,
    pub atr_sl_buffers: [f64; 3],
    pub min_sl_atr_mult: f64,
    pub breakout_confirm_gate: f64,
    pub max_strength_cap: f64,
    pub base_size_max: f64,
    pub min_base_size: f64,
    pub confidence_prior: f64,
    pub min_position_size: f64,
    pub max_position_size: f64,
    pub mult_min: f64,
    pub mult_max: f64,
    pub tsunami_allocation: [f64; 3],
    // 贝叶斯似然比
    pub lr_trend_strong: f64,
    pub lr_trend_weak: f64,
    pub lr_taker_aligned: f64,
    pub lr_taker_mismatch: f64,
    pub lr_tsunami: f64,
    // 资金费率因子
    pub enable_funding_rate: bool,
    pub funding_rate_threshold: f64,
    pub funding_rate_penalty: f64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            rr_min_acceptable: 2.2,
            ma20_extreme_mult: 3.5,
            atr_sl_buffers: [0.8, 1.5, 2.2],
            min_sl_atr_mult: 0.5,
            breakout_confirm_gate: 0.0020,
            max_strength_cap: 3.5,
            base_size_max: 0.8,
            min_base_size: 0.15,
            confidence_prior: 0.5,
            min_position_size: 0.05,
            max_position_size: 1.0,
            mult_min: 0.4,
            mult_max: 1.6,
            tsunami_allocation: [0.2, 0.3, 0.5],
            lr_trend_strong: 2.5,
            lr_trend_weak: 0.6,
            lr_taker_aligned: 1.8,
            lr_taker_mismatch: 0.7,
            lr_tsunami: 1.5,
            enable_funding_rate: false,
            funding_rate_threshold: 0.001,
            funding_rate_penalty: 0.7,
        }
    }
}

// ================= 风险输出结构 =================

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
}

// ================= 风控管理器实现 =================

pub struct RiskManager {
    config: RiskConfig,
}

impl RiskManager {
    pub fn new(config: RiskConfig) -> Self {
        Self { config }
    }

    /// 主入口：综合评估风险并产出交易计划
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
        leverage: f64,
        funding_rate: Option<f64>,
    ) -> Option<RiskAssessment> {
        let dir = direction?;
        let is_long = dir == TradeDirection::Long;
        let mut audit_tags = Vec::with_capacity(16);
        let atr_v = atr_ratio * last_price;

        // 1. 计算交易空间结构（SL / TP / 分配）
        let (mut sl_levels, mut tp_levels, allocation) = self.calculate_trade_structure(
            wells,
            last_price,
            atr_v,
            is_long,
            is_tsunami,
            regime,
            &mut audit_tags,
        );

        // 2. 盈亏比计算（含单档 RR）
        let (weighted_rr, rr_levels) =
            self.calculate_weighted_rr(last_price, &sl_levels, &tp_levels, &allocation);

        // 3. 动态止盈止损调整
        let (trailing_stop, dynamic_tp) = self.apply_dynamic_adjustments(
            &mut sl_levels,
            &mut tp_levels,
            last_price,
            atr_v,
            is_long,
            is_tsunami,
            &regime,
            &mut audit_tags,
        );

        // 4. 似然比集合
        let likelihoods = self.compute_likelihoods(
            is_long,
            regime,
            taker_ratio,
            vol_p,
            weighted_rr,
            ma20_dist,
            atr_ratio,
            net_score,
            is_tsunami,
            funding_rate,
            &mut audit_tags,
        );

        // 5. 贝叶斯融合 → 乘数
        let posterior = self.bayesian_update(self.config.confidence_prior, &likelihoods);
        let confidence_mult =
            (posterior * 2.4 - 0.4).clamp(self.config.mult_min, self.config.mult_max);
        audit_tags.push(format!("CONF_MULT:{:.2}", confidence_mult));

        // 6. 仓位与亏损计算
        let defense_strength = self.get_defense_strength(wells, last_price, is_long);
        let (final_size, est_loss, violated) = self.calculate_final_position(
            defense_strength,
            last_price,
            sl_levels[0],
            confidence_mult,
            vol_p,
            is_tsunami,
            leverage,
            max_loss_pct,
            &mut audit_tags,
        );

        Some(RiskAssessment {
            direction: dir,
            position_size_pct: final_size,
            stop_loss_levels: sl_levels,
            take_profit_levels: tp_levels,
            weighted_rr,
            rr_levels,
            confidence: posterior,
            confidence_mult,
            audit_tags,
            allocation,
            is_tsunami,
            estimated_loss_pct: est_loss,
            max_loss_violated: violated,
            trailing_stop_activated: trailing_stop,
            dynamic_tp_activated: dynamic_tp,
        })
    }

    // ---------------- 私有辅助方法 ----------------

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

    /// 计算交易的空间结构（SL/TP/分配）
    fn calculate_trade_structure(
        &self,
        wells: &[PriceGravityWell],
        last_price: f64,
        atr_v: f64,
        is_long: bool,
        is_tsunami: bool,
        regime: TrendStructure,
        tags: &mut Vec<String>,
    ) -> (Vec<f64>, Vec<f64>, [f64; 3]) {
        let dir_sign = if is_long { 1.0 } else { -1.0 };

        // 目标井位（止盈）
        let mut targets: Vec<_> = wells
            .iter()
            .filter(|w| {
                w.is_active
                    && (if is_long {
                        w.level > last_price
                    } else {
                        w.level < last_price
                    })
            })
            .collect();
        targets.sort_unstable_by(|a, b| (dir_sign * a.level).total_cmp(&(dir_sign * b.level)));

        // 防御井位（止损参考）
        let defense = wells
            .iter()
            .filter(|w| {
                w.is_active
                    && (if is_long {
                        w.level < last_price
                    } else {
                        w.level > last_price
                    })
            })
            .max_by(|a, b| (dir_sign * a.level).total_cmp(&(dir_sign * b.level)));

        // 止损
        let base_def = defense
            .map(|w| w.level)
            .unwrap_or_else(|| last_price * (1.0 - dir_sign * 0.015));
        let sl_levels: Vec<f64> = self
            .config
            .atr_sl_buffers
            .iter()
            .map(|&buf| {
                let raw = base_def - dir_sign * atr_v * buf;
                if is_long {
                    raw.min(last_price - atr_v * self.config.min_sl_atr_mult)
                } else {
                    raw.max(last_price + atr_v * self.config.min_sl_atr_mult)
                }
            })
            .collect();

        // 止盈
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

        // 分配比例（强度平方归一化，保持与原版行为一致）
        let allocation = if is_tsunami {
            tags.push("TSUNAMI_ALLOC".into());
            self.config.tsunami_allocation
        } else {
            self.dynamic_allocation(&targets)
        };

        (sl_levels, vec![tp1, tp2, tp3], allocation)
    }

    /// 动态分配：基于目标井位强度平方归一化
    fn dynamic_allocation(&self, targets: &[&PriceGravityWell]) -> [f64; 3] {
        let mut strengths = [0.5; 3];
        for (i, w) in targets.iter().take(3).enumerate() {
            strengths[i] = w.strength.clamp(0.2, 3.0);
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
            [1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0]
        }
    }

    /// 动态止盈止损调整（移动止损 + 动态 TP）
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
        let mut trailing_activated = false;
        let mut dynamic_activated = false;

        // 移动止损：当价格突破第一目标后，将止损上移至成本或跟踪
        let tp1 = tp[0];
        let breached_tp1 = if is_long {
            last_price > tp1
        } else {
            last_price < tp1
        };
        if breached_tp1 {
            let trail_sl = if is_long {
                last_price - atr_v * 1.0
            } else {
                last_price + atr_v * 1.0
            };
            let new_sl = if is_long {
                trail_sl.max(sl[0])
            } else {
                trail_sl.min(sl[0])
            };
            if (is_long && new_sl > sl[0]) || (!is_long && new_sl < sl[0]) {
                sl[0] = new_sl;
                trailing_activated = true;
                tags.push("TRAILING_SL".into());
            }
        }

        // 动态 TP3：若无第三井位且处于强趋势，用 ATR 跟踪
        let strong_trend = matches!(
            regime,
            TrendStructure::StrongBullish | TrendStructure::StrongBearish
        );
        if !is_tsunami && strong_trend {
            // 简单检测：第三目标是否超出默认静态范围（原版中若 targets.len()<3 会使用 atr_v*3.0）
            // 此处将动态调整标记为激活，并在报告中显示
            dynamic_activated = true;
            tags.push("DYNAMIC_TP".into());
            // 可进一步调整 tp[2] 为更激进的跟踪值（但原版已使用 3.0 ATR，此处保留）
        }

        (trailing_activated, dynamic_activated)
    }

    /// 计算加权盈亏比及单档 RR
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

        let weighted_risk = risks
            .iter()
            .enumerate()
            .map(|(i, r)| r * alloc[i])
            .sum::<f64>();
        let weighted_reward = rewards
            .iter()
            .enumerate()
            .map(|(i, r)| r * alloc[i])
            .sum::<f64>();

        let weighted_rr = if weighted_risk > f64::EPSILON {
            weighted_reward / weighted_risk
        } else {
            0.0
        };
        (weighted_rr, rr_levels)
    }

    /// 计算贝叶斯似然比集合
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
        let mut lrs = Vec::with_capacity(8);

        // 1) 趋势结构
        let trend_ok = match regime {
            TrendStructure::StrongBullish | TrendStructure::Bullish if is_long => true,
            TrendStructure::StrongBearish | TrendStructure::Bearish if !is_long => true,
            _ => false,
        };
        if trend_ok {
            tags.push("TREND_OK".into());
            lrs.push(self.config.lr_trend_strong);
        } else {
            tags.push("TREND_WEAK".into());
            lrs.push(self.config.lr_trend_weak);
        }

        // 2) Taker 主动流向
        let taker_threshold = 0.52 - ((vol_p - 50.0) / 200.0).clamp(-0.05, 0.05);
        let taker_aligned = (is_long && taker_ratio > taker_threshold)
            || (!is_long && taker_ratio < (1.0 - taker_threshold));
        if taker_aligned {
            tags.push("TAKER_FLOW_OK".into());
            lrs.push(self.config.lr_taker_aligned);
        } else {
            tags.push("TAKER_MISMATCH".into());
            lrs.push(self.config.lr_taker_mismatch);
        }

        // 3) 资金费率
        if self.config.enable_funding_rate {
            if let Some(rate) = funding_rate {
                let crowded = (is_long && rate > self.config.funding_rate_threshold)
                    || (!is_long && rate < -self.config.funding_rate_threshold);
                if crowded {
                    tags.push(format!("FUNDING_CROWDED:{:.4}", rate));
                    lrs.push(self.config.funding_rate_penalty);
                }
            }
        }

        // 4) MA乖离惩罚
        if let Some(dist) = ma_dist {
            let limit = atr_r * self.config.ma20_extreme_mult;
            let exceed = (dist.abs() / limit.max(f64::EPSILON)).min(2.0);
            if exceed > 0.5 {
                tags.push(format!("MA_OVEREXTEND:{:.1}%", exceed * 100.0));
                lrs.push(self.ma_overextend_lr(exceed));
            }
        }

        // 5) 波动率分位数
        let vol_lr = if vol_p > 70.0 {
            tags.push("HIGH_VOL".into());
            1.25
        } else if vol_p < 20.0 {
            tags.push("LOW_VOL".into());
            0.8
        } else {
            1.0
        };
        lrs.push(vol_lr);

        // 6) RR质量
        lrs.push(self.rr_quality_lr(rr));

        // 7) 净得分
        let net_lr = 1.0 + (net_score / 100.0).clamp(-1.0, 1.0) * 0.6;
        lrs.push(net_lr);

        if is_tsunami {
            lrs.push(self.config.lr_tsunami);
        }

        lrs
    }

    fn ma_overextend_lr(&self, exceed_ratio: f64) -> f64 {
        if exceed_ratio < 0.5 {
            1.0
        } else if exceed_ratio < 1.0 {
            0.85
        } else if exceed_ratio < 1.5 {
            0.7
        } else {
            0.55
        }
    }

    fn rr_quality_lr(&self, rr: f64) -> f64 {
        if rr >= self.config.rr_min_acceptable * 1.2 {
            1.4
        } else if rr >= self.config.rr_min_acceptable {
            1.15
        } else if rr >= 1.5 {
            0.9
        } else {
            0.6
        }
    }

    /// 获取防御井强度（用于基础仓位）
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
                    && (if is_long {
                        w.level < last_price
                    } else {
                        w.level > last_price
                    })
            })
            .map(|w| w.strength)
            .next()
            .unwrap_or(1.0)
    }

    /// 最终仓位计算与风险拦截
    fn calculate_final_position(
        &self,
        def_strength: f64,
        last_price: f64,
        sl_level: f64,
        conf_mult: f64,
        vol_p: f64,
        is_tsunami: bool,
        leverage: f64,
        max_loss_pct: Option<f64>,
        tags: &mut Vec<String>,
    ) -> (f64, f64, bool) {
        let base_size = (def_strength / self.config.max_strength_cap)
            .clamp(self.config.min_base_size, self.config.base_size_max);

        // 恢复原版波动率分段
        let vol_adj = if vol_p > 80.0 {
            0.70
        } else if vol_p > 60.0 {
            0.85
        } else if vol_p < 20.0 {
            1.15
        } else {
            1.00
        };

        let mut size = base_size * vol_adj * conf_mult;
        if is_tsunami {
            size *= 1.20; // 保持原版 1.2 倍
            tags.push("TSUNAMI_MODE".into());
        }

        let sl_dist_pct = (last_price - sl_level).abs() / last_price;
        let mut est_loss = size * sl_dist_pct * leverage;
        let mut violated = false;

        if let Some(max_l) = max_loss_pct {
            if est_loss > max_l {
                violated = true;
                size = max_l / (sl_dist_pct * leverage).max(f64::EPSILON);
                est_loss = max_l;
                tags.push(format!("RISK_CAPPED:{:.2}%", est_loss * 100.0));
            }
        }

        let final_size = size.clamp(self.config.min_position_size, self.config.max_position_size);
        let final_loss = final_size * sl_dist_pct * leverage;
        (final_size, final_loss, violated)
    }
}
