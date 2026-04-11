use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{PriceGravityWell, TrendStructure, WellSide};
use serde_json::json;

// ================= 风控常量 =================
const RR_MIN_ACCEPTABLE: f64 = 2.5; // 最低盈亏比要求
const MA20_EXTREME_MULT: f64 = 3.8; // 乖离率阈值（ATR 倍数）
const ATR_SL_BUFFER: f64 = 1.2; // 止损缓冲（ATR 倍数）
const BREAKOUT_CONFIRM_GATE: f64 = 0.0025; // 0.25% 贴身肉搏判定
const VACUUM_THRESHOLD_BASE: f64 = 0.015; // 真空区基础阈值
const WEAR_ATTENUATOR_LIMIT: f64 = 0.4; // 阻力磨损后压制力最低保留比例
const MAX_LEVERAGE_DEFAULT: f64 = 5.0; // 默认杠杆倍数
const LIQUIDATION_BUFFER_PCT: f64 = 0.005; // 强平缓冲 0.5%

pub struct RiskAuditAnalyzer {
    /// 用户设置的杠杆倍数
    pub leverage: f64,
}

impl Default for RiskAuditAnalyzer {
    fn default() -> Self {
        Self {
            leverage: MAX_LEVERAGE_DEFAULT,
        }
    }
}

impl RiskAuditAnalyzer {
    pub fn with_leverage(mut self, lev: f64) -> Self {
        self.leverage = lev.clamp(1.0, 100.0);
        self
    }
}

impl Analyzer for RiskAuditAnalyzer {
    fn name(&self) -> &'static str {
        "risk_audit_bi_v1"
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::RiskManagement
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        let last_price = ctx.global.last_price;

        // 0. 获取交易方向（由外部写入）
        let direction = ctx
            .get_cached::<String>(ContextKey::SignalDirection)
            .unwrap_or_else(|| "NONE".to_string());
        if direction == "NONE" {
            return Ok(AnalysisResult::new(self.kind(), "RISK_BI_V1".into())
                .with_mult(1.0)
                .because("NO_DIRECTION"));
        }
        let is_long = direction == "LONG";

        // 1. 获取市场环境数据
        let wells = ctx
            .get_cached::<Vec<PriceGravityWell>>(ContextKey::SpaceGravityWells)
            .unwrap_or_default();

        let atr_v = ctx
            .get_cached::<f64>(ContextKey::VolAtrRatio)
            .map(|r| r * last_price)
            .unwrap_or(last_price * 0.005);

        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .unwrap_or(50.0);

        let regime = ctx
            .get_cached::<TrendStructure>(ContextKey::RegimeStructure)
            .unwrap_or(TrendStructure::Range);

        let is_tsunami = ctx
            .get_cached::<bool>(ContextKey::IsMomentumTsunami)
            .unwrap_or(false);

        let micro_taker = ctx
            .get_role(Role::Entry)?
            .taker_flow
            .taker_buy_ratio
            .unwrap_or(0.5);

        // 2. 根据方向选择锚点井
        let (primary_well, secondary_well, is_favorable_trend) = if is_long {
            // 多头：阻力是目标，支撑是止损基础
            let res_well = wells
                .iter()
                .filter(|w| {
                    (w.side == WellSide::Resistance || w.side == WellSide::Magnet)
                        && w.is_active
                        && w.level > last_price
                })
                .min_by(|a, b| a.distance_pct.partial_cmp(&b.distance_pct).unwrap());

            let sup_well = wells
                .iter()
                .filter(|w| {
                    (w.side == WellSide::Support || w.side == WellSide::Magnet)
                        && w.is_active
                        && w.level < last_price
                })
                .max_by(|a, b| a.level.partial_cmp(&b.level).unwrap());

            let trend_ok = matches!(
                regime,
                TrendStructure::StrongBullish | TrendStructure::Bullish
            );
            (res_well, sup_well, trend_ok)
        } else {
            // 空头：支撑是目标，阻力是止损基础
            let sup_well = wells
                .iter()
                .filter(|w| {
                    (w.side == WellSide::Support || w.side == WellSide::Magnet)
                        && w.is_active
                        && w.level < last_price
                })
                .min_by(|a, b| b.distance_pct.partial_cmp(&a.distance_pct).unwrap()); // 注意比较方向

            let res_well = wells
                .iter()
                .filter(|w| {
                    (w.side == WellSide::Resistance || w.side == WellSide::Magnet)
                        && w.is_active
                        && w.level > last_price
                })
                .max_by(|a, b| a.level.partial_cmp(&b.level).unwrap());

            let trend_ok = matches!(
                regime,
                TrendStructure::StrongBearish | TrendStructure::Bearish
            );
            (sup_well, res_well, trend_ok)
        };

        // 3. 动态止损与止盈计算
        let (sl_price, tp_price) = if is_long {
            let raw_sl = secondary_well
                .map(|w| w.level - atr_v * ATR_SL_BUFFER)
                .unwrap_or(last_price * 0.98);
            let est_liq = last_price * (1.0 - 1.0 / self.leverage + LIQUIDATION_BUFFER_PCT);
            let sl = raw_sl.max(est_liq);

            let mut tp = primary_well.map(|w| w.level).unwrap_or(last_price * 1.05);
            if is_tsunami {
                if let Some(far_magnet) = wells
                    .iter()
                    .filter(|w| w.side == WellSide::Magnet && w.level > last_price)
                    .max_by(|a, b| a.level.partial_cmp(&b.level).unwrap())
                {
                    tp = far_magnet.level;
                }
            }
            if (tp - last_price).abs() < atr_v * 0.5 {
                tp = last_price + atr_v * 1.5;
            }
            (sl, tp)
        } else {
            // 空头：止损在阻力上方，止盈在支撑下方
            let raw_sl = secondary_well
                .map(|w| w.level + atr_v * ATR_SL_BUFFER)
                .unwrap_or(last_price * 1.02);
            let est_liq = last_price * (1.0 + 1.0 / self.leverage - LIQUIDATION_BUFFER_PCT);
            let sl = raw_sl.min(est_liq);

            let mut tp = primary_well.map(|w| w.level).unwrap_or(last_price * 0.95);
            if is_tsunami {
                if let Some(far_magnet) = wells
                    .iter()
                    .filter(|w| w.side == WellSide::Magnet && w.level < last_price)
                    .min_by(|a, b| a.level.partial_cmp(&b.level).unwrap())
                {
                    tp = far_magnet.level;
                }
            }
            if (last_price - tp).abs() < atr_v * 0.5 {
                tp = last_price - atr_v * 1.5;
            }
            (sl, tp)
        };

