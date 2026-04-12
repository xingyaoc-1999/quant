use std::f64;

use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::TrendStructure;
use serde_json::json;

pub struct VolatilityEnvironmentAnalyzer;

const VOL_EXTREME_LOW: f64 = 8.0;
const VOL_SQUEEZE_THRESHOLD: f64 = 22.0;
const VOL_LOW_MOMENTUM: f64 = 25.0;
const VOL_MEAT_GRINDER: f64 = 82.0;
const VOL_ACCELERATION: f64 = 88.0;

impl Analyzer for VolatilityEnvironmentAnalyzer {
    fn name(&self) -> &'static str {
        "volatility_env"
    }
    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::Volatility
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        let last_price = ctx.global.last_price;

        let (vol_p, atr_ratio, regime, is_compressed) = {
            let trend_role = ctx.get_role(Role::Trend)?;
            let filter_role = ctx.get_role(Role::Filter).unwrap_or(trend_role);

            let f_filter = &filter_role.feature_set;
            let f_trend = &trend_role.feature_set;

            let vol_p = f_filter.price_action.volatility_percentile;
            let atr = f_filter.indicators.atr_14.unwrap_or(last_price * 0.005);
            let regime = f_trend
                .structure
                .trend_structure
                .clone()
                .unwrap_or(TrendStructure::Range);

            let atr_ratio = if last_price > f64::EPSILON {
                atr / last_price
            } else {
                0.005
            };
            let is_compressed = vol_p < VOL_SQUEEZE_THRESHOLD;

            (vol_p, atr_ratio, regime, is_compressed)
        };

        ctx.set_cached(ContextKey::VolAtrRatio, atr_ratio);
        ctx.set_cached(ContextKey::VolPercentile, vol_p);
        ctx.set_cached(ContextKey::VolIsCompressed, is_compressed);

        let mut res = AnalysisResult::new(self.kind(), "VOL_ENV".into());

        if vol_p < VOL_EXTREME_LOW {
            return Ok(res
                .with_mult(0.1)
                .because("市场进入死寂期，暂无交易价值")
                .violate());
        }

        let m_env = match regime {
            TrendStructure::StrongBullish | TrendStructure::StrongBearish => {
                if vol_p > VOL_ACCELERATION {
                    res = res.because("强趋势进入加速段，警惕乖离");
                    1.1
                } else if vol_p < VOL_LOW_MOMENTUM {
                    res = res.because("趋势维持但动能不足");
                    0.7
                } else {
                    res = res.because("波动与趋势共振环境佳");
                    1.35
                }
            }
            TrendStructure::Range => {
                if is_compressed {
                    res = res.because("震荡市波动极度压缩 (Squeeze)");
                    0.9
                } else if vol_p > VOL_MEAT_GRINDER {
                    res = res.because("绞肉机行情，拒绝交易").violate();
                    0.25
                } else {
                    res = res.because("标准震荡波幅");
                    1.0
                }
            }
            _ => 1.0,
        };

        Ok(res.with_mult(m_env).debug(json!({
            "vol_p": vol_p,
            "atr_ratio": atr_ratio,
            "is_compressed": is_compressed
        })))
    }
}
