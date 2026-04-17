use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ConfigurableAnalyzer, ContextKey,
    MarketContext, Role,
};
use crate::config::AnalyzerConfig;
use crate::types::futures::OIPositionState;
use crate::types::gravity::PriceGravityWell;
use crate::types::market::{RsiState, TrendStructure};
use crate::types::session::TradingSession;
use std::f64;

// ==================== RegimeExtra ====================
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct RegimeExtra {
    pub structure: TrendStructure,
    pub is_tsunami: bool,
    pub oi_state: OIPositionState,
    pub momentum_mult: f64,
    pub regime_mult: f64,
    pub game_mult: f64,
    pub mtf_mult: f64,
    pub slope_bars: i32,
    pub session: String,
}

// ==================== MarketRegimeAnalyzer ====================
pub struct MarketRegimeAnalyzer {
    config: AnalyzerConfig,
}

impl ConfigurableAnalyzer for MarketRegimeAnalyzer {
    fn with_config(config: AnalyzerConfig) -> Self {
        Self { config }
    }

    fn config(&self) -> &AnalyzerConfig {
        &self.config
    }
}

impl Analyzer for MarketRegimeAnalyzer {
    type Extra = RegimeExtra;

    fn name(&self) -> &'static str {
        "market_regime_v3"
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::MarketRegime
    }

    fn dependencies(&self) -> Vec<ContextKey> {
        vec![
            ContextKey::VolPercentile,
            ContextKey::VolIsCompressed,
            ContextKey::SpaceGravityWells,
        ]
    }

    fn analyze(
        &self,
        ctx: &mut MarketContext,
    ) -> Result<AnalysisResult<Self::Extra>, AnalysisError> {
        // 0. 时段与基础环境
        let session = TradingSession::from_timestamp(ctx.global.timestamp);
        let session_adj = session.factor(&self.config.session);
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .copied()
            .unwrap_or(50.0);
        let is_vol_compressed = ctx
            .get_cached::<bool>(ContextKey::VolIsCompressed)
            .copied()
            .unwrap_or(false);
        let gravity_wells = ctx
            .get_cached::<Vec<PriceGravityWell>>(ContextKey::SpaceGravityWells)
            .cloned()
            .unwrap_or_default();

        let trend_role = ctx.get_role(Role::Trend)?;
        let t_feat = &trend_role.feature_set;
        let struct_feat = &t_feat.structure;

        let structure = struct_feat
            .trend_structure
            .clone()
            .unwrap_or(TrendStructure::Range);
        let rsi_state = struct_feat.rsi_state.clone();
        let mtf_aligned = struct_feat.mtf_aligned.unwrap_or(true);
        let open_price = t_feat.price_action.open;
        let close_price = t_feat.price_action.close;
        let oi_delta = trend_role.oi_data.as_ref().map(|oi| oi.delta_ratio());
        let taker_ratio = trend_role.taker_flow.taker_buy_ratio;
        let ma20_slope = struct_feat.ma20_slope;
        let ma20_slope_bars = struct_feat.ma20_slope_bars;

        let cfg = &self.config.regime;

        // 1. 基础乘数初始化并应用环境调整
        let vol_bias = if vol_p > 80.0 {
            0.8
        } else if vol_p < 20.0 {
            1.2
        } else {
            1.0
        };
        let mut m_regime = cfg.mult_regime_normal() * session_adj * vol_bias;
        let mut m_momentum = 1.0 * session_adj;
        let mut base_score = 0.0;

        // 2. 趋势持续性调整
        let prev_structure = ctx
            .get_cached::<TrendStructure>(ContextKey::RegimeStructure)
            .copied();
        let persistence = self.calc_persistence_factor(structure, prev_structure);
        m_regime *= persistence;

        let mut res = AnalysisResult::new(self.kind())
            .because(format!("结构: {:?} | 波动分位: {:.1}%", structure, vol_p));

        // 3. 趋势结构评分
        match structure {
            TrendStructure::StrongBullish | TrendStructure::StrongBearish => {
                let is_bull = matches!(structure, TrendStructure::StrongBullish);
                let slope_factor = self.calc_slope_factor(ma20_slope, ma20_slope_bars);
                base_score = (if is_bull { 70.0 } else { -70.0 }) * slope_factor;
                m_regime = cfg.mult_regime_trend() * session_adj * vol_bias * persistence;

                m_momentum = self.evaluate_momentum(&rsi_state, is_bull, vol_p, is_vol_compressed)
                    * session_adj;

                if let Some(slope) = ma20_slope {
                    let aligned = (is_bull && slope > 0.0) || (!is_bull && slope < 0.0);
                    if aligned {
                        let boost = self.calc_slope_boost(slope, ma20_slope_bars);
                        m_momentum *= boost;
                        res = res.because(format!("MA20斜率共振: {:.2}", slope));
                    }
                }
            }
            TrendStructure::Range => {
                m_regime = cfg.mult_regime_range() * session_adj * vol_bias * persistence;
                if let Some(rsi) = &rsi_state {
                    match rsi {
                        RsiState::Overbought | RsiState::Oversold => {
                            let in_well = self.is_near_well(&gravity_wells, close_price);
                            base_score = if matches!(rsi, RsiState::Overbought) {
                                -55.0
                            } else {
                                55.0
                            };
                            m_momentum = if in_well {
                                cfg.momentum_range_confluence()
                            } else {
                                1.2
                            } * session_adj;
                            res = res.because("震荡边界共振触发");
                        }
                        _ => {
                            if is_vol_compressed {
                                m_momentum = cfg.momentum_dead_zone() * session_adj;
                                res = res.because("进入死水区");
                            }
                        }
                    }
                }
            }
            TrendStructure::Bullish | TrendStructure::Bearish => {
                let is_bull = matches!(structure, TrendStructure::Bullish);
                let slope_factor = if let Some(slope) = ma20_slope {
                    1.0 + slope.abs().min(3.0) * 0.08
                } else {
                    1.0
                };
                base_score = (if is_bull { 30.0 } else { -30.0 }) * slope_factor;

                if let Some(slope) = ma20_slope {
                    let aligned = (is_bull && slope > 0.0) || (!is_bull && slope < 0.0);
                    if aligned && ma20_slope_bars >= 2 {
                        m_momentum *= 1.0 + slope.abs().min(2.0) * 0.1;
                    }
                }
                m_momentum *= session_adj;
            }
        }

        m_momentum = m_momentum.clamp(0.2, 2.5);

        // 4. 海啸判定（双向，使用固定阈值）
        let is_bullish = matches!(
            structure,
            TrendStructure::StrongBullish | TrendStructure::Bullish
        );
        let is_bearish = matches!(
            structure,
            TrendStructure::StrongBearish | TrendStructure::Bearish
        );
        let tsunami_oi_threshold = cfg.tsunami_base_oi_delta();

        let (is_tsunami, oi_state) = match oi_delta {
            Some(delta) => {
                let price_pct = (close_price - open_price) / open_price.max(f64::EPSILON);
                let state = OIPositionState::determine(price_pct, delta);
                let tsunami = (matches!(state, OIPositionState::LongBuildUp)
                    && is_bullish
                    && delta > tsunami_oi_threshold)
                    || (matches!(state, OIPositionState::ShortBuildUp)
                        && is_bearish
                        && delta > tsunami_oi_threshold);
                (tsunami, state)
            }
            None => (false, OIPositionState::Neutral),
        };

        ctx.set_cached(ContextKey::IsMomentumTsunami, is_tsunami);
        ctx.set_cached(ContextKey::OiPositionState, oi_state);
        ctx.set_cached(ContextKey::RegimeStructure, structure.clone());

        if is_tsunami {
            res = res.because("TSUNAMI: 动能海啸触发");
            m_momentum *= 1.8;
        }

        // 5. Taker博弈乘数
        let m_game = self.calc_game_mult(taker_ratio, structure);
        let m_mtf = if mtf_aligned { 1.0 } else { 0.6 };

        let raw_mult = m_regime + m_momentum + m_game + m_mtf - 3.0;
        let final_mult = raw_mult.clamp(0.2, cfg.max_mult_cap());

        let extra = RegimeExtra {
            structure,
            is_tsunami,
            oi_state,
            momentum_mult: m_momentum,
            regime_mult: m_regime,
            game_mult: m_game,
            mtf_mult: m_mtf,
            slope_bars: ma20_slope_bars,
            session: format!("{:?}", session),
        };

        Ok(res
            .with_score(base_score)
            .with_mult(final_mult)
            .with_extra(extra))
    }
}

