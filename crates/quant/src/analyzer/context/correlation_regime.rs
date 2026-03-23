// use crate::analyzer::{
//     AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, AnalyzerStage, Config, MarketContext,
//     Role, SharedAnalysisState,
// };
// use crate::types::TrendStructure;
// use serde_json::json;
// use std::borrow::Cow;
//
// pub struct CorrelationRegimeAnalyzer;
//
// impl Analyzer for CorrelationRegimeAnalyzer {
//     fn name(&self) -> &'static str {
//         "correlation_regime"
//     }
//
//     fn stage(&self) -> AnalyzerStage {
//         AnalyzerStage::Context
//     }
//
//     fn kind(&self) -> AnalyzerKind {
//         // 属于市场环境/大盘画像类
//         AnalyzerKind::MarketRegime
//     }
//
//     fn analyze(
//         &self,
//         ctx: &MarketContext,
//         _config: &Config,
//         shared: &SharedAnalysisState,
//     ) -> Result<AnalysisResult, AnalysisError> {
//         // 1. 获取大盘（通常是 Role::Global 或直接硬编码获取 BTC 角色）的数据
//         // 假设你的 ctx 路由中包含 Global 角色代表 BTC
//         let btc_feat = &btc_data.feature_set;
//
//         // 2. 获取当前品种的数据进行对比
//         let local_data = ctx.get_role(Role::Trend);
//
//         let mut multiplier = 1.0;
//         let mut description = Vec::new();
//
//         // --- 逻辑 A：大盘系统性风险过滤 ---
//         if let Some(btc_struct) = btc_feat.structure.trend_structure {
//             match btc_struct {
//                 TrendStructure::StrongBearish | TrendStructure::Bearish => {
//                     // 如果 BTC 处于明显的下跌趋势，压制所有多头信号
//                     multiplier *= 0.4;
//                     description.push(Cow::Borrowed("GLOBAL_BEARISH_DRAG:HIGH_RISK"));
//                 }
//                 TrendStructure::StrongBullish => {
//                     // BTC 强势，给所有多头信号加成
//                     multiplier *= 1.2;
//                     description.push(Cow::Borrowed("GLOBAL_BULLISH_TAILWIND"));
//                 }
//                 _ => {}
//             }
//         }
//
//         // --- 逻辑 B：相关性背离检测 (Beta 分析) ---
//         // 假设 feature_set 中预计算了与 BTC 的相关系数 (correlation_coefficient)
//         if let Some(corr) = local_data.feature_set.structure.correlation_with_global {
//             if corr < 0.3 {
//                 // 相关性极低，说明该品种正在走独立行情
//                 multiplier *= 0.9; // 略微降权，因为独立行情容易被大盘“吸血”或“误伤”
//                 description.push(Cow::Borrowed("LOW_CORRELATION:INDEPENDENT_MOVE"));
//             } else if corr > 0.9 {
//                 // 极高相关性，完全跟随大盘
//                 description.push(Cow::Borrowed("HIGH_CORRELATION:INDEX_SYNC"));
//             }
//         }
//
//         // --- 3. 广播大盘因子 ---
//         shared
//             .data
//             .insert("context:global_multiplier".into(), json!(multiplier));
//
//         Ok(AnalysisResult {
//             description: description.join(" | "),
//             debug_data: json!({
//                 "btc_structure": btc_feat.structure.trend_structure,
//                 "applied_global_multiplier": multiplier
//             }),
//             ..Default::default()
//         })
//     }
// }
