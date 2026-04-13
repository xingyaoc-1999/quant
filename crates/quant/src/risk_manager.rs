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

// ================= 风控常量 =================

const RR_MIN_ACCEPTABLE: f64 = 2.2;
const MA20_EXTREME_MULT: f64 = 3.5;
const MA20_PENALTY_COEFF: f64 = 0.10;
const ATR_SL_BUFFERS: [f64; 3] = [0.8, 1.5, 2.2];
const MIN_SL_ATR_MULT: f64 = 0.5;
const BREAKOUT_CONFIRM_GATE: f64 = 0.0020;
const MAX_STRENGTH_CAP: f64 = 3.5;

const BASE_SIZE_MAX: f64 = 0.8;
const MIN_BASE_SIZE: f64 = 0.15;
const CONFIDENCE_PRIOR: f64 = 0.5; // 贝叶斯先验概率
const MIN_POSITION_SIZE: f64 = 0.05;
const MAX_POSITION_SIZE: f64 = 1.0;

const MULT_MIN: f64 = 0.4;
const MULT_MAX: f64 = 1.6;

const TSUNAMI_ALLOCATION: [f64; 3] = [0.2, 0.3, 0.5];

// ================= 贝叶斯似然比定义 =================

/// 趋势结构因子的似然比（方向正确时）
const LR_TREND_STRONG: f64 = 2.5; // 强趋势对齐：显著提升
const LR_TREND_WEAK: f64 = 0.6; // 趋势弱/逆势：适当降低

/// Taker 主动流向对齐时的似然比
const LR_TAKER_ALIGNED: f64 = 1.8;
const LR_TAKER_MISMATCH: f64 = 0.7;

