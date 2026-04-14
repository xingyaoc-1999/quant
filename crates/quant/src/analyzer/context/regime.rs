use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{OIPositionState, PriceGravityWell, RsiState, TrendStructure};
use serde_json::json;

// ================= 常量配置池 =================
const MULT_REGIME_TREND: f64 = 1.5;
const MULT_REGIME_RANGE: f64 = 0.7;
const MULT_REGIME_NORMAL: f64 = 1.1;

const MOMENTUM_STRONG_BOOST: f64 = 1.3;
const MOMENTUM_WEAK_PENALTY: f64 = 0.6;
const MOMENTUM_COMPRESSED_PENALTY: f64 = 0.3;
const MOMENTUM_RANGE_CONFLUENCE: f64 = 2.0;
const MOMENTUM_DEAD_ZONE: f64 = 0.1;

const GAME_TAKER_SMOOTH: f64 = 2.5;
const RANGE_WELL_DIST_THRESHOLD: f64 = 0.015;

const TAKER_TREND_BULL_MIN: f64 = 0.52;
const TAKER_TREND_BEAR_MAX: f64 = 0.48;

const TSUNAMI_BASE_OI_DELTA: f64 = 0.012;
const TSUNAMI_BASE_TAKER_RATIO: f64 = 0.55;

const SLOPE_MOMENTUM_BOOST: f64 = 0.15;
const SLOPE_BARS_THRESHOLD: i32 = 3;

// 乘数上限（防止过度叠加）
const MAX_MULT_CAP: f64 = 3.0;

// =============================================

pub struct MarketRegimeAnalyzer;

