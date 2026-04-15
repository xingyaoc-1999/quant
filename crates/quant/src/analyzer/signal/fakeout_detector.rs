use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{PriceGravityWell, RsiState, VolumeState, WellSide};
use serde_json::json;

// ========== 常量配置 ==========
const BREACH_ATR_MULT: f64 = 0.25;
const CLOSE_RETURN_THRESHOLD: f64 = 0.001;
const FAKEOUT_BASE_PENALTY: f64 = 25.0;
const FAKEOUT_MULT_PENALTY: f64 = 0.6;
const MINOR_FAKEOUT_MULT: f64 = 0.85;

const SLOPE_STRONG_THRESHOLD: f64 = 0.1;
const SLOPE_STRONG_FACTOR: f64 = 0.7;
const SLOPE_WEAK_FACTOR: f64 = 1.3;

const RSI_OVERBOUGHT_FACTOR: f64 = 1.2;
const RSI_OVERSOLD_FACTOR: f64 = 1.2;

const VOL_EFF_LOW_THRESHOLD: f64 = 0.3;
const VOL_EFF_HIGH_THRESHOLD: f64 = 0.6;
const VOL_SURGE_MULT: f64 = 1.5;
const VOL_SHRINK_MULT: f64 = 0.7;

pub struct FakeoutDetector;

impl FakeoutDetector {
    fn is_breach(high: f64, low: f64, level: f64, atr: f64, is_above: bool) -> bool {
        if is_above {
            high > level + atr * BREACH_ATR_MULT
        } else {
            low < level - atr * BREACH_ATR_MULT
        }
    }

    fn is_closed_inside(close: f64, level: f64) -> bool {
        (close - level).abs() / level < CLOSE_RETURN_THRESHOLD
    }

    fn volume_efficiency_penalty(
        efficiency: f64,
        rvol: f64,
        volume_state: Option<VolumeState>,
    ) -> f64 {
        let mut factor: f64 = 1.0;
        if efficiency < VOL_EFF_LOW_THRESHOLD && rvol > VOL_SURGE_MULT {
            factor *= 1.4;
        } else if efficiency > VOL_EFF_HIGH_THRESHOLD && rvol > VOL_SURGE_MULT {
            factor *= 0.7;
        } else if rvol < VOL_SHRINK_MULT {
            factor *= 0.9;
        }

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
        let entry_role = ctx.get_role(Role::Entry)?;
        let fs = &entry_role.feature_set;
        let price_action = &fs.price_action;
        let current_high = price_action.high;
        let current_low = price_action.low;
        let current_close = price_action.close;

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

        let slope = fs.structure.ma20_slope.unwrap_or(0.0);
        let rsi_state = fs.structure.rsi_state.unwrap_or(RsiState::Neutral);

        let efficiency = ctx
            .get_cached::<f64>(ContextKey::LastEfficiency)
            .unwrap_or(0.5);
        let rvol = ctx.get_cached::<f64>(ContextKey::LastRVol).unwrap_or(1.0);
        let cached_vol_state = ctx
            .get_cached::<VolumeState>(ContextKey::VolumeState)
            .unwrap_or(VolumeState::Normal);

        let mut total_penalty = 0.0;
        let mut reasons = Vec::new();

        // 过滤：仅考虑普通支撑/阻力井，排除磁力井（磁力井是趋势加速目标，不适用假突破逻辑）
        for well in wells
            .iter()
            .filter(|w| w.is_active && matches!(w.side, WellSide::Support | WellSide::Resistance))
        {
            let level = well.level;
            let strength = well.strength.clamp(0.2, 3.0);

            let breach_up = Self::is_breach(current_high, current_low, level, atr, true);
            let breach_down = Self::is_breach(current_high, current_low, level, atr, false);

            if !breach_up && !breach_down {
                continue;
            }

            // 确保突破方向与井的类型一致：向上突破应是阻力，向下突破应是支撑
            if (breach_up && well.side != WellSide::Resistance)
                || (breach_down && well.side != WellSide::Support)
            {
                continue;
            }

            let closed_inside = Self::is_closed_inside(current_close, level);
            if !closed_inside {
                continue;
            }

            let mut penalty = FAKEOUT_BASE_PENALTY * strength;
            let mut adjustment = 1.0;

            // 趋势斜率调节
            if breach_up && slope > SLOPE_STRONG_THRESHOLD {
                adjustment = SLOPE_STRONG_FACTOR;
            } else if breach_down && slope < -SLOPE_STRONG_THRESHOLD {
                adjustment = SLOPE_STRONG_FACTOR;
            } else if breach_up && slope < -SLOPE_STRONG_THRESHOLD {
                adjustment = SLOPE_WEAK_FACTOR;
            } else if breach_down && slope > SLOPE_STRONG_THRESHOLD {
                adjustment = SLOPE_WEAK_FACTOR;
            }

            // RSI 调节
            if breach_up && matches!(rsi_state, RsiState::Overbought) {
                adjustment *= RSI_OVERBOUGHT_FACTOR;
            }
            if breach_down && matches!(rsi_state, RsiState::Oversold) {
                adjustment *= RSI_OVERSOLD_FACTOR;
            }

            // 量能调节
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