impl MarketRegimeAnalyzer {
    fn calc_slope_factor(&self, slope: Option<f64>, bars: i32) -> f64 {
        if let Some(s) = slope {
            let slope_abs = s.abs().min(5.0);
            let bars_abs = bars.abs() as f64; // 取绝对值
            let bars_factor = (bars_abs / 10.0).min(1.5);
            1.0 + slope_abs * 0.1 * bars_factor
        } else {
            1.0
        }
    }

    fn calc_slope_boost(&self, slope: f64, bars: i32) -> f64 {
        let cfg = &self.config.regime;
        let slope_strength = slope.abs().min(3.0);
        let bars_abs = bars.abs(); // 取绝对值
        let bars_factor = if bars_abs >= cfg.slope_bars_threshold() {
            1.2
        } else {
            1.0
        };
        let boost = 1.0 + slope_strength * cfg.slope_momentum_boost() * bars_factor;
        boost.min(1.5)
    }

    fn evaluate_momentum(
        &self,
        rsi_state: &Option<RsiState>,
        is_bull: bool,
        vol_p: f64,
        is_vol_compressed: bool,
    ) -> f64 {
        let cfg = &self.config.regime;
        if let Some(rsi) = rsi_state {
            match rsi {
                RsiState::Overbought | RsiState::Strong if is_bull => {
                    cfg.momentum_strong_boost() + (vol_p / 150.0)
                }
                RsiState::Oversold | RsiState::Weak if !is_bull => {
                    cfg.momentum_strong_boost() + (vol_p / 150.0)
                }
                RsiState::Weak if is_bull => {
                    if is_vol_compressed {
                        cfg.momentum_compressed_penalty()
                    } else {
                        cfg.momentum_weak_penalty()
                    }
                }
                RsiState::Strong if !is_bull => {
                    if is_vol_compressed {
                        cfg.momentum_compressed_penalty()
                    } else {
                        cfg.momentum_weak_penalty()
                    }
                }
                _ => 1.1,
            }
        } else {
            1.0
        }
    }