impl Analyzer for MarketRegimeAnalyzer {
    fn name(&self) -> &'static str {
        "market_regime_v2"
    }
    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::MarketRegime
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        // 1. 提取基础缓存
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .unwrap_or(50.0);
        let is_vol_compressed = ctx
            .get_cached::<bool>(ContextKey::VolIsCompressed)
            .unwrap_or(false);
        let gravity_wells = ctx.get_cached::<Vec<PriceGravityWell>>(ContextKey::SpaceGravityWells);

        // 2. 核心数据提取
        let (
            structure,
            rsi_state,
            mtf_aligned,
            open_price,
            close_price,
            oi_delta,
            taker_ratio,
            ma20_slope,
            ma20_slope_bars,
        ) = {
            let trend = ctx.get_role(Role::Trend)?;
            let t_feat = &trend.feature_set;
            let struct_feat = &t_feat.structure;

            (
                struct_feat
                    .trend_structure
                    .clone()
                    .unwrap_or(TrendStructure::Range),
                struct_feat.rsi_state.clone(),
                struct_feat.mtf_aligned.unwrap_or(true),
                t_feat.price_action.open,
                t_feat.price_action.close,
                trend.oi_data.as_ref().map(|oi| oi.delta_ratio()),
                trend.taker_flow.taker_buy_ratio,
                struct_feat.ma20_slope,
                struct_feat.ma20_slope_bars,
            )
        };

        let mut res = AnalysisResult::new(self.kind(), "REGIME_CORE_V2".into())
            .because(format!("结构: {:?} | 波动分位: {:.1}%", structure, vol_p));

        let mut m_regime = MULT_REGIME_NORMAL;
        let mut m_momentum = 1.0;
        let mut base_score = 0.0;

        // 3. 基础得分与环境乘数逻辑
        match structure {
            TrendStructure::StrongBullish | TrendStructure::StrongBearish => {
                let is_bull = matches!(structure, TrendStructure::StrongBullish);
                // 改进1：基础得分结合斜率强度与持续时间
                let slope_factor = if let Some(slope) = ma20_slope {
                    let slope_abs = slope.abs().min(5.0);
                    // 持续时间越长，斜率影响越大
                    let bars_factor = (ma20_slope_bars as f64 / 10.0).min(1.5);
                    1.0 + slope_abs * 0.1 * bars_factor
                } else {
                    1.0
                };
                base_score = (if is_bull { 70.0 } else { -70.0 }) * slope_factor;
                m_regime = MULT_REGIME_TREND;

                // RSI 动量评估
                if let Some(rsi) = &rsi_state {
                    match rsi {
                        RsiState::Overbought | RsiState::Strong if is_bull => {
                            m_momentum = MOMENTUM_STRONG_BOOST + (vol_p / 150.0);
                        }
                        RsiState::Oversold | RsiState::Weak if !is_bull => {
                            m_momentum = MOMENTUM_STRONG_BOOST + (vol_p / 150.0);
                        }
                        RsiState::Weak if is_bull => {
                            m_momentum = if is_vol_compressed {
                                MOMENTUM_COMPRESSED_PENALTY
                            } else {
                                MOMENTUM_WEAK_PENALTY
                            };
                        }
                        RsiState::Strong if !is_bull => {
                            m_momentum = if is_vol_compressed {
                                MOMENTUM_COMPRESSED_PENALTY
                            } else {
                                MOMENTUM_WEAK_PENALTY
                            };
                        }
                        _ => m_momentum = 1.1,
                    }
                }

                // MA20 斜率动量增强（与趋势方向一致时）
                if let Some(slope) = ma20_slope {
                    let slope_aligned = (is_bull && slope > 0.0) || (!is_bull && slope < 0.0);
                    if slope_aligned {
                        let slope_strength = slope.abs().min(3.0);
                        let bars_factor = if ma20_slope_bars >= SLOPE_BARS_THRESHOLD {
                            1.2
                        } else {
                            1.0
                        };
                        // 限制增强幅度不超过 50%
                        let boost = 1.0 + slope_strength * SLOPE_MOMENTUM_BOOST * bars_factor;
                        m_momentum *= boost.min(1.5);
                        res = res.because(format!("MA20斜率共振: {:.2}", slope));
                    }
                }
            }
            TrendStructure::Range => {
                m_regime = MULT_REGIME_RANGE;
                if let Some(rsi) = &rsi_state {
                    match rsi {
                        RsiState::Overbought | RsiState::Oversold => {
                            let in_well = gravity_wells.as_ref().map_or(false, |wells| {
                                wells.iter().any(|w| {
                                    w.is_active
                                        && (w.level - close_price).abs() / close_price
                                            < RANGE_WELL_DIST_THRESHOLD
                                })
                            });
                            base_score = if matches!(rsi, RsiState::Overbought) {
                                -55.0
                            } else {
                                55.0
                            };
                            m_momentum = if in_well {
                                MOMENTUM_RANGE_CONFLUENCE
                            } else {
                                1.2
                            };
                            res = res.because("震荡边界共振触发");
                        }
                        _ => {
                            if is_vol_compressed {
                                m_momentum = MOMENTUM_DEAD_ZONE;
                                res = res.because("进入死水区");
                            }
                        }
                    }
                }
            }
            TrendStructure::Bullish | TrendStructure::Bearish => {
                let is_bull = matches!(structure, TrendStructure::Bullish);
                let slope_factor = if let Some(slope) = ma20_slope {
                    let slope_abs = slope.abs().min(3.0);
                    1.0 + slope_abs * 0.08
                } else {
                    1.0
                };
                base_score = (if is_bull { 30.0 } else { -30.0 }) * slope_factor;

                if let Some(slope) = ma20_slope {
                    let slope_aligned = (is_bull && slope > 0.0) || (!is_bull && slope < 0.0);
                    if slope_aligned && ma20_slope_bars >= 2 {
                        m_momentum *= 1.0 + slope.abs().min(2.0) * 0.1;
                    }
                }
            }
        }

        // 改进2：乘数上限控制（避免叠加失控）
        m_momentum = m_momentum.clamp(0.2, 2.5);

        let tsunami_oi_threshold =
            TSUNAMI_BASE_OI_DELTA * (1.0 + (vol_p - 50.0) / 200.0).clamp(0.8, 1.5);

        let (is_tsunami, oi_state) = match oi_delta {
            Some(delta) => {
                let price_pct = (close_price - open_price) / open_price.max(0.0001);
                // 注意：OIPositionState::determine 实现应符合：
                // 价格涨 + OI增 → LongBuildUp
                // 价格涨 + OI减 → ShortCovering
                // 价格跌 + OI增 → ShortBuildUp
                // 价格跌 + OI减 → LongLiquidation
                let state = OIPositionState::determine(price_pct, delta);

                let tsunami = matches!(state, OIPositionState::LongBuildUp)
                    && delta > tsunami_oi_threshold
                    && taker_ratio.unwrap_or(0.5) > TSUNAMI_BASE_TAKER_RATIO;

                (tsunami, state)
            }
            None => (false, OIPositionState::Neutral),
        };

        ctx.set_cached(ContextKey::IsMomentumTsunami, is_tsunami);
        ctx.set_cached(ContextKey::OiPositionState, oi_state);

        if is_tsunami {
            res = res.because("TSUNAMI: 动能海啸触发");
            m_momentum *= 1.8;
        }

        let m_game = match taker_ratio {
            Some(pct) => {
                let base_game: f64 = match structure {
                    TrendStructure::StrongBullish | TrendStructure::Bullish => {
                        if pct > TAKER_TREND_BULL_MIN {
                            1.0 + (pct - 0.5) * GAME_TAKER_SMOOTH
                        } else if pct < 0.45 {
                            0.6
                        } else {
                            1.0
                        }
                    }
                    TrendStructure::StrongBearish | TrendStructure::Bearish => {
                        if pct < TAKER_TREND_BEAR_MAX {
                            1.0 + (0.5 - pct) * GAME_TAKER_SMOOTH
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

                base_game.clamp(0.5, 2.0)
            }
            None => 1.0,
        };

        let m_mtf = if mtf_aligned { 1.0 } else { 0.6 };

        let raw_mult = m_regime + m_momentum + m_game + m_mtf - 3.0;
        // 改进2：最终乘数上限更严格
        let final_mult = raw_mult.clamp(0.2, MAX_MULT_CAP);

        ctx.set_cached(ContextKey::RegimeStructure, structure);

        Ok(res
            .with_score(base_score)
            .with_mult(final_mult)
            .debug(json!({
                "m_regime": m_regime,
                "m_momentum": m_momentum,
                "m_game": m_game,
                "m_mtf": m_mtf,
                "raw_mult": raw_mult,
                "oi_delta": oi_delta,
                "tsunami_threshold": tsunami_oi_threshold,
                "ma20_slope": ma20_slope,
                "ma20_slope_bars": ma20_slope_bars,
                "is_tsunami": is_tsunami,
            })))
    }
}
