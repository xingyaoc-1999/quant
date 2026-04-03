use serde_json::json;

use crate::analyzer::{
    AnalysisError,
    AnalysisResult,
    Analyzer,
    AnalyzerKind,
    AnalyzerStage,
    Config,
    ContextKey, // 引入 ContextKey
    MarketContext,
    OIPositionState,
    Role,
    SharedAnalysisState,
};
use crate::types::{RsiState, TrendStructure};

pub struct MarketRegimeAnalyzer;

impl Analyzer for MarketRegimeAnalyzer {
    fn name(&self) -> &'static str {
        "market_regime"
    }

    fn stage(&self) -> AnalyzerStage {
        AnalyzerStage::Context // 作为上下文阶段，它最先运行
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
        // 1. 获取作用域并解包 Role (注意这里加了 '?')
        let scope = shared.scope(&ctx.symbol);
        let trend = ctx.get_role(Role::Trend)?;
        let t_feat = &trend.feature_set;

        // 2. 提取核心结构特征 (安全解包)
        let structure = t_feat
            .structure
            .trend_structure
            .as_ref()
            .unwrap_or(&TrendStructure::Range);

        let mut res = AnalysisResult::new(self.kind(), "REGIME_ANALYSIS".to_owned())
            .with_desc(format!("当前市场结构: {:?}", structure));

        // --- A. 核心维度计算 ---
        let m_regime; // 用于控制趋势类指标的权重
        let mut m_momentum = 1.0; // 用于控制动量类指标的权重
        let mut base_score = 0.0;

        match structure {
            // ==========================================
            // 模式 1: 强趋势 (溢价与加速逻辑)
            // ==========================================
            TrendStructure::StrongBullish | TrendStructure::StrongBearish => {
                let is_bull = matches!(structure, TrendStructure::StrongBullish);
                base_score = if is_bull { 60.0 } else { -60.0 };
                m_regime = 1.35; // 强化趋势跟踪指标
                res = res.because(if is_bull {
                    "强牛市：溢价驱动"
                } else {
                    "强熊市：恐慌驱动"
                });

                if let Some(rsi) = &t_feat.structure.rsi_state {
                    match rsi {
                        RsiState::Overbought | RsiState::Strong if is_bull => {
                            m_momentum = 1.25;
                            res = res.because("动能持续扩张，未见枯竭");
                        }
                        RsiState::Oversold | RsiState::Weak if !is_bull => {
                            m_momentum = 1.25;
                            res = res.because("下行动能爆发中");
                        }
                        RsiState::Oversold | RsiState::Weak if is_bull => {
                            m_momentum = 0.8; // 多头趋势但动能极弱，降权
                            res = res.because("强趋势中出现异常背离，注意回调风险");
                        }
                        _ => {}
                    }
                }
            }

            // ==========================================
            // 模式 2: 普通趋势 (健康度与枯竭校验)
            // ==========================================
            TrendStructure::Bullish | TrendStructure::Bearish => {
                let is_bull = matches!(structure, TrendStructure::Bullish);
                base_score = if is_bull { 30.0 } else { -30.0 };
                m_regime = 1.1;

                if let Some(rsi) = &t_feat.structure.rsi_state {
                    match rsi {
                        RsiState::Strong if is_bull => {
                            m_momentum = 1.2;
                            res = res.because("趋势动能充足，处于主升段");
                        }
                        RsiState::Weak if !is_bull => {
                            m_momentum = 1.2;
                            res = res.because("趋势动能充足，处于主降段");
                        }
                        RsiState::Neutral => {
                            m_momentum = 1.15;
                            res = res.because("趋势内健康回踩，指标完成修复");
                        }
                        RsiState::Overbought if is_bull => {
                            m_momentum = 0.8;
                            res = res.because("警告：普通多头趋势进入超买区，短期反转压力大");
                        }
                        RsiState::Oversold if !is_bull => {
                            m_momentum = 0.8;
                            res = res.because("警告：普通空头趋势进入超卖区，短期反转压力大");
                        }
                        RsiState::Weak if is_bull => {
                            m_momentum = 0.6;
                            res = res.because("动能衰竭预警：多头趋势向上推力不足").violate();
                        }
                        RsiState::Strong if !is_bull => {
                            m_momentum = 0.6;
                            res = res.because("动能衰竭预警：空头趋势下行推力不足").violate();
                        }
                        _ => {}
                    }
                }
            }

            // ==========================================
            // 模式 3: 震荡市 (反转逻辑与中轴过滤)
            // ==========================================
            TrendStructure::Range => {
                m_regime = 0.6; // 震荡市全局压制趋势指标
                if let Some(rsi) = &t_feat.structure.rsi_state {
                    match rsi {
                        RsiState::Overbought => {
                            base_score = -45.0;
                            m_momentum = 1.4; // 震荡市超买，放大做空动能权重
                            res = res.because("震荡区间顶部，高空胜率增加");
                        }
                        RsiState::Oversold => {
                            base_score = 45.0;
                            m_momentum = 1.4;
                            res = res.because("震荡区间底部，低吸胜率增加");
                        }
                        RsiState::Neutral => {
                            m_momentum = 0.1; // 杀掉中轴随机信号
                            res = res.because("处于震荡市中轴，信号价值极低").violate();
                        }
                        _ => {}
                    }
                }
            }
        }

        // --- B. 多周期同步校准 ---
        let mut m_mtf = 1.0;
        if !t_feat.structure.mtf_aligned.unwrap_or(true) {
            m_mtf = 0.7;
            res = res.because("多周期趋势不一致，权重下调");
        }

        // --- C. 持仓博弈修正 (OI) ---
        let mut m_oi = 1.0;
        if let Some(oi) = &trend.oi_data {
            let oi_change = oi.change_history.last().cloned().unwrap_or(0.0);
            let price_dir = t_feat.price_action.close - t_feat.price_action.open;
            let oi_state = OIPositionState::determine(price_dir, oi_change);

            match oi_state {
                OIPositionState::LongBuildUp | OIPositionState::ShortBuildUp => {
                    m_oi = 1.25;
                    res = res.because("持仓随趋势同步增长，确认为主力真钱入场");
                }
                OIPositionState::LongUnwinding | OIPositionState::ShortCovering => {
                    m_oi = 0.8;
                    res = res.because("持仓流失，价格走势由平仓驱动，缺乏后续动力");
                }
                _ => {}
            }
        }

        // --- D. 状态持久化与全局“Buff”施加 ---
        let final_mult = m_regime * m_mtf * m_momentum * m_oi;

        // 1. 保存供 AI 或后续逻辑读取的结构体状态
        scope.insert_ctx(
            ContextKey::RegimeStructure,
            serde_json::to_value(structure).unwrap_or_default(),
        );

        // 2. 关键！动态调整后续其他信号分析器的权重
        // 因为 Regime 是 Context 阶段，这里设置的乘数会在 Engine 聚合时生效
        scope.set_multiplier(AnalyzerKind::TrendStrength, m_regime * m_mtf);
        scope.set_multiplier(AnalyzerKind::Momentum, m_momentum);

        scope.insert_ctx(ContextKey::MultOi, json!(m_oi));

        Ok(res
            .with_score(base_score)
            .with_mult(final_mult)
            .debug(json!({
                "m_regime": m_regime,
                "m_momentum": m_momentum,
                "m_oi": m_oi,
                "m_mtf": m_mtf,
                "final_m": final_mult,
            })))
    }
}
