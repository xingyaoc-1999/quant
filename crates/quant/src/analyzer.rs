use crate::types::{CorrelationConflict, DerivativeSnapshot, FeatureSet};
use chrono::{DateTime, Utc};
use common::{Candle, Interval, Symbol};
use core::fmt;
use dashmap::{mapref::one::Ref, DashMap};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, str::FromStr};
mod context;
// ==========================================
// 1. 常量与上下文 Key (类型安全)
// ==========================================

const MULTIPLIER_PREFIX: &str = "multiplier:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextKey {
    // 波动率相关
    VolAtrRatio,
    VolIsCompressed,
    VolPercentile,
    // 市场模式相关
    RegimeStructure,
    MarketCorrelation, // 新增：大盘相关性
    // 乘数相关 (用于不同分析阶段的权重传递)
    MultRegimeBase,
    MultMomentum,
    MultOi,
    MultLongSpace,  // 空间维度的多头修正
    MultShortSpace, // 空间维度的空头修正
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
        if oi_change.abs() < 1e-4 {
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
    pub buy_sell_ratio: f64,
}

impl TakerFlowData {
    pub fn from_candle(candle: &Candle) -> Self {
        let buy_vol = candle.taker_buy_volume;
        let sell_vol = (candle.volume - candle.taker_buy_volume).max(0.0);
        let net_vol = buy_vol - sell_vol;
        let ratio = if sell_vol > 0.0 {
            buy_vol / sell_vol
        } else {
            0.0
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
// 4. 共享状态与隔离作用域
// ==========================================

#[derive(Debug, Default, Clone)]
pub struct SharedAnalysisState {
    pub global: DashMap<String, Value>,
    pub symbols: DashMap<Symbol, DashMap<String, Value>>,
}

impl SharedAnalysisState {
    pub fn scope<'a>(&'a self, symbol: &'a Symbol) -> SymbolScope<'a> {
        self.symbols
            .entry(symbol.clone())
            .or_insert_with(DashMap::new);
        let symbol_map_ref = self.symbols.get(symbol).expect("Safe");
        SymbolScope {
            symbol,
            global: &self.global,
            inner: symbol_map_ref,
        }
    }
}

pub struct SymbolScope<'a> {
    pub symbol: &'a Symbol,
    pub global: &'a DashMap<String, Value>,
    pub inner: Ref<'a, Symbol, DashMap<String, Value>>,
}

impl<'a> SymbolScope<'a> {
    pub fn get_f64(&self, key: ContextKey) -> Option<f64> {
        self.get_val(key).and_then(|v| v.as_f64())
    }

    pub fn get_bool(&self, key: ContextKey) -> bool {
        self.get_val(key).and_then(|v| v.as_bool()).unwrap_or(false)
    }

    pub fn get_val(&self, key: ContextKey) -> Option<Value> {
        self.inner.value().get(key.as_str()).map(|v| v.clone())
    }

    pub fn insert_ctx(&self, key: ContextKey, val: Value) {
        self.inner.value().insert(key.as_str().to_string(), val);
    }

    pub fn set_multiplier(&self, kind: AnalyzerKind, val: f64) {
        let key = format!("{}{:?}", MULTIPLIER_PREFIX, kind);
        self.inner.value().insert(key, json!(val));
    }

    pub fn extract_multipliers(&self) -> HashMap<String, f64> {
        self.inner
            .value()
            .iter()
            .filter(|r| r.key().starts_with(MULTIPLIER_PREFIX))
            .map(|r| {
                let clean_key = r.key().trim_start_matches(MULTIPLIER_PREFIX).to_string();
                (clean_key, r.value().as_f64().unwrap_or(1.0))
            })
            .collect()
    }
}

// ==========================================
// 5. 分析结果与分析器 (AI 友好改造)
// ==========================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
    pub kind: AnalyzerKind, // 核心身份标识
    pub score: f64,
    pub is_violation: bool,
    pub weight_multiplier: f64,
    pub description: String,
    pub tag: String,
    pub rationale: Vec<String>, // 核心证据链 (发给AI的关键)
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

    /// 辅助方法：将当前结果格式化为对大模型友好的文本提示
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

// ==========================================
// 6. 执行引擎与聚合逻辑
// ==========================================

pub struct AnalysisEngine {
    pub analyzers: Vec<Box<dyn Analyzer>>,
    pub shared_state: SharedAnalysisState,
    pub config: Config,
}

impl AnalysisEngine {
    pub fn new(config: Config) -> Self {
        Self {
            analyzers: Vec::new(),
            shared_state: SharedAnalysisState::default(),
            config,
        }
    }

    pub fn add_analyzer(&mut self, analyzer: Box<dyn Analyzer>) {
        self.analyzers.push(analyzer);
    }

    pub fn run(&self, ctx: &MarketContext) -> FinalSignal {
        let mut results = Vec::new();

        // 按阶段排序执行：保证 Context 先于 Signal 运行
        let mut sorted = self.analyzers.iter().collect::<Vec<_>>();
        sorted.sort_by_key(|a| match a.stage() {
            AnalyzerStage::Context => 0,
            AnalyzerStage::Signal => 1,
            AnalyzerStage::Audit => 2,
        });

        for analyzer in sorted {
            if let Ok(res) = analyzer.analyze(ctx, &self.config, &self.shared_state) {
                results.push(res);
            }
        }

        self.aggregate(&ctx.symbol, results)
    }

    fn aggregate(&self, symbol: &Symbol, results: Vec<AnalysisResult>) -> FinalSignal {
        let scope = self.shared_state.scope(symbol);
        let dyn_multipliers = scope.extract_multipliers();

        let mut total_score = 0.0;
        let mut total_weight = 0.0;

        // 克隆报告以便传递给 AI
        let reports = results.clone();

        for res in results {
            if res.is_violation {
                return FinalSignal::rejected_with_reports(
                    symbol.clone(),
                    res.tag.as_str(),
                    reports,
                );
            }

            // 1. 基础权重 (来自 Config)
            let base_weight = self.config.weights.get(&res.kind).cloned().unwrap_or(1.0);

            // 2. 动态环境乘数 (从 Context 阶段读取)
            let kind_name = format!("{:?}", res.kind);
            let dyn_mult = dyn_multipliers.get(&kind_name).cloned().unwrap_or(1.0);

            // 3. 计算最终有效权重
            let weight = base_weight * dyn_mult * res.weight_multiplier;

            total_score += res.score * weight;
            total_weight += weight;
        }

        // 安全计算最终得分，避免除以0
        let net_score = if total_weight > 0.0 {
            total_score / total_weight
        } else {
            0.0
        };

        FinalSignal::new_with_reports(*symbol, net_score, reports)
    }
}

// ==========================================
// 7. 数据结构补全
// ==========================================

#[derive(Debug, Clone)]
pub struct MarketContext {
    pub symbol: Symbol,
    pub timestamp: DateTime<Utc>,
    pub roles: HashMap<Role, RoleData>,
    pub global: DerivativeSnapshot,
}

impl MarketContext {
    pub fn new(symbol: Symbol, timestamp: DateTime<Utc>) -> Self {
        Self {
            symbol,
            timestamp,
            roles: HashMap::new(),
            global: DerivativeSnapshot::default(),
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
}

#[derive(Debug, Clone)]
pub struct RoleData {
    pub interval: Interval,
    pub feature_set: FeatureSet,
    pub taker_flow: TakerFlowData,
    pub oi_data: Option<OIData>,
}

// ==========================================
// 7. 数据结构补全 (补全 OIData 构造函数)
// ==========================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OIData {
    pub current_oi_amount: f64,   // 持仓张数/数量
    pub current_oi_value: f64,    // 持仓名义价值 (USDT)
    pub change_history: Vec<f64>, // 历史增量变化序列 (用于计算斜率或加速度)
}

impl OIData {
    /// 基础构造函数
    pub fn new(amount: f64, value: f64, history: Vec<f64>) -> Self {
        Self {
            current_oi_amount: amount,
            current_oi_value: value,
            change_history: history,
        }
    }

    /// 辅助方法：计算最新的一段 OI 变化率
    pub fn delta_ratio(&self) -> f64 {
        if self.change_history.is_empty() {
            return 0.0;
        }
        self.change_history.last().cloned().unwrap_or(0.0) / self.current_oi_amount.max(1.0)
    }
}
#[derive(Debug, Clone)]
pub struct Config {
    pub weights: HashMap<AnalyzerKind, f64>,
    pub sensitivity: f64,
}

/// 发送给 AI 的最终决策包裹
#[derive(Debug, Serialize, Deserialize)]
pub struct FinalSignal {
    pub symbol: Symbol,
    pub net_score: f64,
    pub is_rejected: bool,
    pub reason: String,

    // AI 需要的具体证据链
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
            market_snapshot: json!({}), // 留给后续逻辑注入当前价格、ATR等基础环境数据
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

#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error("Missing data for role {0:?}")]
    InsufficientData(Role),
    #[error("Calculation error: {0}")]
    Calculation(String),
}
