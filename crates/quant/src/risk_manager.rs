use crate::types::{PriceGravityWell, TrendStructure, WellSide};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ================= 类型增强 =================
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TradeDirection {
    Long,
    Short,
    None,
}

impl From<&str> for TradeDirection {
    fn from(s: &str) -> Self {
        match s {
            "LONG" => Self::Long,
            "SHORT" => Self::Short,
            _ => Self::None,
        }
    }
}
impl TradeDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Long => "LONG",
            Self::Short => "SHORT",
            Self::None => "NONE",
        }
    }
}
// ================= 风控常量 =================
const RR_MIN_ACCEPTABLE: f64 = 2.5;
const MA20_EXTREME_MULT: f64 = 3.8;
const ATR_SL_BUFFERS: [f64; 3] = [1.0, 1.5, 2.0];
const BREAKOUT_CONFIRM_GATE: f64 = 0.0025;
const VACUUM_THRESHOLD_BASE: f64 = 0.015;
const WEAR_ATTENUATOR_LIMIT: f64 = 0.4;
const MAX_STRENGTH_CAP: f64 = 3.5;

pub const TP_RATIOS: [f64; 3] = [0.3, 0.4, 0.3];

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RiskAssessment {
    pub direction: TradeDirection,
    pub position_size_pct: f64,
    pub stop_loss_levels: Vec<f64>,
    pub take_profit_levels: Vec<f64>,
    pub weighted_rr: f64,
    pub rr_levels: [f64; 3],
    pub confidence_mult: f64,
    pub audit_tags: Vec<String>,
    pub allocation: [f64; 3],
}

pub struct RiskManager;

impl RiskManager {
    pub fn assess(
        direction: TradeDirection,
        wells: &[PriceGravityWell],
        last_price: f64,
        atr_ratio: f64,
        vol_p: f64,
        reg_time: TrendStructure,
        is_tsunami: bool,
        taker_ratio: f64,
        ma20_dist: Option<f64>,
    ) -> Option<RiskAssessment> {
        if direction == TradeDirection::None {
            return None;
        }

        let is_long = direction == TradeDirection::Long;
        let atr_v = atr_ratio * last_price;
        let mut audit_tags = Vec::with_capacity(8);

        // 1. 预过滤并分类井位 (一次迭代完成)
        let (mut upper_wells, mut lower_wells): (Vec<_>, Vec<_>) = wells
            .iter()
            .filter(|w| w.is_active)
            .partition(|w| w.level > last_price);

        // 升序排压力位，降序排支撑位
        upper_wells.sort_by(|a, b| a.level.partial_cmp(&b.level).unwrap());
        lower_wells.sort_by(|a, b| b.level.partial_cmp(&a.level).unwrap());

        // 2. 确定锚点井与趋势兼容性
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

        // 3. 计算止损 (SL)
        // 逻辑：以最近的防守井为基准，结合 ATR 缓冲区
        let base_def = defense_wells
            .first()
            .map(|w| w.level)
            .unwrap_or(if is_long {
                last_price * 0.98
            } else {
                last_price * 1.02
            });

        let sl_levels = ATR_SL_BUFFERS
            .iter()
            .map(|&buf| {
                if is_long {
                    (base_def - atr_v * buf).max(last_price * (0.99 - (buf * 0.01)))
                } else {
                    (base_def + atr_v * buf).min(last_price * (1.01 + (buf * 0.01)))
                }
            })
            .collect::<Vec<_>>();

        // 4. 计算止盈 (TP)
        let tp1 = primary_targets
            .get(0)
            .map(|w| w.level)
            .unwrap_or(if is_long {
                last_price * 1.02
            } else {
                last_price * 0.98
            });
        let tp2 = primary_targets
            .get(1)
            .map(|w| w.level)
            .unwrap_or(if is_long {
                last_price * 1.05
            } else {
                last_price * 0.95
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
            last_price + atr_v * 4.0
        } else {
            last_price - atr_v * 4.0
        });

        let tp_levels = vec![tp1, tp2, tp3];

        // 5. 信心系数与审计
        let mut confidence_mult = 1.0;

        // 目标井磨损审计
        if let Some(target) = primary_targets.first() {
            if target.hit_count >= 3 {
                let wear = (1.0 / (target.hit_count as f64 * 0.5)).max(WEAR_ATTENUATOR_LIMIT);
                let taker_favorable = if is_long {
                    taker_ratio > 0.55
                } else {
                    taker_ratio < 0.45
                };
                if taker_favorable {
                    confidence_mult *= 1.0 + (1.0 - wear);
                    audit_tags.push("TARGET_WEAKENED".to_string());
                }
            }
            if target.distance_pct.abs() < BREAKOUT_CONFIRM_GATE {
                let taker_blocked = if is_long {
                    taker_ratio < 0.48
                } else {
                    taker_ratio > 0.52
                };
                if taker_blocked {
                    confidence_mult *= 0.4;
                    audit_tags.push("WALL_REJECTION_RISK".to_string());
                }
            }
        }

        // 6. 盈亏比评估 (RR)
        let risk = (last_price - sl_levels[1]).abs().max(last_price * 0.0005);
        let rewards = tp_levels
            .iter()
            .map(|tp| (tp - last_price).abs())
            .collect::<Vec<_>>();
        let rr_levels = [rewards[0] / risk, rewards[1] / risk, rewards[2] / risk];

        let weighted_reward = rewards
            .iter()
            .enumerate()
            .map(|(i, r)| r * TP_RATIOS[i])
            .sum::<f64>();
        let weighted_rr = weighted_reward / risk;

        if weighted_rr < RR_MIN_ACCEPTABLE {
            confidence_mult *= 0.5;
            audit_tags.push(format!("LOW_RR:{:.1}", weighted_rr));
        } else {
            audit_tags.push(format!("RR_OK:{:.1}", weighted_rr));
        }

        // 7. 真空区与乖离审计
        let dynamic_gate = (VACUUM_THRESHOLD_BASE * (1.0 + vol_p / 100.0)).clamp(0.01, 0.04);
        if primary_targets
            .first()
            .map_or(true, |w| w.distance_pct.abs() > dynamic_gate)
            && trend_ok
        {
            confidence_mult *= 1.2;
            audit_tags.push("VACUUM_ACCEL".to_string());
        }

        if let Some(dist) = ma20_dist {
            let limit = atr_ratio * MA20_EXTREME_MULT;
            if (is_long && dist > limit) || (!is_long && dist < -limit) {
                confidence_mult *= 0.3;
                audit_tags.push("OVEREXTENDED".to_string());
            }
        }

        // 8. 仓位定型
        let pos_base = defense_wells
            .first()
            .map(|w| (w.strength / MAX_STRENGTH_CAP).clamp(0.4, 1.0))
            .unwrap_or(0.5);

        let mut final_size = pos_base * confidence_mult;
        if is_tsunami {
            final_size *= 1.2;
        }

        Some(RiskAssessment {
            direction,
            position_size_pct: final_size.clamp(0.0, 1.0),
            stop_loss_levels: sl_levels,
            take_profit_levels: tp_levels,
            weighted_rr,
            rr_levels,
            confidence_mult,
            audit_tags,
            allocation: TP_RATIOS,
        })
    }
}
