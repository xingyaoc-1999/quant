use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ConfigurableAnalyzer, ContextKey,
    MarketContext, Role,
};
use crate::config::AnalyzerConfig;
use crate::types::market::{DivergenceType, MacdCross, MacdMomentum, TrendStructure};

// ==================== ResonanceExtra ====================
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct ResonanceExtra {
    pub direction: Option<String>,
    pub base_score: f64,
    pub resonance_mult: f64,
    pub extra_score: f64,
    pub slope_bars: i32,
    pub mtf_aligned: bool,
}

// ==================== ResonanceAnalyzer ====================
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
        "resonance_v2"
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
        let entry_data = ctx.get_role(Role::Entry)?;
        let feat = &entry_data.feature_set;

        let is_reclaim = feat.signals.ma20_reclaim.unwrap_or(false);
        let is_breakdown = feat.signals.ma20_breakdown.unwrap_or(false);
        let macd_cross = feat.signals.macd_cross;

        // 无触发信号则直接返回零分
        if !is_reclaim && !is_breakdown && macd_cross.is_none() {
            return Ok(AnalysisResult::new(self.kind()).with_score(0.0));
        }

        let direction = if is_reclaim || macd_cross == Some(MacdCross::Golden) {
            1.0
        } else {
            -1.0
        };
        let is_long = direction > 0.0;

        let mut description: Vec<String> = Vec::new();
        let mut m_resonance: f64 = 1.0;
        let mut extra_score: f64 = 0.0;

        let base_score = if is_reclaim || is_breakdown {
            description.push("TRIGGER:MA20_RECLAIM".to_string());
            self.config.resonance.ma20_trigger_score
        } else {
            description.push("TRIGGER:MACD_CROSS".to_string());
            self.config.resonance.macd_trigger_score
        };

        let slope_bars = feat.structure.ma20_slope_bars.abs();
        let cfg = &self.config.resonance;

        // 趋势早期/老化调整
        if slope_bars < cfg.early_trend_bars {
            m_resonance *= cfg.early_trend_mult;
            description.push("EARLY_TREND".to_string());
        } else if slope_bars > cfg.aging_trend_bars {
            let penalty = 1.0
                - ((slope_bars - cfg.aging_trend_bars) as f64 / cfg.aging_decay_period)
                    .min(cfg.max_aging_penalty);
            m_resonance *= penalty;
            description.push(format!("AGING({:.0}%)", penalty * 100.0));
        }

        // MACD 动量确认
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

        // MACD 背离处理
        if let Some(div) = &feat.signals.macd_divergence {
            match (div, is_long) {
                (DivergenceType::Bearish, true) => {
                    m_resonance *= cfg.bearish_div_long_penalty;
                    extra_score -= cfg.bearish_div_long_score_penalty;
                    description.push("BEAR_DIV:LONG_WEAK".to_string());
                }
                (DivergenceType::Bearish, false) => {
                    m_resonance *= cfg.bearish_div_short_boost;
                    extra_score += cfg.bearish_div_short_score_boost;
                    description.push("BEAR_DIV:SHORT_OK".to_string());
                }
                (DivergenceType::Bullish, true) => {
                    m_resonance *= cfg.bullish_div_long_boost;
                    extra_score += cfg.bullish_div_long_score_boost;
                    description.push("BULL_DIV:LONG_OK".to_string());
                }
                (DivergenceType::Bullish, false) => {
                    m_resonance *= cfg.bullish_div_short_penalty;
                    extra_score -= cfg.bullish_div_short_score_penalty;
                    description.push("BULL_DIV:SHORT_WEAK".to_string());
                }
            }
        }

        // MTF 对齐检查
        let mtf_aligned = match ctx.get_cached::<TrendStructure>(ContextKey::RegimeStructure) {
            Some(TrendStructure::StrongBullish | TrendStructure::Bullish) => is_long,
            Some(TrendStructure::StrongBearish | TrendStructure::Bearish) => !is_long,
            _ => true,
        };
        if !mtf_aligned {
            m_resonance *= cfg.mtf_misalign_penalty;
            description.push("MTF_MISALIGN".to_string());
        }

        let final_score = base_score * direction + extra_score * m_resonance;

        let extra = ResonanceExtra {
            direction: Some(if is_long {
                "BUY".to_string()
            } else {
                "SELL".to_string()
            }),
            base_score,
            resonance_mult: m_resonance,
            extra_score,
            slope_bars,
            mtf_aligned,
        };

        Ok(AnalysisResult::new(self.kind())
            .with_score(final_score)
            .with_mult(m_resonance)
            .because(description.join(" | "))
            .with_extra(extra))
    }
}
