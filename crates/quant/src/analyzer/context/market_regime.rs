use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, AnalyzerStage, Config, MarketContext,
    Role, SharedAnalysisState,
};
use crate::types::TrendStructure;
use serde_json::json;

/// 市场环境分析器 (Context 阶段)
/// 职责：基于大周期的均线结构和波动率百分位，定义趋势背景及其对信号的过滤权重
pub struct MarketRegimeAnalyzer;

impl Analyzer for MarketRegimeAnalyzer {
    fn name(&self) -> &'static str {
        "market_regime"
    }

    fn stage(&self) -> AnalyzerStage {
        AnalyzerStage::Context
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::TrendStrength
    }

    fn analyze(
        &self,
        ctx: &MarketContext,
        _config: &Config,
        shared: &SharedAnalysisState,
    ) -> Result<AnalysisResult, AnalysisError> {
        let trend_data = ctx.get_role(Role::Trend);
        let trend_feat = &trend_data.feature_set;

        // env_multiplier 用于全局环境加成/压制
        let mut env_multiplier = 1.0;
        let mut score = 0.0;
        let mut description = Vec::new();

        // --- 1. 趋势结构评估 ---
        if let Some(structure) = trend_feat.structure.trend_structure {
            match structure {
                TrendStructure::StrongBullish => {
                    score = 60.0;
                    env_multiplier = 1.3; // 强势多头，放大信号
                    description.push("大周期完全多头排列");
                }
                TrendStructure::Bullish => {
                    score = 30.0;
                    env_multiplier = 1.1;
                    description.push("大周期多头趋势");
                }
                TrendStructure::StrongBearish => {
                    score = -60.0;
                    env_multiplier = 1.3; // 强势空头，放大做空信号（取决于具体逻辑，这里指环境强弱）
                    description.push("大周期完全空头排列");
                }
                TrendStructure::Bearish => {
                    score = -30.0;
                    env_multiplier = 1.1;
                    description.push("大周期空头趋势");
                }
                TrendStructure::Range => {
                    score = 0.0;
                    env_multiplier = 0.35; // 震荡市极大削减信号可信度
                    description.push("均线纠缠(Range)");
                }
            }
        }

        // --- 2. 波动率调节 (非线性) ---
        // 获取波动率在历史中的百分位位置
        let vol_p = trend_feat.price_action.volatility_percentile;
        if vol_p > 95.0 {
            // 极高波动率通常伴随着情绪过热或洗盘
            env_multiplier *= 0.6;
            description.push("波动率过激");
        } else if vol_p < 10.0 {
            // 波动率收敛是爆发前兆
            env_multiplier *= 1.2;
            description.push("波动率收敛");
        }

        // --- 3. 持久化到 Shared Analysis State ---
        // 统一命名：context:xxx_multiplier
        shared
            .data
            .insert("context:trend_multiplier".into(), json!(env_multiplier));
        shared
            .data
            .insert("context:trend_score".into(), json!(score));

        Ok(AnalysisResult {
            score, // 环境分

            description: description.join(" | "),
            debug_data: json!({
                "structure": trend_feat.structure.trend_structure,
                "volatility_p": vol_p,
                "final_trend_multiplier": env_multiplier
            }),
            ..Default::default()
        })
    }
}
