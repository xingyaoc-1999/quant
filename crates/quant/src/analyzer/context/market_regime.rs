use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, AnalyzerStage, Config, MarketContext,
    OIPositionState, Role, SharedAnalysisState,
};
use crate::types::TrendStructure; // 确保引入了新定义的枚举
use serde_json::json;

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
        // 1. 获取 Trend 角色的数据
        let trend_data = ctx.get_role(Role::Trend);
        let trend_feat = &trend_data.feature_set;

        let mut multiplier = 1.0;
        let mut score = 0.0;
        let mut description = Vec::new();

        // --- A. 趋势结构评估 (宏观定调) ---
        if let Some(structure) = trend_feat.structure.trend_structure {
            match structure {
                TrendStructure::StrongBullish => {
                    score = 60.0;
                    multiplier = 1.25;
                    description.push("🚀 强力多头");
                }
                TrendStructure::Bullish => {
                    score = 30.0;
                    multiplier = 1.1;
                    description.push("📈 上升趋势");
                }
                TrendStructure::StrongBearish => {
                    score = -60.0;
                    multiplier = 1.25;
                    description.push("💀 强力空头");
                }
                TrendStructure::Bearish => {
                    score = -30.0;
                    multiplier = 1.1;
                    description.push("📉 下降趋势");
                }
                TrendStructure::Range => {
                    score = 0.0;
                    multiplier = 0.45;
                    description.push("🦀 震荡纠缠");
                }
            }
        }

        // --- B. 波动率调节 (强度修正) ---
        let vol_p = trend_feat.price_action.volatility_percentile;
        if vol_p > 90.0 {
            multiplier *= 0.75;
            description.push("⚠️ 波动过热");
        } else if vol_p < 15.0 {
            multiplier *= 1.3;
            description.push("⏳ 深度收敛");
        }

        // --- C. 合约博弈分析 (利用重构后的 OIData) ---
        // 直接从 RoleData 中获取已经计算好的语义化 OI 状态
        if let Some(oi) = &trend_data.oi_data {
            let mut futures_tags = Vec::new();

            // 直接对状态枚举进行模式匹配，逻辑非常清晰
            match oi.state {
                OIPositionState::LongBuildUp => {
                    multiplier *= 1.2;
                    futures_tags.push("真钱买入");
                }
                OIPositionState::ShortBuildUp => {
                    multiplier *= 1.2;
                    futures_tags.push("空头压盘");
                }
                OIPositionState::LongUnwinding => {
                    multiplier *= 0.8;
                    futures_tags.push("多头踩踏");
                }
                OIPositionState::ShortCovering => {
                    multiplier *= 0.85;
                    futures_tags.push("空头回补");
                }
                OIPositionState::Neutral => {}
            }

            if !futures_tags.is_empty() {
                description.push(format!("[博弈: {}]", futures_tags.join(",")));
            }
        }

        // --- D. 结果写入 SharedState 供后续 Signal 阶段分析器引用 ---
        shared
            .data
            .insert("ctx:regime:multiplier".to_string(), json!(multiplier));
        shared
            .data
            .insert("ctx:regime:score".to_string(), json!(score));

        Ok(AnalysisResult {
            score,
            weight_multiplier: multiplier,
            description: description.join(" | "),
            debug_data: json!({
                "vol_p": vol_p,
                "final_multiplier": multiplier,
                "oi_state": trend_data.oi_data.as_ref().map(|d| format!("{:?}", d.state))
            }),
            ..Default::default()
        })
    }
}
