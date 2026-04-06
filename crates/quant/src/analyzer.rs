use crate::types::{CorrelationConflict, DerivativeSnapshot, FeatureSet};
use chrono::{DateTime, Utc};
use common::{Candle, Interval, Symbol};
use core::fmt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, str::FromStr};
use tracing::{debug, error, warn};
mod context;
// ==========================================
// 1. 常量与上下文 Key (类型安全)
// ==========================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextKey {
    // 波动率相关
    VolAtrRatio,
    VolIsCompressed,
    VolPercentile,
    VolumeState,
    // 市场模式相关
    RegimeStructure,
    MarketCorrelation,
    // 乘数相关
    MultRegimeBase,
    MultMomentum,
    MultOi,
    MultLongSpace,
    MultShortSpace,
    // 空间与位置
    SpaceGravityWells,
}

impl ContextKey {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::VolAtrRatio => "ctx:vol:atr_ratio",
            Self::VolIsCompressed => "ctx:vol:is_compressed",
            Self::VolPercentile => "ctx:vol:percentile",
            Self::RegimeStructure => "ctx:regime:structure",
            Self::MarketCorrelation => "ctx:market:correlation",
            Self::MultRegimeBase => "multiplier:regime:base",
            Self::MultMomentum => "multiplier:regime:momentum",
            Self::MultOi => "multiplier:regime:oi",
            Self::MultLongSpace => "multiplier:space:long",
            Self::MultShortSpace => "multiplier:space:short",
            Self::SpaceGravityWells => "ctx:space:gravity_wells",
        }
    }
}

// ==========================================
// 2. 角色定义与枚举
// ==========================================

#[derive(
    Debug, Hash, Eq, PartialEq, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialOrd, Ord,
)]
pub enum Role {
    Entry,
    Filter,
    Trend,
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
            Self::Entry => "Entry",
            Self::Filter => "Filter",
            Self::Trend => "Trend",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.icon(), self.as_str())
    }
}

impl FromStr for Role {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Entry" => Ok(Self::Entry),
            "Filter" => Ok(Self::Filter),
            "Trend" => Ok(Self::Trend),
            _ => Err(format!("Unknown role: '{}'", s)),
        }
    }
}

// ==========================================
// 3. 核心业务逻辑 (持仓量与流向)
// ==========================================

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum OIPositionState {
    LongBuildUp,   // 价升量增
    ShortBuildUp,  // 价跌量增
    LongUnwinding, // 价跌量减
    ShortCovering, // 价升量减
    Neutral,
}

impl OIPositionState {
    pub fn determine(price_change: f64, oi_change: f64) -> Self {
        const EPSILON: f64 = 1e-6;
        if oi_change.abs() < EPSILON {
            return Self::Neutral;
        }
        match (price_change > 0.0, oi_change > 0.0) {
            (true, true) => Self::LongBuildUp,
            (false, true) => Self::ShortBuildUp,
            (false, false) => Self::LongUnwinding,
            (true, false) => Self::ShortCovering,
        }
    }

