use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ConfigurableAnalyzer, ContextKey,
    MarketContext, Role,
};
use crate::config::AnalyzerConfig;
use crate::types::market::{DivergenceType, MacdCross, MacdMomentum, TrendStructure};

#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct ResonanceExtra {
    pub direction: Option<String>,
    pub base_score: f64,
    pub resonance_mult: f64,
    pub slope_bars: i32,
    pub mtf_aligned: bool,
}

pub struct ResonanceAnalyzer {
    config: AnalyzerConfig,
}

impl ConfigurableAnalyzer for ResonanceAnalyzer {
    fn with_config(config: AnalyzerConfig) -> Self {
        Self { config }
    }

    fn config(&self) -> &AnalyzerConfig {
        &self.config
    }
}

impl Analyzer for ResonanceAnalyzer {
    type Extra = ResonanceExtra;

    fn name(&self) -> &'static str {
        "resonance_v3"
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::Resonance
    }

    fn dependencies(&self) -> Vec<ContextKey> {
        vec![ContextKey::RegimeStructure]
    }

    fn analyze(
        &self,
        ctx: &mut MarketContext,
    ) -> Result<AnalysisResult<Self::Extra>, AnalysisError> {
        let last_price = ctx.global.last_price;
        if !last_price.is_finite() || last_price <= 0.0 {
            return Ok(AnalysisResult::new(self.kind()).with_score(0.0));
        }

        let entry_data = ctx.get_role(Role::Entry)?;
        let feat = &entry_data.feature_set;

        let is_reclaim = feat.signals.ma20_reclaim.unwrap_or(false);
        let is_breakdown = feat.signals.ma20_breakdown.unwrap_or(false);
        let macd_cross = feat.signals.macd_cross;

        if !is_reclaim && !is_breakdown && macd_cross.is_none() {
            return Ok(AnalysisResult::new(self.kind()).with_score(0.0));
        }

        let direction = if is_reclaim || macd_cross == Some(MacdCross::Golden) {
            1.0
        } else {
            -1.0
        };
        let is_long = direction > 0.0;

        let mut description = Vec::new();
        let mut m_resonance = 1.0;
        let cfg = &self.config.resonance;

        let base_score = if is_reclaim {
            description.push("TRIGGER:MA20_RECLAIM".to_string());
            cfg.ma20_trigger_score
        } else if is_breakdown {
            description.push("TRIGGER:MA20_BREAKDOWN".to_string());
            cfg.ma20_trigger_score
        } else {
            description.push("TRIGGER:MACD_CROSS".to_string());
            cfg.macd_trigger_score
        };

        let slope_bars = feat.structure.ma20_slope_bars.abs();
        if slope_bars < cfg.early_trend_bars {
            m_resonance *= cfg.early_trend_mult;
            description.push("EARLY_TREND".to_string());
        } else if slope_bars > cfg.aging_trend_bars {
            let aging_period = cfg.aging_decay_period;
            let raw_penalty = ((slope_bars - cfg.aging_trend_bars) as f64 / aging_period)
                .min(cfg.max_aging_penalty);
            let penalty = (1.0 - raw_penalty).max(0.0);
            m_resonance *= penalty;
            description.push(format!("AGING({:.0}% remaining)", penalty * 100.0));
        }

        if let Some(macd_mom) = feat.signals.macd_momentum {
            let mom_confirmed = (is_long && macd_mom == MacdMomentum::Increasing)
                || (!is_long && macd_mom == MacdMomentum::Decreasing);
            if mom_confirmed {
                m_resonance *= cfg.momentum_confirm_mult;
                description.push("MOMENTUM_OK".to_string());
            } else {
                m_resonance *= cfg.momentum_div_penalty;
                description.push("MOMENTUM_DIV".to_string());
            }
        }

        // 背离处理：相反背离直接拒绝信号
        if let Some(div) = &feat.signals.macd_divergence {
            match (div, is_long) {
                (DivergenceType::Bearish, true) => {
                    return Ok(AnalysisResult::new(self.kind())
                        .violate()
                        .with_score(0.0)
                        .because("BEAR_DIV: 看跌背离拒绝做多"));
                }
                (DivergenceType::Bullish, false) => {
                    return Ok(AnalysisResult::new(self.kind())
                        .violate()
                        .with_score(0.0)
                        .because("BULL_DIV: 看涨背离拒绝做空"));
                }
                (DivergenceType::Bearish, false) => {
                    m_resonance *= cfg.bearish_div_short_boost;
                    description.push("BEAR_DIV:SHORT_OK".to_string());
                }
                (DivergenceType::Bullish, true) => {
                    m_resonance *= cfg.bullish_div_long_boost;
                    description.push("BULL_DIV:LONG_OK".to_string());
                }
            }
        }

        let mtf_aligned = match ctx.get_cached::<TrendStructure>(ContextKey::RegimeStructure) {
            Some(TrendStructure::StrongBullish | TrendStructure::Bullish) => is_long,
            Some(TrendStructure::StrongBearish | TrendStructure::Bearish) => !is_long,
            _ => {
                m_resonance *= cfg.mtf_unknown_penalty;
                description.push("MTF_UNKNOWN".to_string());
                true
            }
        };
        if !mtf_aligned {
            m_resonance *= cfg.mtf_misalign_penalty;
            description.push("MTF_MISALIGN".to_string());
        }

        let final_score = (base_score * direction * m_resonance).clamp(-100.0, 100.0);
        let extra = ResonanceExtra {
            direction: Some(if is_long { "BUY" } else { "SELL" }.to_string()),
            base_score,
            resonance_mult: m_resonance,
            slope_bars,
            mtf_aligned,
        };

        Ok(AnalysisResult::new(self.kind())
            .with_score(final_score)
            .with_mult(1.0)
            .because(description.join(" | "))
            .with_extra(extra))
    }
}
