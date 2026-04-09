use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{OIPositionState, PriceGravityWell, RsiState, TrendStructure};
use serde_json::json;

pub struct MarketRegimeAnalyzer;

impl Analyzer for MarketRegimeAnalyzer {
    fn name(&self) -> &'static str {
        "market_regime"
    }
    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::MarketRegime
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        let trend = ctx.get_role(Role::Trend)?;
        let t_feat = &trend.feature_set;

        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .unwrap_or(50.0);
        let is_vol_compressed = ctx
            .get_cached::<bool>(ContextKey::VolIsCompressed)
            .unwrap_or(false);

        let gravity_wells = ctx
            .get_cached::<serde_json::Value>(ContextKey::SpaceGravityWells)
            .and_then(|v| serde_json::from_value::<Vec<PriceGravityWell>>(v).ok());

        let structure = t_feat
            .structure
            .trend_structure
            .as_ref()
            .unwrap_or(&TrendStructure::Range);

        let mut res = AnalysisResult::new(self.kind(), "REGIME_CORE".into())
            .with_desc(format!("结构: {:?} | 波动分位: {:.1}%", structure, vol_p));

        let m_regime;
        let mut m_momentum = 1.0;
        let mut base_score = 0.0;

        match structure {
            TrendStructure::StrongBullish | TrendStructure::StrongBearish => {
                let is_bull = matches!(structure, TrendStructure::StrongBullish);
                base_score = if is_bull { 70.0 } else { -70.0 };
                m_regime = 1.5;

                if let Some(rsi) = &t_feat.structure.rsi_state {
                    match rsi {
                        RsiState::Overbought | RsiState::Strong if is_bull => {
                            m_momentum = 1.3 + (vol_p / 150.0);
                        }
                        RsiState::Oversold | RsiState::Weak if !is_bull => {
                            m_momentum = 1.3 + (vol_p / 150.0);
                        }
                        RsiState::Weak if is_bull => {
                            m_momentum = if is_vol_compressed { 0.3 } else { 0.6 };
                            res = res.violate();
                        }
                        _ => m_momentum = 1.1,
                    }
                }
            }
            TrendStructure::Range => {
                m_regime = 0.7;
                if let Some(rsi) = &t_feat.structure.rsi_state {
                    match rsi {
                        RsiState::Overbought | RsiState::Oversold => {
                            let in_well = gravity_wells.as_ref().map_or(false, |wells| {
                                wells.iter().any(|w| w.distance_pct.abs() < 0.012)
                            });
                            base_score = if matches!(rsi, RsiState::Overbought) {
                                -55.0
                            } else {
                                55.0
                            };
                            m_momentum = if in_well { 1.8 } else { 1.2 };
                            res = res.because("震荡极值+引力井共振：高确定性边界反转");
                        }
                        _ => {
                            if is_vol_compressed {
                                m_momentum = 0.1;
                                res = res.violate().because("死水区：波动压缩且无边界指示");
                            }
                        }
                    }
                }
            }
            _ => {
                m_regime = 1.1;
                base_score = if matches!(structure, TrendStructure::Bullish) {
                    30.0
                } else {
                    -30.0
                };
            }
        }

        let mut m_game = 1.0;

        if let Some(oi) = &trend.oi_data {
            let oi_change = oi.change_history.last().cloned().unwrap_or(0.0);
            let price_dir = t_feat.price_action.close - t_feat.price_action.open;
            let oi_state = OIPositionState::determine(price_dir, oi_change);

            match oi_state {
                OIPositionState::LongBuildUp | OIPositionState::ShortBuildUp => {
                    m_game *= if !is_vol_compressed { 1.35 } else { 1.1 };
                }
                OIPositionState::LongUnwinding | OIPositionState::ShortCovering => {
                    m_game *= 0.7;
                }
                _ => {}
            }
        }

        // 3.2 主动流向 (Taker Buy Ratio)
        if let Some(pct) = trend.taker_flow.taker_buy_ratio {
            let flow_strength = match structure {
                TrendStructure::StrongBullish | TrendStructure::Bullish => {
                    if pct > 0.53 {
                        1.0 + (pct - 0.5) * 2.5
                    } else if pct < 0.45 {
                        0.6
                    } else {
                        1.0
                    }
                }
                TrendStructure::StrongBearish | TrendStructure::Bearish => {
                    if pct < 0.47 {
                        1.0 + (0.5 - pct) * 2.5
                    } else if pct > 0.55 {
                        0.6
                    } else {
                        1.0
                    }
                }
                TrendStructure::Range => {
                    if pct > 0.62 || pct < 0.38 {
                        1.3
                    } else {
                        0.8
                    }
                }
            };
            m_game *= flow_strength;
        }

        let m_mtf = if t_feat.structure.mtf_aligned.unwrap_or(true) {
            1.0
        } else {
            0.6
        };

        let final_mult = (m_regime * m_momentum * m_game * m_mtf).clamp(0.1, 5.0);

        ctx.set_cached(ContextKey::RegimeStructure, json!(structure));

        Ok(res
            .with_score(base_score)
            .with_mult(final_mult)
            .debug(json!({
                "m_regime": m_regime,
                "m_momentum": m_momentum,
                "m_game": m_game,
                "is_compressed": is_vol_compressed,
                "final_mult": final_mult
            })))
    }
}
