use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{OIPositionState, RsiState, TrendStructure};
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

        // 1. 获取波动率背景（关键联动点）
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .unwrap_or(50.0);
        let is_compressed = ctx
            .get_cached::<bool>(ContextKey::VolIsCompressed)
            .unwrap_or(false);

        let structure = t_feat
            .structure
            .trend_structure
            .as_ref()
            .unwrap_or(&TrendStructure::Range);

        let mut res = AnalysisResult::new(self.kind(), "REGIME_ANALYSIS".into())
            .with_desc(format!("结构: {:?} | 波动分位: {:.1}%", structure, vol_p));

        let m_regime;
        let mut m_momentum = 1.0;
        let mut base_score = 0.0;

        match structure {
            // ==========================================
            // 模式 1: 强趋势 (配合波动率判断是否处于加速期)
            // ==========================================
            TrendStructure::StrongBullish | TrendStructure::StrongBearish => {
                let is_bull = matches!(structure, TrendStructure::StrongBullish);
                base_score = if is_bull { 65.0 } else { -65.0 };
                m_regime = 1.4;

                if let Some(rsi) = &t_feat.structure.rsi_state {
                    match rsi {
                        // 智能联动：强趋势中，波动率越高，RSI超买的“容忍度”越高
                        RsiState::Overbought | RsiState::Strong if is_bull => {
                            // 公式：在高波动下，超买不再是压力，而是动能的溢价
                            m_momentum = 1.2 + (vol_p / 200.0);
                            res = res.because("强牛市+高动能溢价：指标钝化视为趋势加强信号");
                        }
                        RsiState::Oversold | RsiState::Weak if !is_bull => {
                            m_momentum = 1.2 + (vol_p / 200.0);
                            res = res.because("强熊市+恐慌溢价：惯性下行压力持续放大");
                        }
                        // 强趋势中的逆向指标：如果是低波动下的逆向，说明趋势极度疲软
                        RsiState::Weak if is_bull => {
                            m_momentum = if vol_p < 30.0 { 0.4 } else { 0.7 };
                            res = res
                                .because("警告：强牛市出现动能背离，动力严重不足")
                                .violate();
                        }
                        _ => {
                            m_momentum = 1.1;
                        }
                    }
                }
            }

            // ==========================================
            // 模式 2: 普通趋势 (严格校验回撤与竭尽)
            // ==========================================
            TrendStructure::Bullish | TrendStructure::Bearish => {
                let is_bull = matches!(structure, TrendStructure::Bullish);
                base_score = if is_bull { 35.0 } else { -35.0 };
                m_regime = 1.15;

                if let Some(rsi) = &t_feat.structure.rsi_state {
                    match rsi {
                        RsiState::Neutral => {
                            m_momentum = 1.2;
                            res = res.because("趋势内健康修正，等待下一波行情发动");
                        }
                        RsiState::Overbought if is_bull => {
                            m_momentum = 0.75; // 普通趋势超买需谨慎
                            res = res.because("普通多头趋势进入阻力区，警惕多头陷阱");
                        }
                        RsiState::Oversold if !is_bull => {
                            m_momentum = 0.75;
                            res = res.because("普通空头趋势进入支撑区，警惕空头陷阱");
                        }
                        _ => {}
                    }
                }
            }

            // ==========================================
            // 模式 3: 震荡市 (绝对的反转逻辑)
            // ==========================================
            TrendStructure::Range => {
                m_regime = 0.6;
                if let Some(rsi) = &t_feat.structure.rsi_state {
                    match rsi {
                        RsiState::Overbought => {
                            base_score = -50.0;
                            m_momentum = 1.5;
                            res = res.because("区间顶部触碰：触发高位反转逻辑");
                        }
                        RsiState::Oversold => {
                            base_score = 50.0;
                            m_momentum = 1.5;
                            res = res.because("区间底部触碰：触发低位反转逻辑");
                        }
                        RsiState::Neutral => {
                            // 联动：震荡市中轴 + 波动率压缩 = 极度无聊
                            if is_compressed {
                                m_momentum = 0.05;
                                res = res
                                    .because("震荡死水区：波动与成交双重枯竭，禁止入场")
                                    .violate();
                            } else {
                                m_momentum = 0.2;
                                res = res.because("震荡中轴：随机波动过高，缺乏交易价值");
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // --- B. 多周期同步 (MTF) ---
        let m_mtf = if t_feat.structure.mtf_aligned.unwrap_or(true) {
            1.0
        } else {
            0.65
        };

        // --- C. 持仓博弈 (OI) ---
        let mut m_oi = 1.0;
        if let Some(oi) = &trend.oi_data {
            let oi_change = oi.change_history.last().cloned().unwrap_or(0.0);
            let price_dir = t_feat.price_action.close - t_feat.price_action.open;
            let oi_state = OIPositionState::determine(price_dir, oi_change);

            match oi_state {
                OIPositionState::LongBuildUp | OIPositionState::ShortBuildUp => m_oi = 1.3,
                OIPositionState::LongUnwinding | OIPositionState::ShortCovering => m_oi = 0.75,
                _ => {}
            }
        }
        // --- E. 主动流向博弈 (Taker Flow) ---
        let mut m_flow = 1.0;

        if let Some(pct) = trend.taker_flow.taker_buy_ratio {
            match structure {
                TrendStructure::StrongBullish | TrendStructure::Bullish => {
                    if pct > 0.55 {
                        // 主买占比超过 55%：动力充足
                        // 公式：将 0.55-0.80 的占比映射为 1.1-1.6 的乘数
                        m_flow = 1.0 + (pct.min(0.8) - 0.5) * 2.0;
                    } else if pct < 0.42 {
                        // 主买占比低于 42%：虽然价格在涨，但全是主动砸盘，背离严重
                        m_flow = 0.65;
                    }
                }

                TrendStructure::StrongBearish | TrendStructure::Bearish => {
                    if pct < 0.45 {
                        // 主买占比低于 45% = 主卖占比超过 55%
                        m_flow = 1.0 + (0.5 - pct.max(0.2)) * 2.0;
                    } else if pct > 0.58 {
                        // 价格在跌，但主动买盘开始反扑，警惕空头回补
                        m_flow = 0.65;
                    }
                }

                // --- 震荡市：利用极端占比识别假突破 ---
                TrendStructure::Range => {
                    // 占比 > 65% 或 < 35% 通常意味着震荡即将结束，资金开始选择方向
                    if pct > 0.65 || pct < 0.35 {
                        m_flow = 1.25;
                    } else {
                        m_flow = 0.9; // 极其均衡的占比在震荡市意味着没有交易价值
                    }
                }
            }
        }
        let final_mult = m_regime * m_mtf * m_momentum * m_oi * m_flow;
        // --- D. 持久化 (必须与 Volume/Level 分析器对齐) ---
        ctx.set_cached(ContextKey::RegimeStructure, json!(structure));
        ctx.set_multiplier(AnalyzerKind::TrendStrength, m_regime * m_mtf);
        ctx.set_multiplier(AnalyzerKind::Momentum, m_momentum);
        ctx.set_cached(ContextKey::MultOi, json!(m_oi));

        Ok(res
            .with_score(base_score)
            .with_mult(final_mult)
            .debug(json!({
                "vol_p": vol_p,
                "m_momentum": m_momentum,
                "m_oi": m_oi,
                "final_m": final_mult,
            })))
    }
}
