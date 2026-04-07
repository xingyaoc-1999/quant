use common::Symbol;
use quant::{analyzer::AnalysisEngine, report::AnalysisAudit};
use rig::{completion::ToolDefinition, tool::Tool};
use schemars::JsonSchema;
use serde::Deserialize;
use service::context::FeatureContextManager;
use std::{str::FromStr, sync::Arc};
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub enum ScoringToolError {
    #[error("数据查询失败: {0}")]
    QueryError(String),
    #[error("币种未找到: {0}")]
    SymbolNotFound(String),
}

#[derive(Deserialize, JsonSchema)]
pub struct ScoreQueryArgs {
    #[schemars(description = "交易对符号，例如 BTCUSDT")]
    pub symbol: String,
}

pub struct ScoreQueryTool {
    // 直接持有管理器和引擎
    manager: Arc<FeatureContextManager>,
    engine: Arc<AnalysisEngine>,
}
impl ScoreQueryTool {
    pub fn new(manager: Arc<FeatureContextManager>, engine: Arc<AnalysisEngine>) -> Self {
        Self { manager, engine }
    }
}

impl Tool for ScoreQueryTool {
    const NAME: &'static str = "get_scoring_analysis";

    type Error = ScoringToolError;
    type Args = ScoreQueryArgs;
    type Output = AnalysisAudit; // 目标输出类型

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let parameters = serde_json::to_value(schemars::schema_for!(ScoreQueryArgs))
            .expect("Failed to serialize schema");
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description:
                "获取指定交易对的系统深度分析报告。包含物理快照、市场结构、博弈状态及关键引力位。"
                    .into(),
            parameters,
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let symbol = Symbol::from_str(&args.symbol)
            .map_err(|_| ScoringToolError::SymbolNotFound(args.symbol.to_string()))?;

        let mut market_context = self
            .manager
            .get_market_context(symbol)
            .ok_or_else(|| ScoringToolError::SymbolNotFound(args.symbol))?;

        let report = self.engine.run(&mut market_context);

        Ok(report)
    }
    fn name(&self) -> String {
        Self::NAME.to_string()
    }
}
