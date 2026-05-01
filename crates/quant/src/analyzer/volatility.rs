use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ConfigurableAnalyzer, ContextKey,
    MarketContext, Role,
};
use crate::config::AnalyzerConfig;
use crate::types::market::{MarketStressLevel, TrendStructure, VolatilityConclusion};
use std::f64;

#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct VolatilityExtra {
    pub vol_percentile: f64,
    pub atr_ratio: f64,
    pub atr_vs_median: f64,
    pub is_compressed: bool,
    pub stress_level: MarketStressLevel,
    pub conclusion: VolatilityConclusion,
}

struct VolatilityInput {
    vol_p: f64,
    atr_ratio: f64,
    regime: TrendStructure,
    is_compressed: bool,
    vol_median_atr: f64,
}

// ==================== VolatilityEnvironmentAnalyzer ====================
pub struct VolatilityEnvironmentAnalyzer {
    config: AnalyzerConfig,
}

impl ConfigurableAnalyzer for VolatilityEnvironmentAnalyzer {
    fn with_config(config: AnalyzerConfig) -> Self {
        Self { config }
    }

    fn config(&self) -> &AnalyzerConfig {
        &self.config
    }
}

impl Analyzer for VolatilityEnvironmentAnalyzer {
    type Extra = VolatilityExtra;

    fn name(&self) -> &'static str {
        "volatility_env_v2"
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::Volatility
    }

    fn dependencies(&self) -> Vec<ContextKey> {
        vec![]
    }

    fn analyze(
        &self,
        ctx: &mut MarketContext,
    ) -> Result<AnalysisResult<Self::Extra>, AnalysisError> {
        let input = self.extract_volatility_input(ctx, &self.config.volatility)?;

        if ctx.global.last_price <= 0.0 {
            return Ok(AnalysisResult::new(self.kind())
                .with_score(0.0)
                .because("Invalid last_price"));
        }
        ctx.set_cached(ContextKey::VolAtrRatio, input.atr_ratio);
        ctx.set_cached(ContextKey::VolPercentile, input.vol_p);
        ctx.set_cached(ContextKey::VolIsCompressed, input.is_compressed);

        // 3. 评估波动环境
        let (multiplier, stress_level, conclusion, reason) =
            self.evaluate_environment(&input, ctx.global.last_price);

        ctx.set_cached(ContextKey::MarketStressLevel, stress_level);
        let mut res = AnalysisResult::new(self.kind())
            .with_score(0.0)
            .with_mult(multiplier)
            .because(reason);

        if matches!(stress_level, MarketStressLevel::MeatGrinder) {
            res = res.violate();
        }

        let extra = VolatilityExtra {
            vol_percentile: input.vol_p,
            atr_ratio: input.atr_ratio,
            atr_vs_median: input.atr_ratio
                / (input.vol_median_atr / ctx.global.last_price.max(f64::EPSILON)),
            is_compressed: input.is_compressed,
            stress_level,
            conclusion,
        };

        Ok(res.with_extra(extra))
    }
}

impl VolatilityEnvironmentAnalyzer {
    fn extract_volatility_input(
        &self,
        ctx: &MarketContext,
        cfg: &crate::config::VolatilityConfig,
    ) -> Result<VolatilityInput, AnalysisError> {
        let last_price = ctx.global.last_price;

        let trend_role = ctx.get_role(Role::Trend)?;
        let filter_role = ctx.get_role(Role::Filter).unwrap_or_else(|_| trend_role);

        let f_filter = &filter_role.feature_set;
        let f_trend = &trend_role.feature_set;

        let vol_p = f_filter.price_action.volatility_percentile;

        let atr = f_filter
            .indicators
            .atr_14
            .unwrap_or_else(|| last_price * 0.005);
        let atr_ratio = if last_price > f64::EPSILON {
            atr / last_price
        } else {
            0.005
        };

        let vol_median_atr = f_filter
            .indicators
            .atr_median_20
            .unwrap_or(atr)
            .max(f64::EPSILON);

        let regime = f_trend
            .structure
            .trend_structure
            .clone()
            .unwrap_or(TrendStructure::Range);

        let is_compressed = vol_p < cfg.compressed_threshold;

        Ok(VolatilityInput {
            vol_p,
            atr_ratio,
            regime,
            is_compressed,
            vol_median_atr,
        })
    }

    fn evaluate_environment(
        &self,
        input: &VolatilityInput,
        last_price: f64,
    ) -> (f64, MarketStressLevel, VolatilityConclusion, &'static str) {
        let cfg = &self.config.volatility;

        let atr_vs_median = input.atr_ratio / (input.vol_median_atr / last_price.max(f64::EPSILON));

        if atr_vs_median < cfg.extreme_low_ratio {
            return (
                cfg.dead_multiplier,
                MarketStressLevel::Dead,
                VolatilityConclusion::Dead,
                "市场进入死寂期，交易价值极低",
            );
        }

        match input.regime {
            TrendStructure::StrongBullish | TrendStructure::StrongBearish => {
                if atr_vs_median > cfg.acceleration_ratio {
                    (
                        cfg.acceleration_multiplier,
                        MarketStressLevel::Acceleration,
                        VolatilityConclusion::Acceleration,
                        "强趋势进入加速段，警惕乖离",
                    )
                } else if atr_vs_median < cfg.low_momentum_ratio {
                    (
                        cfg.weak_momentum_multiplier,
                        MarketStressLevel::Normal,
                        VolatilityConclusion::TrendWeakMomentum,
                        "趋势维持但动能不足",
                    )
                } else {
                    (
                        cfg.trend_resonance_multiplier,
                        MarketStressLevel::Normal,
                        VolatilityConclusion::TrendResonance,
                        "波动与趋势共振环境佳",
                    )
                }
            }
            TrendStructure::Range => {
                if atr_vs_median < cfg.squeeze_ratio {
                    (
                        cfg.squeeze_multiplier,
                        MarketStressLevel::Squeeze,
                        VolatilityConclusion::Squeeze,
                        "震荡市波动极度压缩 (Squeeze)",
                    )
                } else if atr_vs_median > cfg.meat_grinder_ratio {
                    (
                        cfg.meat_grinder_multiplier,
                        MarketStressLevel::MeatGrinder,
                        VolatilityConclusion::MeatGrinder,
                        "绞肉机行情，风险过高",
                    )
                } else {
                    (
                        cfg.normal_range_multiplier,
                        MarketStressLevel::Normal,
                        VolatilityConclusion::NormalRange,
                        "标准震荡波幅",
                    )
                }
            }
            _ => (
                cfg.normal_range_multiplier,
                MarketStressLevel::Normal,
                VolatilityConclusion::NormalRange,
                "标准市场环境",
            ),
        }
    }
}
