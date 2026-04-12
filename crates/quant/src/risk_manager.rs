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
const MA20_EXTREME_MULT: f64 = 3.5; // 均线乖离限制系数
const ATR_SL_BUFFERS: [f64; 3] = [0.8, 1.5, 2.2]; // 止损阶梯
const BREAKOUT_CONFIRM_GATE: f64 = 0.0020; // 突破确认阈值 (0.2%)
const MAX_STRENGTH_CAP: f64 = 3.5; // 井位强度上限基准

// 仓位管理常量
const BASE_SIZE_MAX: f64 = 0.8; // 基础仓位上限
const MIN_BASE_SIZE: f64 = 0.15; // 基础仓位下限（原 0.3 降低，更保守）
const CONFIDENCE_BASE: f64 = 0.5; // 置信度初始基准
const MIN_POSITION_SIZE: f64 = 0.05; // 最低允许仓位 (5%)
const MAX_POSITION_SIZE: f64 = 1.0; // 最高仓位 (100%)

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RiskAssessment {
    pub direction: TradeDirection,
    pub position_size_pct: f64,
    pub stop_loss_levels: Vec<f64>,
    pub take_profit_levels: Vec<f64>,
    pub weighted_rr: f64,
    pub rr_levels: [f64; 3],
    pub confidence_mult: f64, // 综合乘数（0.2~2.0），融合内部置信度和信号总分
    pub audit_tags: Vec<String>,
    pub allocation: [f64; 3], // 动态止盈分配比例
    pub is_tsunami: bool,
}

pub struct RiskManager;

impl RiskManager {
    /// 核心风控评估函数（动态止盈分配 + net_score 融合）
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
        net_score: f64, // 分析器链最终总分（-100..100）
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

        let sl_levels = ATR_SL_BUFFERS
            .iter()
            .map(|&buf| {
                if is_long {
                    (base_def - atr_v * buf).max(last_price * 0.95)
                } else {
                    (base_def + atr_v * buf).min(last_price * 1.05)
                }
            })
            .collect::<Vec<_>>();

        // 4. 计算止盈位 (TP) —— 保留原有逻辑
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

        let tp_levels = vec![tp1, tp2, tp3];

        // ================= 动态止盈分配（根据目标井强度平方归一化） =================
        // 获取三个目标井的强度（若不足则使用默认强度 0.5）
        let mut target_strengths = vec![0.5; 3];
        for i in 0..3 {
            if let Some(well) = primary_targets.get(i) {
                target_strengths[i] = well.strength.clamp(0.2, 3.0);
            }
        }
        // 平方放大差异
        let squared: Vec<f64> = target_strengths.iter().map(|&s| s * s).collect();
        let sum_sq: f64 = squared.iter().sum();
        let allocation: [f64; 3] = if sum_sq > 0.0 {
            [
                squared[0] / sum_sq,
                squared[1] / sum_sq,
                squared[2] / sum_sq,
            ]
        } else {
            [1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0] // 均匀分配
        };

        // 5. 计算加权盈亏比 (RR) —— 使用动态 allocation
        let risk_base = (last_price - sl_levels[0]).abs().max(last_price * 0.0005);
        let rewards = tp_levels
            .iter()
            .map(|tp| (tp - last_price).abs())
            .collect::<Vec<_>>();
        let rr_levels = [
            rewards[0] / risk_base,
            rewards[1] / risk_base,
            rewards[2] / risk_base,
        ];

        let weighted_reward = rewards
            .iter()
            .enumerate()
            .map(|(i, r)| r * allocation[i])
            .sum::<f64>();
        let weighted_rr = weighted_reward / risk_base;

        // ================= 置信度分数计算（加性，0~1 区间） =================
        let mut conf_score = CONFIDENCE_BASE;

        // 1) 趋势结构
        if trend_ok {
            conf_score += 0.15;
            audit_tags.push("TREND_OK".to_string());
        } else {
            conf_score -= 0.10;
            audit_tags.push("TREND_WEAK".to_string());
        }

        // 2) Taker 主动流向（动态阈值，根据波动率调整）
        let taker_threshold = 0.52 - ((vol_p - 50.0) / 200.0).clamp(-0.05, 0.05);
        let taker_aligned = (is_long && taker_ratio > taker_threshold)
            || (!is_long && taker_ratio < (1.0 - taker_threshold));
        if taker_aligned {
            conf_score += 0.12;
            audit_tags.push("TAKER_FLOW_OK".to_string());
        } else {
            conf_score -= 0.06;
            audit_tags.push("TAKER_FLOW_MISMATCH".to_string());
        }

        // 3) 均线乖离惩罚
        if let Some(dist) = ma20_dist {
            let limit = atr_ratio * MA20_EXTREME_MULT;
            let exceed_ratio = (dist.abs() / limit).min(2.0);
            let penalty = 0.15 * exceed_ratio;
            conf_score -= penalty;
            if penalty > 0.05 {
                audit_tags.push(format!("MA_OVEREXTEND:{:.1}%", exceed_ratio * 100.0));
            }
        }

        // 4) 成交量分位数加成
        let vol_bonus = ((vol_p - 50.0) / 50.0).clamp(-0.15, 0.15);
        conf_score += vol_bonus;
        if vol_p > 70.0 {
            audit_tags.push("HIGH_VOL".to_string());
        } else if vol_p < 20.0 {
            audit_tags.push("LOW_VOL".to_string());
        }

        // 5) 盈亏比质量因子
        let rr_deviation = weighted_rr / RR_MIN_ACCEPTABLE - 1.0;
        let rr_quality = rr_deviation.tanh().clamp(-0.20, 0.20);
        conf_score += rr_quality;
        if weighted_rr >= RR_MIN_ACCEPTABLE {
            audit_tags.push(format!("RR_OK:{:.1}", weighted_rr));
        } else {
            audit_tags.push(format!("RR_LOW:{:.1}", weighted_rr));
        }

        // 6) 临近强阻力/支撑处理
        if let Some(target) = primary_targets.first() {
            if target.distance_pct.abs() < BREAKOUT_CONFIRM_GATE {
                if vol_p < 60.0 {
                    conf_score -= 0.10;
                    audit_tags.push("WALL_NEAR".to_string());
                } else {
                    conf_score += 0.05;
                    audit_tags.push("BREAKOUT_READY".to_string());
                }
            }
        }

        let normalized_net = (net_score / 100.0).clamp(-1.0, 1.0);
        conf_score += normalized_net * 0.15;

        // 最终置信度钳位 (0.05 ~ 0.95)
        conf_score = conf_score.clamp(0.05, 0.95);
        audit_tags.push(format!("CONF_SCORE:{:.2}", conf_score));

        // ================= 综合乘数计算 =================
        let base_mult = (conf_score * 2.0).clamp(0.2, 1.9);
        let net_mult = 1.0 + normalized_net * 0.5;
        let mut total_mult = base_mult * net_mult;
        total_mult = total_mult.clamp(0.2, 2.0);

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

        let mut final_size = base_size * vol_adj * total_mult;

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
            confidence_mult: total_mult,
            audit_tags,
            allocation,
            is_tsunami,
        })
    }
}
