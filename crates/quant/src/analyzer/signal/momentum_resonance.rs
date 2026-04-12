use crate::analyzer::{AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, MarketContext, Role};
use crate::types::{DivergenceType, MacdCross, MacdMomentum};
use serde_json::json;

pub struct ResonanceAnalyzer;

impl Analyzer for ResonanceAnalyzer {
    fn name(&self) -> &'static str {
        "momentum_resonance"
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::Momentum
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        let entry_data = ctx.get_role(Role::Entry)?;
        let feat = &entry_data.feature_set;

        let is_reclaim = feat.signals.ma20_reclaim.unwrap_or(false);
        let is_breakdown = feat.signals.ma20_breakdown.unwrap_or(false);
        let macd_cross = feat.signals.macd_cross;

        if !is_reclaim && !is_breakdown && macd_cross.is_none() {
            return Ok(AnalysisResult::new(self.kind(), "RESONANCE_V2".into()).with_score(0.0));
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

        // --- 2. 基础得分 ---
        let base_score = if is_reclaim || is_breakdown {
            description.push("TRIGGER:MA20_RECLAIM".to_string());
            45.0
        } else {
            description.push("TRIGGER:MACD_CROSS".to_string());
            30.0
        };

        let slope_bars = feat.structure.ma20_slope_bars.abs();

        if slope_bars < 12 {
            m_resonance *= 1.3;
            description.push("EARLY_TREND".to_string());
        } else if slope_bars > 24 {
            let penalty = 1.0 - ((slope_bars - 24) as f64 / 30.0).min(0.7);
            m_resonance *= penalty;
            description.push(format!("AGING({:.0}%)", penalty * 100.0));
        }

        if let Some(macd_mom) = feat.signals.macd_momentum {
            let mom_confirmed = (is_long && macd_mom == MacdMomentum::Increasing)
                || (!is_long && macd_mom == MacdMomentum::Decreasing);
            if mom_confirmed {
                m_resonance *= 1.25;
                description.push("MOMENTUM_OK".to_string());
            } else {
                m_resonance *= 0.8;
                description.push("MOMENTUM_DIV".to_string());
            }
        }

        if let Some(div) = &feat.signals.macd_divergence {
            match (div, is_long) {
                (DivergenceType::Bearish, true) => {
                    m_resonance *= 0.6;
                    extra_score -= 30.0;
                    description.push("BEAR_DIV:LONG_WEAK".to_string());
                }
                (DivergenceType::Bearish, false) => {
                    m_resonance *= 1.3;
                    extra_score += 20.0;
                    description.push("BEAR_DIV:SHORT_OK".to_string());
                }
                (DivergenceType::Bullish, true) => {
                    m_resonance *= 1.3;
                    extra_score += 20.0;
                    description.push("BULL_DIV:LONG_OK".to_string());
                }
                (DivergenceType::Bullish, false) => {
                    m_resonance *= 0.6;
                    extra_score -= 30.0;
                    description.push("BULL_DIV:SHORT_WEAK".to_string());
                }
            }
        }

        let final_score = base_score * direction + extra_score;

        Ok(AnalysisResult::new(self.kind(), "RESONANCE_V2".into())
            .with_score(final_score)
            .with_mult(m_resonance)
            .because(description.join(" | "))
            .debug(json!({
                  "direction": if is_long { "BUY" } else { "SELL" },
                  "m_resonance": m_resonance,
                  "slope_bars": slope_bars,
                  "base_score": base_score,
                  "extra_score": extra_score,
            })))
    }
}
