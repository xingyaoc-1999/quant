use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{PriceGravityWell, RsiState, VolumeState};
use serde_json::json;

// ========== 常量配置 ==========
/// 突破阈值（ATR倍数），超过此幅度才算有效刺破
const BREACH_ATR_MULT: f64 = 0.5;
/// 假突破判定：收盘价回到井内的距离阈值（相对于井水平）
const CLOSE_RETURN_THRESHOLD: f64 = 0.001; // 0.1%
/// 假突破基础扣分（乘以井强度）
const FAKEOUT_BASE_PENALTY: f64 = 25.0;
/// 假突破导致的多空乘数惩罚
const FAKEOUT_MULT_PENALTY: f64 = 0.6;
/// 轻微假突破的乘数（仅刺破但无明显量价背离）
const MINOR_FAKEOUT_MULT: f64 = 0.85;

/// MA20 斜率调节因子
const SLOPE_STRONG_THRESHOLD: f64 = 0.1;
const SLOPE_STRONG_FACTOR: f64 = 0.7;
const SLOPE_WEAK_FACTOR: f64 = 1.3;

/// RSI 极端区调节因子
const RSI_OVERBOUGHT_FACTOR: f64 = 1.2;
const RSI_OVERSOLD_FACTOR: f64 = 1.2;

/// 成交量效率调节因子
const VOL_EFF_LOW_THRESHOLD: f64 = 0.3; // 效率低于此值加重惩罚
const VOL_EFF_HIGH_THRESHOLD: f64 = 0.6; // 效率高于此值减轻惩罚
const VOL_SURGE_MULT: f64 = 1.5; // 相对成交量高于此值视为放量
const VOL_SHRINK_MULT: f64 = 0.7; // 相对成交量低于此值视为缩量

pub struct FakeoutDetector;

impl FakeoutDetector {
    /// 判断价格是否有效突破某个水平
    fn is_breach(high: f64, low: f64, level: f64, atr: f64, is_above: bool) -> bool {
        if is_above {
            high > level + atr * BREACH_ATR_MULT
        } else {
            low < level - atr * BREACH_ATR_MULT
        }
    }

    /// 判断收盘价是否收回到井附近
    fn is_closed_inside(close: f64, level: f64) -> bool {
        (close - level).abs() / level < CLOSE_RETURN_THRESHOLD
    }

    /// 根据效率、相对成交量、量状态计算额外惩罚系数（>1 加重，<1 减轻）
    fn volume_efficiency_penalty(
        efficiency: f64,
        rvol: f64,
        volume_state: Option<VolumeState>,
    ) -> f64 {
        let mut factor: f64 = 1.0;
        // 效率低且放量 → 典型假突破特征，加重惩罚
        if efficiency < VOL_EFF_LOW_THRESHOLD && rvol > VOL_SURGE_MULT {
            factor *= 1.4;
        }
        // 效率高且放量 → 可能是真突破，减轻惩罚
        else if efficiency > VOL_EFF_HIGH_THRESHOLD && rvol > VOL_SURGE_MULT {
            factor *= 0.7;
        }
        // 缩量背景下的假突破可信度稍低，略微减轻
        else if rvol < VOL_SHRINK_MULT {
            factor *= 0.9;
        }

        // 利用 FeatureSet 中的归一化量状态做二次确认
        match volume_state {
            Some(VolumeState::Expand) if efficiency < VOL_EFF_LOW_THRESHOLD => factor *= 1.2,
            Some(VolumeState::Shrink) => factor *= 0.9,
            _ => {}
        }
        factor.clamp(0.5, 2.0)
    }
}

impl Analyzer for FakeoutDetector {
    fn name(&self) -> &'static str {
        "fakeout_detector"
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::Fakeout
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        // 1. 获取趋势角色特征集
        let trend_role = ctx.get_role(Role::Trend)?;
        let fs = &trend_role.feature_set;
        let price_action = &fs.price_action;
        let current_high = price_action.high;
        let current_low = price_action.low;
        let current_close = price_action.close;

        // 2. 获取引力井、波动率数据
        let wells = ctx
            .get_cached::<Vec<PriceGravityWell>>(ContextKey::SpaceGravityWells)
            .unwrap_or_default();
        let atr_ratio = ctx
            .get_cached::<f64>(ContextKey::VolAtrRatio)
            .unwrap_or(0.005);
        let last_price = ctx.global.last_price;
        let atr = atr_ratio * last_price;

