use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::TrendStructure;
use serde_json::json;
use std::f64;

pub struct VolatilityEnvironmentAnalyzer;

// 阈值相对于波动率中位数的比例
const VOL_EXTREME_LOW_RATIO: f64 = 0.3; // 低于中位数的30%视为死寂
const VOL_SQUEEZE_RATIO: f64 = 0.6; // 低于60%视为压缩
const VOL_LOW_MOMENTUM_RATIO: f64 = 0.7;
const VOL_MEAT_GRINDER_RATIO: f64 = 2.0; // 高于中位数2倍视为绞肉机
const VOL_ACCELERATION_RATIO: f64 = 2.2;

impl Analyzer for VolatilityEnvironmentAnalyzer {
    fn name(&self) -> &'static str {
        "volatility_env"
    }
    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::Volatility
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        let last_price = ctx.global.last_price;

        let (vol_p, atr_ratio, regime, is_compressed, vol_median_atr) = {
            let trend_role = ctx.get_role(Role::Trend)?;
            let filter_role = ctx.get_role(Role::Filter).unwrap_or(trend_role);

            let f_filter = &filter_role.feature_set;
            let f_trend = &trend_role.feature_set;

            let vol_p = f_filter.price_action.volatility_percentile;
            let atr = f_filter.indicators.atr_14.unwrap_or(last_price * 0.005);
            let vol_median_atr = f_filter.indicators.atr_median_20.unwrap_or(atr);
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
            let is_compressed = vol_p < 22.0; // 保留原逻辑，可后续优化

            (vol_p, atr_ratio, regime, is_compressed, vol_median_atr)
        };

        ctx.set_cached(ContextKey::VolAtrRatio, atr_ratio);
        ctx.set_cached(ContextKey::VolPercentile, vol_p);
        ctx.set_cached(ContextKey::VolIsCompressed, is_compressed);

        let mut res = AnalysisResult::new(self.kind(), "VOL_ENV".into());

        // 动态阈值：基于ATR与其中位数的比值
        let atr_vs_median = atr_ratio / (vol_median_atr / last_price).max(f64::EPSILON);

        // 死寂市场：降低乘数但不直接拒绝（除非连续多周期）
        if atr_vs_median < VOL_EXTREME_LOW_RATIO {
            res = res.because("市场进入死寂期，交易价值极低");
            return Ok(res.with_mult(0.15));
        }

        let m_env = match regime {
            TrendStructure::StrongBullish | TrendStructure::StrongBearish => {
                if atr_vs_median > VOL_ACCELERATION_RATIO {
                    res = res.because("强趋势进入加速段，警惕乖离");
                    1.1
                } else if atr_vs_median < VOL_LOW_MOMENTUM_RATIO {
                    res = res.because("趋势维持但动能不足");
                    0.7
                } else {
                    res = res.because("波动与趋势共振环境佳");
                    1.35
                }
            }
            TrendStructure::Range => {
                if atr_vs_median < VOL_SQUEEZE_RATIO {
                    res = res.because("震荡市波动极度压缩 (Squeeze)");
                    0.9
                } else if atr_vs_median > VOL_MEAT_GRINDER_RATIO {
                    res = res.because("绞肉机行情，风险过高").violate();
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
            "atr_vs_median": atr_vs_median,
            "is_compressed": is_compressed
        })))
    }
}