        // 4. 风险审计乘数计算
        let mut confidence_mult = 1.0;
        let mut audit_tags = Vec::new();

        // --- A. 磨损博弈审计（针对目标井）---
        if let Some(target_well) = primary_well {
            if target_well.hit_count >= 3 {
                let wear_factor =
                    (1.0 / (target_well.hit_count as f64 * 0.5)).max(WEAR_ATTENUATOR_LIMIT);
                // 根据方向判断主动盘是否有利
                let taker_favorable = if is_long {
                    micro_taker > 0.55
                } else {
                    micro_taker < 0.45
                };
                if taker_favorable {
                    confidence_mult *= 1.0 + (1.0 - wear_factor);
                    audit_tags.push("TARGET_WEAKENED");
                }
            } else if target_well.distance_pct.abs() < BREAKOUT_CONFIRM_GATE {
                let taker_unfavorable = if is_long {
                    micro_taker < 0.48
                } else {
                    micro_taker > 0.52
                };
                if taker_unfavorable {
                    confidence_mult *= 0.4;
                    audit_tags.push("TARGET_WALL_NEAR");
                }
            }
        }

        // --- B. 盈亏比审查 ---
        let risk = if is_long {
            (last_price - sl_price).abs()
        } else {
            (sl_price - last_price).abs()
        };
        let reward = if is_long {
            (tp_price - last_price).abs()
        } else {
            (last_price - tp_price).abs()
        };
        let risk = risk.max(last_price * 0.001);
        let rr = reward / risk;

        if rr < RR_MIN_ACCEPTABLE {
            let penalty = if self.leverage > 10.0 { 0.1 } else { 0.3 };
            confidence_mult *= penalty;
            audit_tags.push(&format!("POOR_RR({:.2})", rr));
        } else {
            audit_tags.push(&format!("RR_OK({:.2})", rr));
        }

        // --- C. 杠杆风险检查 ---
        if is_long {
            let est_liq = last_price * (1.0 - 1.0 / self.leverage + LIQUIDATION_BUFFER_PCT);
            if sl_price <= est_liq {
                confidence_mult *= 0.2;
                audit_tags.push("SL_TOO_CLOSE_TO_LIQ");
            }
        } else {
            let est_liq = last_price * (1.0 + 1.0 / self.leverage - LIQUIDATION_BUFFER_PCT);
            if sl_price >= est_liq {
                confidence_mult *= 0.2;
                audit_tags.push("SL_TOO_CLOSE_TO_LIQ");
            }
        }
        if self.leverage > 20.0 {
            confidence_mult *= 0.7;
            audit_tags.push("HIGH_LEV_CAUTION");
        }

        // --- D. 真空区与趋势共振 ---
        let dynamic_vacuum_gate = (VACUUM_THRESHOLD_BASE * (1.0 + vol_p / 100.0)).clamp(0.01, 0.04);
        let is_vacuum = primary_well.map_or(true, |w| w.distance_pct.abs() > dynamic_vacuum_gate);
        if is_vacuum && is_favorable_trend {
            confidence_mult *= 1.2;
            audit_tags.push("BLUE_SKY_RAIL");
        }

        // --- E. 乖离熔断 ---
        if let Some(ma_dist) = ctx.get_role(Role::Trend)?.feature_set.space.ma20_dist_ratio {
            let extreme_threshold = (atr_v / last_price) * MA20_EXTREME_MULT;
            if is_long && ma_dist > extreme_threshold {
                confidence_mult *= 0.3;
                audit_tags.push("OVEREXTENDED_LONG");
            } else if !is_long && ma_dist < -extreme_threshold {
                confidence_mult *= 0.3;
                audit_tags.push("OVEREXTENDED_SHORT");
            }
        }

        // --- F. 极端波动保护 ---
        if vol_p > 97.0 {
            confidence_mult *= 0.5;
            audit_tags.push("HIGH_VOL_CAUTION");
        }

        // 5. 写回缓存
        ctx.set_cached(ContextKey::CurrentStopLoss, sl_price);
        ctx.set_cached(ContextKey::CurrentTakeProfit, tp_price);
        ctx.set_cached(ContextKey::FinalRiskMult, confidence_mult);
        ctx.set_cached(ContextKey::RecommendedLeverage, self.leverage);

        Ok(AnalysisResult::new(self.kind(), "RISK_BI_V1".into())
            .with_mult(confidence_mult)
            .because(audit_tags.join(" | "))
            .debug(json!({
                "direction": direction,
                "rr": format!("{:.2}", rr),
                "sl": sl_price,
                "tp": tp_price,
                "leverage": self.leverage,
                "target_hits": primary_well.map(|w| w.hit_count).unwrap_or(0),
                "risk_mult": confidence_mult
            })))
    }
}
