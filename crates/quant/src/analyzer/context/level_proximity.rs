use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, AnalyzerStage, Config, MarketContext,
    Role, SharedAnalysisState,
};
use crate::types::PriceGravityWell;
use serde_json::json;
use std::borrow::Cow;

/// 空间位置分析器 (Context 阶段)
/// 职责：基于 SpaceGeometry 评估价格与关键位的距离，产生环境乘数并记录空间重力场
pub struct LevelProximityAnalyzer;

impl Analyzer for LevelProximityAnalyzer {
    fn name(&self) -> &'static str {
        "level_proximity"
    }

    fn stage(&self) -> AnalyzerStage {
        AnalyzerStage::Context
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::SupportResistance
    }

    fn analyze(
        &self,
        ctx: &MarketContext,
        _config: &Config,
        shared: &SharedAnalysisState,
    ) -> Result<AnalysisResult, AnalysisError> {
        // 1. 从 Trend 角色中获取空间几何数据 (SpaceGeometry)
        let trend_data = ctx.get_role(Role::Trend);
        let space = &trend_data.feature_set.space;
        let indicators = &trend_data.feature_set.indicators;

        let mut multiplier = 1.0;
        let mut description: Vec<Cow<'static, str>> = Vec::new();
        let mut gravity_wells: Vec<PriceGravityWell> = Vec::new();

        // --- 2. 压力位分析 (Resistance) ---
        if let Some(dist_res) = space.dist_to_resistance {
            // 将绝对价格（如果字段存的是价格）或相对距离转换为 GravityWell
            // 假设 FeatureSet 里的 dist_to_resistance 是相对百分比
            gravity_wells.push(PriceGravityWell {
                level: ctx.current_price * (1.0 + dist_res),
                source: "Major_Resistance".to_string(),
                distance_pct: dist_res,
            });

            if dist_res > 0.0 && dist_res < 0.006 {
                multiplier *= 0.45; // 贴近压力位，多头风险极高
                description.push(Cow::Borrowed("NEAR_RESISTANCE:UPSIDE_LOCKED"));
            }
        }

        // --- 3. 支撑位分析 (Support) ---
        if let Some(dist_sup) = space.dist_to_support {
            gravity_wells.push(PriceGravityWell {
                level: ctx.current_price * (1.0 - dist_sup),
                source: "Major_Support".to_string(),
                distance_pct: -dist_sup,
            });

            if dist_sup > 0.0 && dist_sup < 0.006 {
                multiplier *= 1.35; // 踩在支撑上，高置信度背景
                description.push(Cow::Borrowed("SUPPORT_BOUNCE_ZONE"));
            }
        }

        // --- 4. 关键均线分析 (MA200 作为长期牛熊分界) ---
        if let Some(ma200) = indicators.ma_200 {
            let dist_ma200 = (ma200 - ctx.current_price) / ctx.current_price;
            gravity_wells.push(PriceGravityWell {
                level: ma200,
                source: "MA200_Trend_Line".to_string(),
                distance_pct: dist_ma200,
            });

            // 如果价格在 MA200 下方贴得很近，通常是极强的压力
            if dist_ma200 > 0.0 && dist_ma200 < 0.005 {
                multiplier *= 0.7;
                description.push(Cow::Borrowed("MA200_RESISTANCE"));
            }
        }

        // --- 5. 状态持久化 (供 Aggregator 和 Builder 使用) ---

        // 广播给其他分析器的乘数
        shared
            .data
            .insert("context:space_multiplier".into(), json!(multiplier));

        // 存入重力井数据，方便最终构造 AIAuditPayload
        shared
            .data
            .insert("context:gravity_wells".into(), json!(gravity_wells));

        Ok(AnalysisResult {
            description: description.join(" | "),
            debug_data: json!({
                "wells_count": gravity_wells.len(),
                "applied_multiplier": multiplier
            }),
            ..Default::default()
        })
    }
}
