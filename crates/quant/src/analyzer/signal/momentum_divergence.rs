use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, AnalyzerStage, Config, MarketContext,
    Role, SharedAnalysisState,
};
use crate::types::DivergenceType;
use serde_json::json;
use std::borrow::Cow;

pub struct StructureDivergenceAnalyzer;

impl Analyzer for StructureDivergenceAnalyzer {
    fn name(&self) -> &'static str {
        "structure_divergence"
    }
    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::Divergence
    }
    fn stage(&self) -> AnalyzerStage {
        AnalyzerStage::Signal
    }

    fn analyze(
        &self,
        ctx: &MarketContext,
        _config: &Config,
        shared: &SharedAnalysisState,
    ) -> Result<AnalysisResult, AnalysisError> {
        let trend_data = ctx.get_role(Role::Trend);
        let feat = &trend_data.feature_set;

        // --- 1. 获取上下文 ---
        let regime = shared
            .data
            .get("ctx:regime:structure")
            .map(|v| v.to_string())
            .unwrap_or("Range".to_string());

        // 获取当前信号方向 (Signal 阶段特有)
        let action = shared
            .data
            .get("signal:action")
            .map(|v| v.to_string())
            .unwrap_or("NONE".to_string());

        if action == "NONE" {
            return Ok(AnalysisResult::default());
        }

        let is_long = action == "BUY";
        let mut score = 0.0;
        let mut m_divergence: f64 = 1.0; // 引入维度乘数：背离验证
        let mut m_mtf: f64 = 1.0; // 引入维度乘数：多周期对齐
        let mut description: Vec<Cow<'static, str>> = Vec::new();

        // --- 2. 背离验证 (Divergence Validation) ---
        if let Some(div) = &feat.signals.macd_divergence {
            match (div, is_long) {
                // 顶背离 (Bearish Div)
                (DivergenceType::Bearish, true) => {
                    // 做多遇到顶背离：严重惩罚
                    m_divergence = if regime == "Range" { 0.3 } else { 0.6 };
                    score -= 30.0;
                    description.push(Cow::Borrowed("MACD_BEAR_DIV:LONG_EXHAUSTION"));
                }
                (DivergenceType::Bearish, false) => {
                    // 做空遇到顶背离：共振奖励
                    m_divergence = 1.3;
                    score += 20.0;
                    description.push(Cow::Borrowed("MACD_BEAR_DIV:SHORT_CONFIRM"));
                }
                // 底背离 (Bullish Div)
                (DivergenceType::Bullish, true) => {
                    // 做多遇到底背离：共振奖励
                    m_divergence = 1.3;
                    score += 20.0;
                    description.push(Cow::Borrowed("MACD_BULL_DIV:LONG_CONFIRM"));
                }
                (DivergenceType::Bullish, false) => {
                    // 做空遇到底背离：严重惩罚
                    m_divergence = if regime == "Range" { 0.3 } else { 0.6 };
                    score -= 30.0;
                    description.push(Cow::Borrowed("MACD_BULL_DIV:SHORT_EXHAUSTION"));
                }
            }
        }

        // --- 3. MTF 校验 (MTF Dimension) ---
        if let Some(aligned) = feat.structure.mtf_aligned {
            if aligned {
                m_mtf = 1.2;
                description.push(Cow::Borrowed("MTF_ALIGNED"));
            } else {
                // 强趋势下的逆势惩罚更重
                m_mtf = if regime.contains("Strong") { 0.4 } else { 0.7 };
                description.push(Cow::Borrowed("MTF_CONFLICT:ANTI_TREND"));
            }
        }

        // --- 4. 空间截断 (Final Safety Gate) ---
        let dist_to_barrier = if is_long {
            feat.space.dist_to_resistance.unwrap_or(1.0)
        } else {
            feat.space.dist_to_support.unwrap_or(1.0)
        };

        if dist_to_barrier < 0.002 {
            // 距离最近障碍不足 0.2%：直接将该交易降权到几乎不可执行
            m_divergence *= 0.2;
            description.push(Cow::Borrowed("SAFETY:NO_RUNWAY_KILL_SWITCH"));
        }

        // --- 5. 持久化到维度矩阵 ---
        // 注意：这是 Signal 阶段的校验维度
        shared.data.insert(
            format!("multiplier:signal_check:{}_div", action.to_lowercase()),
            json!(m_divergence),
        );
        shared.data.insert(
            format!("multiplier:signal_check:{}_mtf", action.to_lowercase()),
            json!(m_mtf),
        );

        Ok(AnalysisResult {
            score, // 这里保留 score，因为 Signal 阶段通常需要加分/减分来决定最终等级
            weight_multiplier: m_divergence * m_mtf,
            description: description.join(" | "),
            debug_data: json!({
                "action": action,
                "m_div": m_divergence,
                "m_mtf": m_mtf,
                "dist": dist_to_barrier
            }),
            ..Default::default()
        })
    }
}
