use chrono::{DateTime, Utc};
use common::{Candle, Interval, Symbol};
use core::fmt;
use dashmap::DashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, str::FromStr};

use crate::types::{CorrelationConflict, DerivativeSnapshot, FeatureSet, LogicComponent};
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum OITrend {
    Increasing, // 持仓累积
    Decreasing, // 持仓流失
    Stable,     // 持仓平稳
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum OIPositionState {
    LongBuildUp,   // 价升量增：多头开仓
    ShortBuildUp,  // 价跌量增：空头开仓
    LongUnwinding, // 价跌量减：多头平仓（止损/止盈）
    ShortCovering, // 价升量减：空头平仓（止损/止盈）
    Neutral,
}

#[derive(Debug, Clone)]
pub struct RoleData {
    pub interval: Interval,
    pub feature_set: FeatureSet,
    pub taker_flow: TakerFlowData,
    pub oi_data: Option<OIData>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OIData {
    pub current_oi: f64,
    pub oi_ma_fast: Option<f64>, // 比如 MA20
    pub oi_ma_slow: Option<f64>, // 比如 MA50

    // 相对变化率 (当前值 vs N个周期前)
    pub change_rate: f64,
    pub trend: OITrend,
    pub state: OIPositionState,

    pub oi_strength: f64, // 0.0 到 100.0
    pub oi_signal: f64,   // -1.0 (极度看空) 到 1.0 (极度看多)
}
impl OIData {
    /// 根据价格变化和OI变化计算市场状态
    pub fn calculate_state(&mut self, price_change_pct: f64, oi_change_pct: f64) {
        self.change_rate = oi_change_pct;

        // 1. 确定趋势
        self.trend = if oi_change_pct > 0.02 {
            // 阈值可配置
            OITrend::Increasing
        } else if oi_change_pct < -0.02 {
            OITrend::Decreasing
        } else {
            OITrend::Stable
        };

        // 2. 确定博弈状态 (经典四象限)
        self.state = match (price_change_pct > 0.0, oi_change_pct > 0.0) {
            (true, true) => OIPositionState::LongBuildUp, // 强势看涨
            (false, true) => OIPositionState::ShortBuildUp, // 强势看跌
            (false, false) => OIPositionState::LongUnwinding, // 弱势回调
            (true, false) => OIPositionState::ShortCovering, // 空头回补
        };

        // 3. 计算信号得分 (简易示例)
        self.oi_signal = match self.state {
            OIPositionState::LongBuildUp => 1.0,
            OIPositionState::ShortCovering => 0.5,
            OIPositionState::LongUnwinding => -0.5,
            OIPositionState::ShortBuildUp => -1.0,
            OIPositionState::Neutral => 0.0,
        };
    }
}
#[derive(Debug, Clone, Default)]
pub struct TakerFlowData {
    pub buy_vol: f64,        // 该角色周期内的主动买入量
    pub sell_vol: f64,       // 该角色周期内的主动卖出量
    pub net_vol: f64,        // 净主动量
    pub buy_sell_ratio: f64, // 主动买卖比
}
impl TakerFlowData {
    pub fn from_candle(candle: &Candle) -> Self {
        let buy_vol = candle.taker_buy_volume;
        let sell_vol = candle.volume - candle.taker_buy_volume;
        let net_vol = buy_vol - sell_vol;

        let buy_sell_ratio = match (buy_vol, sell_vol) {
            (_, 0.0) if buy_vol > 0.0 => f64::INFINITY,
            (0.0, 0.0) => 0.0,
            (_, 0.0) => 0.0,
            _ => buy_vol / sell_vol,
        };

        Self {
            buy_vol,
            sell_vol,
            net_vol,
            buy_sell_ratio,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MarketContext {
    pub symbol: Symbol,
    pub timestamp: DateTime<Utc>,
    pub roles: HashMap<Role, RoleData>,
    pub current_price: f64,

    pub global: DerivativeSnapshot,
}

impl MarketContext {
    pub fn new(symbol: Symbol, timestamp: DateTime<Utc>, price: f64) -> Self {
        Self {
            symbol,
            timestamp,
            current_price: price,
            roles: HashMap::new(),
            global: DerivativeSnapshot::default(),
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
    FuturesGameTheory,
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
