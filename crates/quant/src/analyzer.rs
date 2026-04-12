use anyhow::Result;
use chrono::{DateTime, Utc};
use common::Symbol;
use core::fmt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, f64, str::FromStr};
use tracing::warn;

use crate::{
    report::AnalysisAudit,
    types::{DerivativeSnapshot, RoleData},
};

pub mod context;
pub mod signal;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ContextKey {
    // 波动率环境
    VolAtrRatio,
    VolIsCompressed,
    VolPercentile,
    VolumeState,
    // 市场结构
    RegimeStructure,
    MarketCorrelation,
    IsMomentumTsunami,
    OiPositionState,

    // 物理引擎数据
    SpaceGravityWells,
    GravitySigma,
    StopLossLevels,   // Vec<f64>
    TakeProfitLevels, // Vec<f64>
    WeightedRR,       // f64
    PositionSizePct,
    LastEfficiency,
    LastRVol,
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

#[derive(Debug, Hash, Eq, PartialEq, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub enum AnalyzerKind {
    TrendStrength,
    Momentum,
    VolumeProfile,
    Divergence,
    SupportResistance,
    Volatility,
    MarketRegime,
    RiskManagement,
    Fakeout,
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
            score: 0.0,
            is_violation: false,
            weight_multiplier: 1.0,
            description: "PENDING".into(),
            rationale: vec![],
            debug_data: json!({}),
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

    pub fn debug(mut self, data: Value) -> Self {
        self.debug_data = data;
        self
    }
}

pub struct AnalysisEngine {
    pub analyzers: Vec<Box<dyn Analyzer>>,
    pub config: Config,
}

impl AnalysisEngine {
    pub fn new(config: Config, analyzers: Vec<Box<dyn Analyzer>>) -> Self {
        Self { analyzers, config }
    }

    pub fn run(&self, ctx: &mut MarketContext) -> AnalysisAudit {
        let mut results = Vec::new();

        for analyzer in &self.analyzers {
            match analyzer.analyze(ctx) {
                Ok(res) => results.push(res),
                Err(e) => {
                    warn!("Analyzer {} failed: {}", analyzer.name(), e);
                }
            }
        }

        let signal = self.aggregate(ctx, results);
        AnalysisAudit::build(ctx, signal)
    }

    fn aggregate(&self, ctx: &MarketContext, results: Vec<AnalysisResult>) -> FinalSignal {
        let violation_tag = results
            .iter()
            .find(|r| r.is_violation)
            .map(|r| r.tag.clone());

        if let Some(tag) = violation_tag {
            return FinalSignal::rejected_with_reports(ctx.symbol, &tag, results);
        }

        let mut total_weighted_score = 0.0;
        let mut total_weight = 0.0;
        let mut pos_weighted_sum = 0.0;
        let mut neg_weighted_sum = 0.0;

        for res in &results {
            let base_weight = self.config.weights.get(&res.kind).cloned().unwrap_or(1.0);

            let final_weight = base_weight * res.weight_multiplier;

            total_weighted_score += res.score * final_weight;
            total_weight += final_weight;

            if res.score > 0.0 {
                pos_weighted_sum += final_weight;
            } else if res.score < 0.0 {
                neg_weighted_sum += final_weight;
            }
        }

        let net_score = if total_weight > 0.0 {
            total_weighted_score / total_weight
        } else {
            0.0
        };

        let total_active_weight = pos_weighted_sum + neg_weighted_sum;
        let resonance_factor = if total_active_weight > 0.0 {
            (pos_weighted_sum - neg_weighted_sum).abs() / total_active_weight
        } else {
            1.0
        };

        if total_active_weight > 0.0 && resonance_factor < 0.2 {
            return FinalSignal::rejected_with_reports(ctx.symbol, "HIGH_DIVERGENCE", results);
        }
        let final_score =
            self.normalize_to_standard_range(net_score * (0.5 + 0.5 * resonance_factor));

        FinalSignal::new_with_reports(ctx.symbol, final_score, results)
    }

    fn normalize_to_standard_range(&self, score: f64) -> f64 {
        let normalized = (score * self.config.sensitivity).tanh();
        (normalized * 100.0).round()
    }
}

#[derive(Debug, Clone)]
pub struct MarketContext {
    pub symbol: Symbol,
    pub timestamp: DateTime<Utc>,
    pub roles: HashMap<Role, RoleData>,
    pub global: DerivativeSnapshot,
    pub cache: HashMap<ContextKey, Value>,
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

    pub fn get_cached<T: serde::de::DeserializeOwned>(&self, key: ContextKey) -> Option<T> {
        self.cache
            .get(&key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    pub fn set_cached<T: Serialize>(&mut self, key: ContextKey, value: T) {
        self.cache.insert(key, json!(value));
    }
}

pub trait Analyzer: Send + Sync {
    fn name(&self) -> &'static str;
    fn kind(&self) -> AnalyzerKind;
    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError>;
}

#[derive(Debug, Clone)]
pub struct Config {
    pub weights: HashMap<AnalyzerKind, f64>,
    pub sensitivity: f64,
}

impl Default for Config {
    fn default() -> Self {
        let mut weights = HashMap::new();
        weights.insert(AnalyzerKind::MarketRegime, 1.5);
        weights.insert(AnalyzerKind::Momentum, 1.0);
        weights.insert(AnalyzerKind::VolumeProfile, 1.0);
        weights.insert(AnalyzerKind::SupportResistance, 0.8);
        weights.insert(AnalyzerKind::Volatility, 0.5);
        weights.insert(AnalyzerKind::Fakeout, 1.2);

        Self {
            weights,
            sensitivity: 0.02,
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
    pub fn new_with_reports(symbol: Symbol, score: f64, reports: Vec<AnalysisResult>) -> Self {
        Self {
            symbol,
            net_score: score,
            is_rejected: false,
            reason: "OK".into(),
            sub_reports: reports,
        }
    }
    pub fn rejected_with_reports(symbol: Symbol, tag: &str, reports: Vec<AnalysisResult>) -> Self {
        Self {
            symbol,
            net_score: 0.0,
            is_rejected: true,
            reason: format!("Violation: {}", tag),
            sub_reports: reports,
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