    fn is_near_well(&self, wells: &[PriceGravityWell], close_price: f64) -> bool {
        let threshold = self.config.regime.range_well_dist_threshold();
        wells
            .iter()
            .any(|w| w.is_active && (w.level - close_price).abs() / close_price < threshold)
    }

    fn calc_game_mult(&self, taker_ratio: Option<f64>, structure: TrendStructure) -> f64 {
        let cfg = &self.config.regime;
        match taker_ratio {
            Some(pct) => {
                let base = match structure {
                    TrendStructure::StrongBullish | TrendStructure::Bullish => {
                        if pct > cfg.taker_trend_bull_min() {
                            1.0 + (pct - 0.5) * cfg.game_taker_smooth()
                        } else if pct < 0.45 {
                            0.6
                        } else {
                            1.0
                        }
                    }
                    TrendStructure::StrongBearish | TrendStructure::Bearish => {
                        if pct < cfg.taker_trend_bear_max() {
                            1.0 + (0.5 - pct) * cfg.game_taker_smooth()
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
                base.clamp(0.5, 2.0)
            }
            None => 1.0,
        }
    }

    fn calc_persistence_factor(
        &self,
        current: TrendStructure,
        prev: Option<TrendStructure>,
    ) -> f64 {
        let cfg = &self.config.regime;
        match (current, prev) {
            (
                TrendStructure::StrongBullish,
                Some(TrendStructure::StrongBullish | TrendStructure::Bullish),
            ) => cfg.trend_persistence_boost,
            (
                TrendStructure::StrongBearish,
                Some(TrendStructure::StrongBearish | TrendStructure::Bearish),
            ) => cfg.trend_persistence_boost,
            (
                TrendStructure::Bullish,
                Some(TrendStructure::Bullish | TrendStructure::StrongBullish),
            ) => cfg.trend_persistence_boost,
            (
                TrendStructure::Bearish,
                Some(TrendStructure::Bearish | TrendStructure::StrongBearish),
            ) => cfg.trend_persistence_boost,
            (TrendStructure::Range, Some(TrendStructure::Range)) => cfg.range_persistence_boost,
            _ => 1.0,
        }
    }
}
