use chrono::{DateTime, Utc};
use common::{Interval, Symbol};
use core::fmt;
use dashmap::DashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, str::FromStr};

use crate::types::{CorrelationConflict, FeatureSet, LiquidationZone, LogicComponent};
mod audit;
mod context;
mod signal;

#[derive(
    Debug, Hash, Eq, PartialEq, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialOrd, Ord,
)]
pub enum Role {
    Entry,
    Filter,
    Trend,
}
impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.icon(), self.as_str())
    }
}
impl Role {
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Entry => "🎯",
            Self::Filter => "🔍",
            Self::Trend => "📈",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Entry => "入场",
            Self::Filter => "过滤",
            Self::Trend => "趋势",
        }
    }
}
impl FromStr for Role {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Entry" => Ok(Self::Entry),
            "Filter" => Ok(Self::Filter),
            "Trend" => Ok(Self::Trend),
            _ => Err(format!(
                "Unknown role: '{}'. Valid roles are: Entry, Filter, Trend",
                s
            )),
        }
    }
}
#[derive(Debug, Clone)]
pub struct RoleData {
    pub interval: Interval,
    pub feature_set: FeatureSet,
}

#[derive(Debug, Clone, Default)]
pub struct MarketContext {
    pub symbol: Symbol,
    pub timestamp: DateTime<Utc>,
    pub roles: HashMap<Role, RoleData>,
    pub current_price: f64,
    pub open_interest: Option<f64>,
    pub funding_rate: Option<f64>,
    pub liquidation_levels: Vec<LiquidationZone>,
}

impl MarketContext {
    pub fn new(symbol: Symbol, timestamp: DateTime<Utc>, price: f64) -> Self {
        Self {
            symbol,
            timestamp,
            current_price: price,
            roles: HashMap::new(),
            open_interest: None,
            funding_rate: None,
            liquidation_levels: Vec::new(),
        }
    }
    pub fn get_role(&self, role: Role) -> &RoleData {
        self.roles
            .get(&role)
            .expect("Role not initialized in context")
    }
    pub fn with_role(mut self, role: Role, data: RoleData) -> Self {
        self.roles.insert(role, data);
        self
    }

    pub fn is_futures(&self) -> bool {
        self.open_interest.is_some() || self.funding_rate.is_some()
    }
    pub fn with_futures_data(mut self, oi: Option<f64>, funding: Option<f64>) -> Self {
        self.open_interest = oi;
        self.funding_rate = funding;
        self
    }
}

#[derive(Debug, Hash, Eq, PartialEq, Clone, Copy, Serialize, Deserialize)]
pub enum AnalyzerKind {
    TrendStrength,
    Momentum,
    VolumeProfile,
    Divergence,
    SupportResistance,
    Volatility,
    MarketRegime,
}

/// 全局配置
#[derive(Debug, Clone)]
pub struct Config {
    pub weights: HashMap<AnalyzerKind, f64>,
    /// 灵敏度参数
    pub sensitivity: f64,
    /// 止损/止盈的全局乘数 (用于计算建议的 RR)
    pub risk_config: RiskConfig,
}

impl Config {
    /// 创建带默认权重的配置
    pub fn with_defaults() -> Self {
        let mut weights = HashMap::new();
        weights.insert(AnalyzerKind::TrendStrength, 0.3);
        weights.insert(AnalyzerKind::Momentum, 0.2);
        weights.insert(AnalyzerKind::VolumeProfile, 0.15);
        weights.insert(AnalyzerKind::Divergence, 0.1);
        weights.insert(AnalyzerKind::SupportResistance, 0.15);
        weights.insert(AnalyzerKind::Volatility, 0.1);
        Self {
            weights,
            sensitivity: 1.0,
            risk_config: RiskConfig::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RiskConfig {
    pub atr_multiplier: f64, // 止损距离 ATR 的倍数
    pub min_rr_ratio: f64,   // 接受的最小盈亏比
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            atr_multiplier: 1.5,
            min_rr_ratio: 2.0,
        }
    }
}

// ==================== 分析器 Trait 与共享状态 ====================

/// 分析过程中可选的共享状态，用于分析器间传递中间结果
#[derive(Debug, Default, Clone)]
pub struct SharedAnalysisState {
    pub data: DashMap<String, Value>,
}

#[derive(Debug, Clone)]
pub struct AnalysisResult {
    pub score: f64,         // 原始评分 (-100 到 100)
    pub is_violation: bool, // 一票否决标识
    pub weight_multiplier: f64,
    pub description: String, // 喂给 AI 的核心描述
    pub conflict: Option<CorrelationConflict>,
    pub debug_data: Value, // 原始参数
}
impl AnalysisResult {
    pub fn to_component(&self, analyzer_name: &str) -> LogicComponent {
        LogicComponent {
            id: analyzer_name.to_string(),

            score: self.score,

            // 将分析器内部的 description 转化为 AI 可读的描述
            // 如果内部没有描述，则使用默认文案
            desc: if self.description.is_empty() {
                format!("Analysis from {}", analyzer_name)
            } else {
                self.description.clone()
            },
        }
    }
}
impl Default for AnalysisResult {
    fn default() -> Self {
        Self {
            score: 0.0,
            is_violation: false,
            weight_multiplier: 1.0,
            description: "NO_SIGNAL".to_string(),
            conflict: None,
            debug_data: serde_json::json!({}),
        }
    }
}
/// 分析器可能产生的错误
#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error("Insufficient data for role {0:?}")]
    InsufficientData(Role),
    #[error("Calculation error: {0}")]
    Calculation(String),
    #[error("Configuration error: {0}")]
    Config(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum AnalyzerStage {
    Context,
    Signal,
    Audit,
}
pub trait Analyzer: Send + Sync {
    fn name(&self) -> &'static str;
    fn stage(&self) -> AnalyzerStage;
    fn kind(&self) -> AnalyzerKind;
    fn analyze(
        &self,
        ctx: &MarketContext,
        config: &Config,
        shared: &SharedAnalysisState,
    ) -> Result<AnalysisResult, AnalysisError>;
}
