use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{PriceGravityWell, TrendStructure, WellSide};
use serde_json::json;

// ================= 安全审计常量 (优化版) =================
const RR_MIN_ACCEPTABLE: f64 = 1.5;
const MA20_EXTREME_MULT: f64 = 3.8;
const ATR_PROTECTION_MULT: f64 = 1.5; // 用于 SL 的微调缓冲
const BREAKOUT_CONFIRM_GATE: f64 = 0.0025; // 0.25% 贴身肉搏判定
const VACUUM_THRESHOLD_BASE: f64 = 0.015;
const WEAR_ATTENUATOR_LIMIT: f64 = 0.4; // 阻力被撞烂后，其压制力最多衰减到 40%

pub struct RiskAuditAnalyzer;

impl Analyzer for RiskAuditAnalyzer {
    fn name(&self) -> &'static str {
        "risk_audit_pro_v2"
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::RiskManagement
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        let last_price = ctx.global.last_price;

        // 1. 获取增强后的引力井数据与市场环境
        let wells = ctx
            .get_cached::<Vec<PriceGravityWell>>(ContextKey::SpaceGravityWells)
            .unwrap_or_default();
        let atr_v = ctx
            .get_cached::<f64>(ContextKey::VolAtrValue) // 使用绝对 ATR 值
            .unwrap_or(last_price * 0.005);
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .unwrap_or(50.0);
        let regime = ctx
            .get_cached::<TrendStructure>(ContextKey::RegimeStructure)
            .unwrap_or(TrendStructure::Range);

        let micro_taker = ctx
            .get_role(Role::Entry)?
            .taker_flow
            .taker_buy_ratio
            .unwrap_or(0.5);

        // 2. 识别核心锚点：最近的支撑与阻力
        // 阻力：价格上方最近的井
        let min_res_well = wells
            .iter()
            .filter(|w| w.side == WellSide::Resistance && w.is_active)
            .min_by(|a, b| a.distance_pct.partial_cmp(&b.distance_pct).unwrap());

        // 支撑：价格下方最近的井
        let min_sup_well = wells
            .iter()
            .filter(|w| w.side == WellSide::Support && w.is_active)
            .max_by(|a, b| a.level.partial_cmp(&b.level).unwrap());

        // 3. 动态止损与止盈逻辑 (结合磨损修正)
        // 止损位：最近支撑位下方减去一小段 ATR 缓冲
        let sl_price = min_sup_well
            .map(|w| w.level - (atr_v * ATR_PROTECTION_MULT))
            .unwrap_or(last_price * 0.98);

        // 止盈位：根据阻力位强度决定是“阻力前平仓”还是“突破后预期”
        let mut tp_price = min_res_well.map(|w| w.level).unwrap_or(last_price * 1.05);

        // 4. 多维风险审计状态
        let mut confidence_mult = 1.0;
        let mut audit_tags = Vec::new();

        // --- A. 磨损博弈审计 (新增强化逻辑) ---
        if let Some(res) = min_res_well {
            // 如果阻力位已经被撞击超过 3 次，且最近一次撞击就在附近
            if res.hit_count >= 3 {
                let wear_factor = (1.0 / (res.hit_count as f64 * 0.5)).max(WEAR_ATTENUATOR_LIMIT);

                // 逻辑：阻力越烂，我们对“突破”越有信心
                if micro_taker > 0.55 {
                    confidence_mult *= 1.0 + (1.0 - wear_factor);
                    audit_tags.push("RES_WEAKENED");
                    // 止盈可以设得稍微穿过阻力位一点，赌它守不住
                    tp_price *= 1.002;
                }
            } else if res.distance_pct < BREAKOUT_CONFIRM_GATE && micro_taker < 0.48 {
                // 阻力很硬且买盘弱，离阻力太近是高危，大幅扣分
                confidence_mult *= 0.4;
                audit_tags.push("RES_WALL_NEAR");
            }
        }

        // --- B. 盈亏比审查 ---
        let risk = (last_price - sl_price).abs().max(last_price * 0.001);
        let reward = (tp_price - last_price).abs();
        let rr = reward / risk;

        if rr < RR_MIN_ACCEPTABLE {
            confidence_mult *= 0.2; // 盈亏比不及格，直接打骨折
            audit_tags.push("POOR_RR");
        }

        // --- C. 真空区与趋势共振 ---
        let dynamic_vacuum_gate = (VACUUM_THRESHOLD_BASE * (1.0 + vol_p / 100.0)).clamp(0.01, 0.04);
        let is_up_vacuum = min_res_well.map_or(true, |w| w.distance_pct > dynamic_vacuum_gate);

        if is_up_vacuum
            && matches!(
                regime,
                TrendStructure::StrongBullish | TrendStructure::Bullish
            )
        {
            confidence_mult *= 1.2;
            audit_tags.push("BLUE_SKY_RAIL"); // 上方真空，趋势加速预期
        }

        // --- D. 乖离熔断 (基于 FeatureSet 空间数据) ---
        if let Some(ma_dist) = ctx.get_role(Role::Trend)?.feature_set.space.ma20_dist_ratio {
            // 离均线太远说明处于超买/超卖末端，防止追高
            let extreme_threshold = (atr_v / last_price) * MA20_EXTREME_MULT;
            if ma_dist > extreme_threshold {
                confidence_mult *= 0.3;
                audit_tags.push("OVEREXTENDED_LONG");
            } else if ma_dist < -extreme_threshold {
                confidence_mult *= 0.3;
                audit_tags.push("OVEREXTENDED_SHORT");
            }
        }

        // --- E. 极端波动保护 ---
        if vol_p > 97.0 {
            confidence_mult *= 0.5;
            audit_tags.push("HIGH_VOL_CAUTION");
        }

        // 5. 写回核心交易决策参数
        ctx.set_cached(ContextKey::CurrentStopLoss, sl_price);
        ctx.set_cached(ContextKey::CurrentTakeProfit, tp_price);
        ctx.set_cached(ContextKey::FinalRiskMult, confidence_mult);

        Ok(AnalysisResult::new(self.kind(), "RISK_PRO_V2".into())
            .with_mult(confidence_mult)
            .because(audit_tags.join(" | "))
            .debug(json!({
                "rr": format!("{:.2}", rr),
                "sl": sl_price,
                "tp": tp_price,
                "res_hits": min_res_well.map(|w| w.hit_count).unwrap_or(0),
                "risk_mult": confidence_mult
            })))
    }
}
