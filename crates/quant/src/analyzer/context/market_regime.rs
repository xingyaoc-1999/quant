use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, AnalyzerStage, Config, MarketContext,
    OIPositionState, Role, SharedAnalysisState,
};
use crate::types::{RsiState, TrendStructure};
use serde_json::json;
use std::borrow::Cow;

pub struct MarketRegimeAnalyzer;

impl Analyzer for MarketRegimeAnalyzer {
    fn name(&self) -> &'static str {
        "market_regime"
    }

    fn stage(&self) -> AnalyzerStage {
        AnalyzerStage::Context
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::MarketRegime
    }

    fn analyze(
        &self,
        ctx: &MarketContext,
        _config: &Config,
        shared: &SharedAnalysisState,
    ) -> Result<AnalysisResult, AnalysisError> {
        let trend = ctx.get_role(Role::Trend);
        let t_feat = &trend.feature_set;

        // --- 1. 初始化维度乘数 ---
        let mut m_regime: f64; // 维度：大趋势背景
        let mut m_momentum: f64 = 1.0; // 维度：动能/枯竭
        let mut m_oi: f64 = 1.0; // 维度：持仓博弈

        let mut final_score: f64 = 0.0;
        let mut description: Vec<Cow<'static, str>> = Vec::new();

        let structure = t_feat
            .structure
            .trend_structure
            .as_ref()
            .unwrap_or(&TrendStructure::Range);

        // --- A. 核心趋势维度逻辑 (Regime Dimension) ---
        match structure {
            // 1. 强趋势模式：动能爆发逻辑
            TrendStructure::StrongBullish | TrendStructure::StrongBearish => {
                let is_bullish = matches!(structure, TrendStructure::StrongBullish);
                final_score = if is_bullish { 60.0 } else { -60.0 };
                m_regime = 1.35; // 强趋势溢价
                description.push(Cow::Owned(format!(
                    "REGIME:STRONG_{}",
                    if is_bullish { "BULL" } else { "BEAR" }
                )));

                if let Some(rsi_state) = &t_feat.structure.rsi_state {
                    match rsi_state {
                        RsiState::Overbought | RsiState::Strong if is_bullish => {
                            m_momentum = 1.25; // 动能加速
                            description.push(Cow::Borrowed("MOMENTUM:ACCELERATING"));
                        }
                        RsiState::Oversold | RsiState::Weak if !is_bullish => {
                            m_momentum = 1.25;
                            description.push(Cow::Borrowed("MOMENTUM:ACCELERATING"));
                        }
                        RsiState::Oversold | RsiState::Weak if is_bullish => {
                            m_momentum = 0.8; // 强牛市中的异常走弱
                            description.push(Cow::Borrowed("MOMENTUM:DIVERGENCE_WARNING"));
                        }
                        _ => {}
                    }
                }
            }

            // 2. 普通趋势模式：健康度校验
            TrendStructure::Bullish | TrendStructure::Bearish => {
                let is_bullish = matches!(structure, TrendStructure::Bullish);
                final_score = if is_bullish { 30.0 } else { -30.0 };
                m_regime = 1.1;
                description.push(Cow::Owned(format!(
                    "REGIME:{}",
                    if is_bullish { "BULLISH" } else { "BEARISH" }
                )));

                if let Some(rsi_state) = &t_feat.structure.rsi_state {
                    match rsi_state {
                        RsiState::Overbought | RsiState::Oversold => {
                            m_momentum = 0.7; // 普通趋势进入极端区，预防枯竭
                            description.push(Cow::Borrowed("MOMENTUM:EXTENDED_REVERSION_RISK"));
                        }
                        RsiState::Neutral => {
                            m_momentum = 1.1; // 健康的趋势回调中轴
                            description.push(Cow::Borrowed("MOMENTUM:HEALTHY_PULLBACK"));
                        }
                        _ => {}
                    }
                }
            }

            TrendStructure::Range => {
                description.push(Cow::Borrowed("REGIME:RANGING"));
                m_regime = 0.6; // 震荡市全局压制风险

                if let Some(rsi_state) = &t_feat.structure.rsi_state {
                    match rsi_state {
                        RsiState::Overbought => {
                            final_score = -45.0; // 震荡顶
                            m_momentum = 1.4; // 增加反转信心
                            description.push(Cow::Borrowed("RANGE:TOP_SELL"));
                        }
                        RsiState::Oversold => {
                            final_score = 45.0; // 震荡底
                            m_momentum = 1.4;
                            description.push(Cow::Borrowed("RANGE:BOTTOM_BUY"));
                        }
                        RsiState::Neutral => {
                            m_momentum = 0.1; // 彻底杀掉中轴随机信号
                            description.push(Cow::Borrowed("RANGE:MIDDLE_DEAD_ZONE"));
                        }
                        _ => {
                            m_momentum = 0.6;
                            description.push(Cow::Borrowed("RANGE:TRANSITION"));
                        }
                    }
                }
            }
        }

        // --- B. 多周期同步修正 ---
        if !t_feat.structure.mtf_aligned.unwrap_or(true) {
            m_regime *= 0.7;
            description.push(Cow::Borrowed("MTF_CONFLICT_PENALTY"));
        }

        // --- C. 持仓数据修正 (OI) ---
        if let Some(oi) = &trend.oi_data {
            let macro_oi_change = oi.change_history.first().cloned().unwrap_or(0.0);
            let price_direction = t_feat.price_action.close - t_feat.price_action.open;
            let macro_state = OIPositionState::determine(price_direction, macro_oi_change);

            if matches!(
                macro_state,
                OIPositionState::LongBuildUp | OIPositionState::ShortBuildUp
            ) {
                m_oi = 1.25;
                description.push(Cow::Borrowed("OI:SMART_MONEY_CONFIRMED"));
            } else if matches!(
                macro_state,
                OIPositionState::LongUnwinding | OIPositionState::ShortCovering
            ) {
                m_oi = 0.8;
                description.push(Cow::Borrowed("OI:LIQUIDATION_DRIVEN_WARNING"));
            }
        }

        // --- D. 持久化与结果输出 ---
        shared
            .data
            .insert("multiplier:regime:base".into(), json!(m_regime));
        shared
            .data
            .insert("multiplier:regime:momentum".into(), json!(m_momentum));
        shared
            .data
            .insert("multiplier:regime:oi".into(), json!(m_oi));
        shared.data.insert(
            "ctx:regime:structure".into(),
            json!(format!("{:?}", structure)),
        );

        let total_m = m_regime * m_momentum * m_oi;

        Ok(AnalysisResult {
            score: final_score,
            weight_multiplier: total_m,
            description: description.join(" | "),
            debug_data: json!({
                "m_regime": m_regime,
                "m_momentum": m_momentum,
                "m_oi": m_oi,
                "final_m": total_m,
                "structure": structure
            }),
            ..Default::default()
        })
    }
}