    pub fn signal_score(&self) -> f64 {
        match self {
            Self::LongBuildUp => 1.0,
            Self::ShortCovering => 0.5,
            Self::Neutral => 0.0,
            Self::LongUnwinding => -0.5,
            Self::ShortBuildUp => -1.0,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TakerFlowData {
    pub buy_vol: f64,
    pub sell_vol: f64,
    pub net_vol: f64,
    pub buy_sell_ratio: Option<f64>, // 改为 Option，避免除以零
}

impl TakerFlowData {
    pub fn from_candle(candle: &Candle) -> Self {
        let buy_vol = candle.taker_buy_volume;
        let sell_vol = (candle.volume - candle.taker_buy_volume).max(0.0);
        let net_vol = buy_vol - sell_vol;
        let ratio = if sell_vol > 0.0 {
            Some(buy_vol / sell_vol)
        } else if buy_vol > 0.0 {
            Some(f64::INFINITY)
        } else {
            None
        };
        Self {
            buy_vol,
            sell_vol,
            net_vol,
            buy_sell_ratio: ratio,
        }
    }
}

// ==========================================
// 4. 分析结果与分析器
// ==========================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
    pub kind: AnalyzerKind,
    pub score: f64,
    pub is_violation: bool,
    pub weight_multiplier: f64,
    pub description: String,
    pub tag: String,
    pub rationale: Vec<String>,
    pub debug_data: Value,
    pub conflict: Option<CorrelationConflict>,
}

impl AnalysisResult {
    pub fn new(kind: AnalyzerKind, tag: String) -> Self {
        Self {
            kind,
            tag,
            ..Default::default()
        }
    }

    pub fn because(mut self, s: impl Into<String>) -> Self {
        self.rationale.push(s.into());
        self
    }

    pub fn violate(mut self) -> Self {
        self.is_violation = true;
        self
    }

    pub fn with_score(mut self, s: f64) -> Self {
        self.score = s;
        self
    }

    pub fn with_mult(mut self, mult: f64) -> Self {
        self.weight_multiplier = mult;
        self
    }

    pub fn with_desc(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn debug(mut self, data: Value) -> Self {
        self.debug_data = data;
        self
    }

    pub fn to_ai_prompt(&self) -> String {
        format!(
            "[{}] Type: {:?}, Score: {:.1}, Context_Weight_Mult: {:.2}\n  - Reason: {}\n  - Data Snapshot: {}",
            self.tag,
            self.kind,
            self.score,
            self.weight_multiplier,
            self.rationale.join("; "),
            self.debug_data.to_string()
        )
    }
}

impl Default for AnalysisResult {
    fn default() -> Self {
        Self {
            score: 0.0,
            is_violation: false,
            weight_multiplier: 1.0,
            description: "PENDING".into(),
            tag: "NONE".to_owned(),
            rationale: vec![],
            debug_data: json!({}),
            kind: AnalyzerKind::MarketRegime,
            conflict: None,
        }
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

impl AnalyzerKind {
    /// 返回该分析器类型对应的乘数 ContextKey（如果有）
    pub fn multiplier_key(&self) -> Option<ContextKey> {
        match self {
            Self::MarketRegime => Some(ContextKey::MultRegimeBase),
            Self::Momentum => Some(ContextKey::MultMomentum),
            Self::VolumeProfile => Some(ContextKey::MultOi),
            Self::TrendStrength => Some(ContextKey::MultLongSpace),
            Self::Divergence => Some(ContextKey::MultShortSpace),
            _ => None, // 不支持动态乘数
        }
    }
}

pub trait Analyzer: Send + Sync {
    fn name(&self) -> &'static str;
    fn kind(&self) -> AnalyzerKind;
    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError>;
}

// ==========================================
// 5. 市场上下文 (包含共享缓存)
// ==========================================

#[derive(Debug, Clone)]
pub struct MarketContext {
    pub symbol: Symbol,
    pub timestamp: DateTime<Utc>,
    pub roles: HashMap<Role, RoleData>,
    pub global: DerivativeSnapshot,
    pub cache: HashMap<ContextKey, Value>, // analyzer 间共享的中间结果
}

impl MarketContext {
    pub fn new(symbol: Symbol, timestamp: DateTime<Utc>) -> Self {
        Self {
            symbol,
            timestamp,
            roles: HashMap::new(),
            global: DerivativeSnapshot::default(),
            cache: HashMap::new(),
        }
    }

    pub fn get_role(&self, role: Role) -> Result<&RoleData, AnalysisError> {
        self.roles
            .get(&role)
            .ok_or(AnalysisError::InsufficientData(role))
    }

    pub fn with_role(mut self, role: Role, data: RoleData) -> Self {
        self.roles.insert(role, data);
        self
    }

    /// 从缓存获取值，自动反序列化
    pub fn get_cached<T: serde::de::DeserializeOwned>(&self, key: ContextKey) -> Option<T> {
        self.cache
            .get(&key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// 存入缓存，自动序列化
    pub fn set_cached<T: Serialize>(&mut self, key: ContextKey, value: T) {
        self.cache.insert(key, json!(value));
    }

    /// 获取分析器对应的动态乘数
    pub fn get_multiplier(&self, kind: AnalyzerKind) -> f64 {
        match kind.multiplier_key() {
            Some(key) => self.get_cached::<f64>(key).unwrap_or(1.0),
            None => 1.0,
        }
    }

    /// 设置分析器对应的动态乘数
    pub fn set_multiplier(&mut self, kind: AnalyzerKind, value: f64) {
        if let Some(key) = kind.multiplier_key() {
            self.set_cached(key, value);
        } else {
            debug!(
                "Attempted to set multiplier for kind {:?} which has no multiplier key",
                kind
            );
        }
    }
}

#[derive(Debug, Clone)]
pub struct RoleData {
    pub interval: Interval,
    pub feature_set: FeatureSet,
    pub taker_flow: TakerFlowData,
    pub oi_data: Option<OIData>,
}

// ==========================================
// 6. OI 数据
// ==========================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OIData {
    pub current_oi_amount: f64,
    pub current_oi_value: f64,
    pub change_history: Vec<f64>,
}

impl OIData {
    pub fn new(amount: f64, value: f64, history: Vec<f64>) -> Self {
        Self {
            current_oi_amount: amount,
            current_oi_value: value,
            change_history: history,
        }
    }

    /// 计算最新一段 OI 变化率
    pub fn delta_ratio(&self) -> f64 {
        if self.change_history.is_empty() {
            return 0.0;
        }
        self.change_history.last().cloned().unwrap_or(0.0) / self.current_oi_amount.max(1.0)
    }
}

// ==========================================
// 7. 配置与最终信号
// ==========================================

#[derive(Debug, Clone)]
pub struct Config {
    pub weights: HashMap<AnalyzerKind, f64>,
    pub sensitivity: f64,
}

impl Config {
    /// 验证配置的有效性
    pub fn validate(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.sensitivity) {
            return Err(format!(
                "sensitivity must be in [0,1], got {}",
                self.sensitivity
            ));
        }
        for (kind, &weight) in &self.weights {
            if weight < 0.0 {
                return Err(format!("weight for {:?} is negative: {}", kind, weight));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FinalSignal {
    pub symbol: Symbol,
    pub net_score: f64,
    pub is_rejected: bool,
    pub reason: String,
    pub sub_reports: Vec<AnalysisResult>,
    pub market_snapshot: Value,
}

impl FinalSignal {
    fn new_with_reports(symbol: Symbol, score: f64, reports: Vec<AnalysisResult>) -> Self {
        Self {
            symbol,
            net_score: score,
            is_rejected: false,
            reason: "OK".into(),
            sub_reports: reports,
            market_snapshot: json!({}),
        }
    }

    fn rejected_with_reports(symbol: Symbol, tag: &str, reports: Vec<AnalysisResult>) -> Self {
        Self {
            symbol,
            net_score: 0.0,
            is_rejected: true,
            reason: format!("Violation triggered by: {}", tag),
            sub_reports: reports,
            market_snapshot: json!({}),
        }
    }
}

// ==========================================
// 8. 执行引擎
// ==========================================

pub struct AnalysisEngine {
    pub analyzers: Vec<Box<dyn Analyzer>>,
    pub config: Config,
}

impl AnalysisEngine {
    pub fn new(config: Config) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            analyzers: Vec::new(),
            config,
        })
    }

    pub fn add_analyzer(&mut self, analyzer: Box<dyn Analyzer>) {
        debug!("Adding analyzer: {}", analyzer.name());
        self.analyzers.push(analyzer);
    }

    /// 按添加顺序依次执行所有分析器
    pub fn run(&mut self, mut ctx: MarketContext) -> FinalSignal {
        let mut results = Vec::new();
        let mut errors = Vec::new();

        for analyzer in &self.analyzers {
            match analyzer.analyze(&mut ctx) {
                Ok(res) => {
                    debug!(
                        "Analyzer {} produced score: {:.3}",
                        analyzer.name(),
                        res.score
                    );
                    results.push(res);
                }
                Err(e) => {
                    warn!("Analyzer {} failed: {}", analyzer.name(), e);
                    errors.push((analyzer.name(), e));
                }
            }
        }

        if !errors.is_empty() {
            error!("{} analyzer(s) failed during analysis", errors.len());
        }

        self.aggregate(ctx, results)
    }

    fn aggregate(&self, ctx: MarketContext, results: Vec<AnalysisResult>) -> FinalSignal {
        if let Some(violation) = results.iter().find(|r| r.is_violation) {
            let tag = violation.tag.clone();
            return FinalSignal::rejected_with_reports(ctx.symbol.clone(), &tag, results);
        }

        let mut total_score = 0.0;
        let mut total_weight = 0.0;

        for res in &results {
            let base_weight = self.config.weights.get(&res.kind).cloned().unwrap_or(1.0);
            let dyn_mult = ctx.get_multiplier(res.kind);
            let weight = base_weight * dyn_mult * res.weight_multiplier;
            total_score += res.score * weight;
            total_weight += weight;
        }

        let net_score = if total_weight > 0.0 {
            total_score / total_weight
        } else {
            0.0
        };

        FinalSignal::new_with_reports(ctx.symbol, net_score, results)
    }
}

// ==========================================
// 9. 错误类型
// ==========================================

#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error("Missing data for role {0:?}")]
    InsufficientData(Role),
    #[error("Calculation error: {0}")]
    Calculation(String),
}