        if atr <= 0.0 || wells.is_empty() {
            return Ok(AnalysisResult::new(self.kind(), "FAKEOUT_V3".into())
                .with_score(0.0)
                .because("ATR 无效或无引力井，跳过检测"));
        }

        // 3. 获取调节因子
        let slope = fs.structure.ma20_slope.unwrap_or(0.0);
        let rsi_state = fs.structure.rsi_state.unwrap_or(RsiState::Neutral);

        // 从 VolumeStructureAnalyzer 缓存中读取效率与相对成交量
        let efficiency = ctx
            .get_cached::<f64>(ContextKey::LastEfficiency)
            .unwrap_or(0.5);
        let rvol = ctx.get_cached::<f64>(ContextKey::LastRVol).unwrap_or(1.0);
        let cached_vol_state = ctx
            .get_cached::<VolumeState>(ContextKey::VolumeState)
            .unwrap_or(VolumeState::Normal);

        let mut total_penalty = 0.0;
        let mut reasons = Vec::new();

        // 4. 遍历所有活跃的引力井（支撑和阻力均已包含）
        for well in wells.iter().filter(|w| w.is_active) {
            let level = well.level;
            let strength = well.strength.clamp(0.2, 3.0);

            // 检查当前 K 线是否向上突破阻力（假突破）
            let breach_up = Self::is_breach(current_high, current_low, level, atr, true);
            // 检查当前 K 线是否向下跌破支撑（假破位）
            let breach_down = Self::is_breach(current_high, current_low, level, atr, false);

            if !breach_up && !breach_down {
                continue;
            }

            // 判断收盘是否收回井内
            let closed_inside = Self::is_closed_inside(current_close, level);
            if !closed_inside {
                continue;
            }

            // 5. 计算基础惩罚
            let mut penalty = FAKEOUT_BASE_PENALTY * strength;

            // 6. 趋势斜率调节
            let mut adjustment = 1.0;
            if breach_up && slope > SLOPE_STRONG_THRESHOLD {
                adjustment = SLOPE_STRONG_FACTOR; // 上升趋势中的向上假突破减轻
            } else if breach_down && slope < -SLOPE_STRONG_THRESHOLD {
                adjustment = SLOPE_STRONG_FACTOR; // 下降趋势中的向下假破位减轻
            } else if breach_up && slope < -SLOPE_STRONG_THRESHOLD {
                adjustment = SLOPE_WEAK_FACTOR; // 下降趋势中的向上假突破加重
            } else if breach_down && slope > SLOPE_STRONG_THRESHOLD {
                adjustment = SLOPE_WEAK_FACTOR; // 上升趋势中的向下假破位加重
            }

            // 7. RSI 极端区调节
            if breach_up && matches!(rsi_state, RsiState::Overbought) {
                adjustment *= RSI_OVERBOUGHT_FACTOR;
            }
            if breach_down && matches!(rsi_state, RsiState::Oversold) {
                adjustment *= RSI_OVERSOLD_FACTOR;
            }

            // 8. 成交量与效率调节
            let vol_eff_factor =
                Self::volume_efficiency_penalty(efficiency, rvol, Some(cached_vol_state));
            adjustment *= vol_eff_factor;

            penalty *= adjustment;
            total_penalty -= penalty;
            reasons.push(format!(
                "{}: {}突破后收盘收回 (adj={:.2}, eff={:.2}, rvol={:.2})",
                well.source,
                if breach_up { "向上" } else { "向下" },
                adjustment,
                efficiency,
                rvol
            ));
        }

        // 9. 最终分数与乘数
        let final_score = total_penalty.clamp(-100.0, 100.0);
        let mult = if final_score < -30.0 {
            FAKEOUT_MULT_PENALTY
        } else if final_score < -10.0 {
            MINOR_FAKEOUT_MULT
        } else {
            1.0
        };

        let description = if final_score < -30.0 {
            "强烈假突破信号"
        } else if final_score < 0.0 {
            "疑似假突破"
        } else {
            "未发现假突破"
        };

        Ok(AnalysisResult::new(self.kind(), "FAKEOUT_V3".into())
            .with_score(final_score)
            .with_mult(mult)
            .because(description)
            .because(reasons.join("; "))
            .debug(json!({
                "penalty": total_penalty,
                "wells_scanned": wells.iter().filter(|w| w.is_active).count(),
                "atr": atr,
                "slope": slope,
                "rsi_state": format!("{:?}", rsi_state),
                "efficiency": efficiency,
                "rvol": rvol,
            })))
    }
}
