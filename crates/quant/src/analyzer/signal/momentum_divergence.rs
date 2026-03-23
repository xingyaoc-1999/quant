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
        // 作为信号验证层，运行在 Resonance 之后
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

        let mut score = 0.0;
        let mut description: Vec<Cow<'static, str>> = Vec::new();

        // --- 1. 获取主信号方向 (Direction Locking) ---
        // 从共享状态中提取由 Resonance 或其他动量分析器存入的方向 (-1.0 到 1.0)
        let direction = shared
            .data
            .get("signal:direction")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        // 如果没有明确的交易意向，防守型分析器直接休眠，节省计算资源
        if direction == 0.0 {
            return Ok(AnalysisResult::default());
        }

        let is_long = direction > 0.0;

        // --- 2. 动能背离验证 (Divergence Check) ---
        if let Some(div) = &feat.signals.macd_divergence {
            match div {
                DivergenceType::Bearish => {
                    if is_long {
                        // 准备做多，但出现顶背离 -> 致命危险，强烈扣分 (负分抵消多头)
                        score -= 45.0;
                        description.push(Cow::Borrowed("MACD_BEAR_DIV:EXHAUSTION_RISK"));
                    } else {
                        // 准备做空，且出现顶背离 -> 顺势确认，给予奖励 (负分增强空头)
                        score -= 15.0;
                        description.push(Cow::Borrowed("MACD_BEAR_DIV:CONFIRM_SHORT"));
                    }
                }
                DivergenceType::Bullish => {
                    if !is_long {
                        // 准备做空，但出现底背离 -> 致命危险，强烈扣分 (正分抵消空头)
                        score += 45.0;
                        description.push(Cow::Borrowed("MACD_BULL_DIV:REVERSAL_RISK"));
                    } else {
                        // 准备做多，且出现底背离 -> 顺势确认，给予奖励 (正分增强多头)
                        score += 15.0;
                        description.push(Cow::Borrowed("MACD_BULL_DIV:CONFIRM_LONG"));
                    }
                }
            }
        }

        // --- 3. 支撑与阻力空间验证 (S/R Space) ---
        // 核心逻辑：买在阻力位下方和卖在支撑位上方是交易大忌
        if is_long {
            if let Some(dist_r) = feat.space.dist_to_resistance {
                if dist_r < 0.008 {
                    // 距离阻力位不到 0.8%，盈亏比极差 -> 强力踩刹车
                    score -= 30.0;
                    description.push(Cow::Borrowed("WALL_NEAR:RESISTANCE"));
                } else if dist_r > 0.03 {
                    // 上方空间极大 (>3%) -> 增加开仓信心
                    score += 10.0;
                    description.push(Cow::Borrowed("SPACE_OPEN:UP"));
                }
            }
        } else {
            if let Some(dist_s) = feat.space.dist_to_support {
                if dist_s < 0.008 {
                    // 距离支撑位不到 0.8%，随时可能被反抽 -> 强力踩刹车 (正分抵消空单)
                    score += 30.0;
                    description.push(Cow::Borrowed("WALL_NEAR:SUPPORT"));
                } else if dist_s > 0.03 {
                    // 下方深不见底 -> 增加开仓信心
                    score -= 10.0;
                    description.push(Cow::Borrowed("SPACE_OPEN:DOWN"));
                }
            }
        }

        // --- 4. 多周期对齐验证 (MTF Alignment) ---
        if let Some(aligned) = feat.structure.mtf_aligned {
            let sign = direction.signum(); // 1.0 或 -1.0
            if aligned {
                // 顺着大级别趋势走，给予奖励
                score += 10.0 * sign;
                description.push(Cow::Borrowed("MTF_ALIGNED"));
            } else {
                // 逆着大级别趋势（比如 15m 看多但 4h 在 200均线下方），削弱分数
                score -= 20.0 * sign;
                description.push(Cow::Borrowed("MTF_CONFLICT:COUNTER_TREND"));
            }
        }

        // --- 5. 组装结果 ---
        Ok(AnalysisResult {
            score,
            description: description.join(" | "),
            debug_data: json!({
                "macd_div": feat.signals.macd_divergence,
                "dist_r": feat.space.dist_to_resistance,
                "dist_s": feat.space.dist_to_support,
                "mtf_aligned": feat.structure.mtf_aligned,
                "direction_input": direction
            }),
            ..Default::default()
        })
    }
}