/// 均线乖离惩罚（按超出比例分段）
fn ma_overextend_lr(exceed_ratio: f64) -> f64 {
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

/// 成交量分位数对置信度的影响
fn volume_percentile_lr(vol_p: f64) -> f64 {
    if vol_p > 70.0 {
        1.25 // 高波动，趋势延续性强
    } else if vol_p < 20.0 {
        0.80 // 低波动，缺乏动能
    } else {
        1.0
    }
}

/// 盈亏比质量因子
fn rr_quality_lr(weighted_rr: f64) -> f64 {
    if weighted_rr >= RR_MIN_ACCEPTABLE * 1.2 {
        1.4
    } else if weighted_rr >= RR_MIN_ACCEPTABLE {
        1.15
    } else if weighted_rr >= 1.5 {
        0.9
    } else {
        0.6
    }
}

/// 临近强阻力/支撑墙
fn wall_proximity_lr(distance_pct: f64, vol_p: f64) -> f64 {
    if distance_pct.abs() < BREAKOUT_CONFIRM_GATE {
        if vol_p >= 60.0 {
            1.2 // 高波动突破蓄力
        } else {
            0.8 // 低波动遇阻
        }
    } else {
        1.0
    }
}

/// 净得分因子（将 net_score 映射为似然比）
fn net_score_lr(net_score: f64) -> f64 {
    let norm = (net_score / 100.0).clamp(-1.0, 1.0);
    // 映射到 [0.6, 1.8] 区间
    1.0 + norm * 0.6
}

/// 海啸模式似然比提升
const LR_TSUNAMI: f64 = 1.5;

// ================= 风险输出结构 =================

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RiskAssessment {
    pub direction: TradeDirection,
    pub position_size_pct: f64,
    pub stop_loss_levels: Vec<f64>,
    pub take_profit_levels: Vec<f64>,
    pub weighted_rr: f64,
    pub rr_levels: [f64; 3],
    pub confidence: f64,      // 贝叶斯后验概率
    pub confidence_mult: f64, // 由后验概率映射的仓位乘数
    pub audit_tags: Vec<String>,
    pub allocation: [f64; 3],
    pub is_tsunami: bool,
}

pub struct RiskManager;

impl RiskManager {
    /// 贝叶斯融合核心函数：输入先验概率与似然比列表，返回后验概率
    fn bayesian_update(prior: f64, likelihoods: &[f64]) -> f64 {
        if likelihoods.is_empty() {
            return prior;
        }

        // 先验几率 = p / (1-p)
        let mut log_odds = (prior / (1.0 - prior)).ln();

        for &lr in likelihoods {
            // LR 应保证为正数，钳位避免极端值
            let safe_lr = lr.clamp(0.1, 10.0);
            log_odds += safe_lr.ln();
        }

        // 后验概率 = 1 / (1 + exp(-log_odds))
        let posterior = 1.0 / (1.0 + (-log_odds).exp());

        // 钳位到 [0.05, 0.95]，避免绝对化
        posterior.clamp(0.05, 0.95)
    }

    /// 核心风控评估函数（贝叶斯融合版本）
    pub fn assess(
        direction: Option<TradeDirection>,
        wells: &[PriceGravityWell],
        last_price: f64,
        atr_ratio: f64,
        vol_p: f64,
        reg_time: TrendStructure,
        is_tsunami: bool,
        taker_ratio: f64,
        ma20_dist: Option<f64>,
        net_score: f64,
    ) -> Option<RiskAssessment> {
        // 1. 方向预处理
        let direction = direction?;
        let is_long = direction == TradeDirection::Long;
        let atr_v = atr_ratio * last_price;
        let mut audit_tags = Vec::with_capacity(12);

        // 2. 井位过滤与分类
        let (mut upper_wells, mut lower_wells): (Vec<_>, Vec<_>) = wells
            .iter()
            .filter(|w| w.is_active)
            .partition(|w| w.level > last_price);

        upper_wells.sort_unstable_by(|a, b| a.level.partial_cmp(&b.level).unwrap());
        lower_wells.sort_unstable_by(|a, b| b.level.partial_cmp(&a.level).unwrap());

        let (primary_targets, defense_wells, trend_ok) = if is_long {
            let ok = matches!(
                reg_time,
                TrendStructure::StrongBullish | TrendStructure::Bullish
            );
            (upper_wells, lower_wells, ok)
        } else {
            let ok = matches!(
                reg_time,
                TrendStructure::StrongBearish | TrendStructure::Bearish
            );
            (lower_wells, upper_wells, ok)
        };

        // 3. 计算止损位 (SL)
        let base_def = defense_wells.first().map(|w| w.level).unwrap_or_else(|| {
            if is_long {
                last_price * 0.985
            } else {
                last_price * 1.015
            }
        });

        let min_sl_distance = atr_v * MIN_SL_ATR_MULT;
        let sl_levels = ATR_SL_BUFFERS
            .iter()
            .map(|&buf| {
                let raw_sl = if is_long {
                    (base_def - atr_v * buf).max(last_price * 0.95)
                } else {
                    (base_def + atr_v * buf).min(last_price * 1.05)
                };
                if is_long {
                    raw_sl.min(last_price - min_sl_distance)
                } else {
                    raw_sl.max(last_price + min_sl_distance)
                }
            })
            .collect::<Vec<_>>();

        // 4. 计算止盈位 (TP)
        let tp1 = primary_targets
            .first()
            .map(|w| w.level)
            .unwrap_or(if is_long {
                last_price * 1.015
            } else {
                last_price * 0.985
            });
        let tp2 = primary_targets
            .get(1)
            .map(|w| w.level)
            .unwrap_or(if is_long {
                last_price * 1.035
            } else {
                last_price * 0.965
            });
        let tp3 = if is_tsunami {
            primary_targets
                .iter()
                .filter(|w| w.side == WellSide::Magnet)
                .last()
                .map(|w| w.level)
        } else {
            primary_targets.get(2).map(|w| w.level)
        }
        .unwrap_or(if is_long {
            last_price + atr_v * 5.0
        } else {
            last_price - atr_v * 5.0
        });

        let (tp1, tp2, tp3) = if is_long {
            (
                tp1.max(last_price * 1.001),
                tp2.max(last_price * 1.001),
                tp3.max(last_price * 1.001),
            )
        } else {
            (
                tp1.min(last_price * 0.999),
                tp2.min(last_price * 0.999),
                tp3.min(last_price * 0.999),
            )
        };
        let tp_levels = vec![tp1, tp2, tp3];

        // 5. 动态止盈分配（井位强度平方归一化）
        let mut target_strengths = vec![0.5; 3];
        for i in 0..3 {
            if let Some(well) = primary_targets.get(i) {
                target_strengths[i] = well.strength.clamp(0.2, 3.0);
            }
        }
        let squared: Vec<f64> = target_strengths.iter().map(|&s| s * s).collect();
        let sum_sq: f64 = squared.iter().sum();
        let mut allocation: [f64; 3] = if sum_sq > 0.0 {
            [
                squared[0] / sum_sq,
                squared[1] / sum_sq,
                squared[2] / sum_sq,
            ]
        } else {
            [1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0]
        };

        if is_tsunami {
            allocation = TSUNAMI_ALLOCATION;
            audit_tags.push("TSUNAMI_ALLOC".to_string());
        }

        // 6. 计算加权盈亏比 (RR)
        let risks: Vec<f64> = sl_levels
            .iter()
            .map(|sl| (last_price - sl).abs().max(last_price * 0.0005))
            .collect();
        let rewards: Vec<f64> = tp_levels.iter().map(|tp| (tp - last_price).abs()).collect();

        let weighted_risk = risks
            .iter()
            .enumerate()
            .map(|(i, r)| r * allocation[i])
            .sum::<f64>();

        let weighted_reward = rewards
            .iter()
            .enumerate()
            .map(|(i, r)| r * allocation[i])
            .sum::<f64>();

        let weighted_rr = if weighted_risk > f64::EPSILON {
            weighted_reward / weighted_risk
        } else {
            0.0
        };

        let rr_levels = [
            rewards[0] / risks[0],
            rewards[1] / risks[1],
            rewards[2] / risks[2],
        ];

        // ================= 贝叶斯置信度融合 =================
        let mut likelihoods: Vec<f64> = Vec::new();

        // 1) 趋势结构
        if trend_ok {
            likelihoods.push(LR_TREND_STRONG);
            audit_tags.push("TREND_OK".to_string());
        } else {
            likelihoods.push(LR_TREND_WEAK);
            audit_tags.push("TREND_WEAK".to_string());
        }

        // 2) Taker 主动流向
        let taker_threshold = 0.52 - ((vol_p - 50.0) / 200.0).clamp(-0.05, 0.05);
        let taker_aligned = (is_long && taker_ratio > taker_threshold)
            || (!is_long && taker_ratio < (1.0 - taker_threshold));
        if taker_aligned {
            likelihoods.push(LR_TAKER_ALIGNED);
            audit_tags.push("TAKER_FLOW_OK".to_string());
        } else {
            likelihoods.push(LR_TAKER_MISMATCH);
            audit_tags.push("TAKER_FLOW_MISMATCH".to_string());
        }

        // 3) 均线乖离惩罚
        if let Some(dist) = ma20_dist {
            let limit = atr_ratio * MA20_EXTREME_MULT;
            let exceed_ratio = (dist.abs() / limit).min(2.0);
            let lr = ma_overextend_lr(exceed_ratio);
            likelihoods.push(lr);
            if exceed_ratio > 0.5 {
                audit_tags.push(format!("MA_OVEREXTEND:{:.1}%", exceed_ratio * 100.0));
            }
        }

        // 4) 成交量分位数
        let lr_vol = volume_percentile_lr(vol_p);
        likelihoods.push(lr_vol);
        if vol_p > 70.0 {
            audit_tags.push("HIGH_VOL".to_string());
        } else if vol_p < 20.0 {
            audit_tags.push("LOW_VOL".to_string());
        }

        // 5) 盈亏比质量
        let lr_rr = rr_quality_lr(weighted_rr);
        likelihoods.push(lr_rr);
        if weighted_rr >= RR_MIN_ACCEPTABLE {
            audit_tags.push(format!("RR_OK:{:.1}", weighted_rr));
        } else {
            audit_tags.push(format!("RR_LOW:{:.1}", weighted_rr));
        }

        // 6) 临近强阻力/支撑墙
        if let Some(target) = primary_targets.first() {
            let lr_wall = wall_proximity_lr(target.distance_pct, vol_p);
            likelihoods.push(lr_wall);
            if target.distance_pct.abs() < BREAKOUT_CONFIRM_GATE {
                if vol_p < 60.0 {
                    audit_tags.push("WALL_NEAR".to_string());
                } else {
                    audit_tags.push("BREAKOUT_READY".to_string());
                }
            }
        }

        // 7) 净得分因子
        let lr_net = net_score_lr(net_score);
        likelihoods.push(lr_net);

        // 8) 海啸模式
        if is_tsunami {
            likelihoods.push(LR_TSUNAMI);
        }

        // 贝叶斯融合
        let posterior = Self::bayesian_update(CONFIDENCE_PRIOR, &likelihoods);
        audit_tags.push(format!("BAYES_CONF:{:.2}", posterior));

        // ================= 综合乘数计算（收窄范围） =================
        // 将后验概率映射到乘数区间 [0.4, 1.6]
        let confidence_mult = (posterior * 2.4 - 0.4).clamp(MULT_MIN, MULT_MAX);

        // ================= 仓位计算 =================
        let base_size = defense_wells
            .first()
            .map(|w| (w.strength / MAX_STRENGTH_CAP).clamp(MIN_BASE_SIZE, BASE_SIZE_MAX))
            .unwrap_or(0.4);

        let vol_adj = if vol_p > 80.0 {
            0.70
        } else if vol_p > 60.0 {
            0.85
        } else if vol_p < 20.0 {
            1.15
        } else {
            1.00
        };

        let mut final_size = base_size * vol_adj * confidence_mult;

        if is_tsunami {
            final_size *= 1.20;
            audit_tags.push("TSUNAMI_MODE".to_string());
        }

        final_size = final_size.clamp(MIN_POSITION_SIZE, MAX_POSITION_SIZE);

        // ================= 输出 =================
        Some(RiskAssessment {
            direction,
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
        })
    }
}
