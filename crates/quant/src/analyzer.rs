use crate::{
    report::AnalysisAudit,
    types::{DerivativeSnapshot, Direction, FeatureSet, RoleData},
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use common::{Candle, Interval, Symbol};
use core::fmt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, f64, str::FromStr};
use tracing::{debug, error, warn};
pub mod context;
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
            Self::VolumeState => "ctx:vol:volumestate",
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisResult {
    pub kind: AnalyzerKind,
    pub score: f64,
    pub is_violation: bool,
    pub weight_multiplier: f64,
    pub description: String,
    pub tag: String,
    pub rationale: Vec<String>,
    pub debug_data: Value,
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
        }
    }
}

#[derive(Debug, Hash, Eq, PartialEq, Clone, Copy, Serialize, Deserialize, JsonSchema)]
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
pub struct Config {
    pub weights: HashMap<AnalyzerKind, f64>,
    pub sensitivity: f64,
}

impl Default for Config {
    fn default() -> Self {
        let mut weights = HashMap::new();

        // 核心过滤器：环境不对，后面全废 (给高权重)
        weights.insert(AnalyzerKind::MarketRegime, 1.5);

        // 趋势追踪：量化交易的盈利核心
        weights.insert(AnalyzerKind::TrendStrength, 1.2);

        // 辅助指标：作为验证使用 (默认权重)
        weights.insert(AnalyzerKind::Momentum, 1.0);
        weights.insert(AnalyzerKind::VolumeProfile, 1.0);

        // 反转指标：风险较高，初始给低权重，靠动态乘数激活
        weights.insert(AnalyzerKind::Divergence, 0.8);
        weights.insert(AnalyzerKind::SupportResistance, 0.8);
        weights.insert(AnalyzerKind::Volatility, 0.5);

        Self {
            weights,
            sensitivity: 0.04, // 对应你 tanh 逻辑里的推荐值
        }
    }
}
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct FinalSignal {
    pub symbol: Symbol,
    pub net_score: f64,
    pub is_rejected: bool,
    pub reason: String,
    pub sub_reports: Vec<AnalysisResult>,
}

impl FinalSignal {
    fn new_with_reports(symbol: Symbol, score: f64, reports: Vec<AnalysisResult>) -> Self {
        Self {
            symbol,
            net_score: score,
            is_rejected: false,
            reason: "OK".into(),
            sub_reports: reports,
        }
    }

    fn rejected_with_reports(symbol: Symbol, tag: &str, reports: Vec<AnalysisResult>) -> Self {
        Self {
            symbol,
            net_score: 0.0,
            is_rejected: true,
            reason: format!("Violation triggered by: {}", tag),
            sub_reports: reports,
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
    pub fn new(config: Config, analyzers: Vec<Box<dyn Analyzer>>) -> Self {
        Self { analyzers, config }
    }

    pub fn add_analyzer(&mut self, analyzer: Box<dyn Analyzer>) {
        debug!("Adding analyzer: {}", analyzer.name());
        self.analyzers.push(analyzer);
    }

    pub fn run(&self, ctx: &mut MarketContext) -> AnalysisAudit {
        let mut results = Vec::new();
        let mut errors = Vec::new();

        for analyzer in &self.analyzers {
            match analyzer.analyze(ctx) {
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

        let verdict = self.aggregate(&ctx, results);

        AnalysisAudit::build(&ctx, verdict)
    }

    fn aggregate(&self, ctx: &MarketContext, results: Vec<AnalysisResult>) -> FinalSignal {
        // 1. 一票否决逻辑 (保持不变，这是最强的风控)
        if let Some(violation) = results.iter().find(|r| r.is_violation) {
            let tag = violation.tag.clone();
            return FinalSignal::rejected_with_reports(ctx.symbol.clone(), &tag, results);
        }

        let mut total_weighted_score = 0.0;
        let mut total_weight = 0.0;
        let mut pos_scores = 0.0;
        let mut neg_scores = 0.0;

        // 2. 核心加权计算
        for res in &results {
            let base_weight = self.config.weights.get(&res.kind).cloned().unwrap_or(1.0);
            let dyn_mult = ctx.get_multiplier(res.kind);

            // 最终权重 = 静态配置 * 运行时环境乘数 * 分析器内部修正
            let weight = base_weight * dyn_mult * res.weight_multiplier;

            total_weighted_score += res.score * weight;
            total_weight += weight;

            // 用于计算方向一致性（共振度）
            if res.score > 0.0 {
                pos_scores += weight;
            } else if res.score < 0.0 {
                neg_scores += weight;
            }
        }

        // 3. 计算基础净分 (归一化第一步：加权平均)
        let mut net_score = if total_weight > 0.0 {
            total_weighted_score / total_weight
        } else {
            0.0
        };

        // 4. 计算共振因子 (Resonance Factor)
        // 逻辑：如果多空权重严重失衡，说明方向高度一致，增强信号；如果多空势均力敌，说明分歧大，削弱信号。
        let resonance_factor = if (pos_scores + neg_scores) > 0.0 {
            (pos_scores - neg_scores).abs() / (pos_scores + neg_scores)
        } else {
            1.0
        };

        // 施加共振惩罚/奖励：分歧越大，分数越向 0 萎缩
        net_score *= resonance_factor;

        // 5. 最终映射 (归一化第二步：非线性映射)
        // 使用 tanh 函数将分数平滑映射到 [-100, 100] 区间
        // 这样可以保证即便出现极端加权分，下游的仓位计算逻辑也不会溢出
        let final_score = self.normalize_to_standard_range(net_score);

        FinalSignal::new_with_reports(ctx.symbol, final_score, results)
    }

    fn normalize_to_standard_range(&self, score: f64) -> f64 {
        // 使用配置中的灵敏度，而不是硬编码
        let normalized = (score * self.config.sensitivity).tanh();
        (normalized * 100.0).round()
    }
}
#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error("Missing data for role {0:?}")]
    InsufficientData(Role),
    #[error("Calculation error: {0}")]
    Calculation(String),
}
