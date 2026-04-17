use chrono::{DateTime, Utc};
use common::Symbol;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{any::Any, collections::HashMap, f64};
use tracing::warn;

use crate::{
    config::AnalyzerConfig,
    report::AnalysisAudit,
    types::{
        futures::{Role, RoleData},
        market::DerivativeSnapshot,
    },
};

pub mod fakeout;
pub mod gravity;
pub mod regime;
pub mod resonance;
pub mod volatility;
pub mod volume;

pub use fakeout::FakeoutDetector;
pub use gravity::GravityAnalyzer;
pub use regime::MarketRegimeAnalyzer;
pub use resonance::ResonanceAnalyzer;
pub use volatility::VolatilityEnvironmentAnalyzer;
pub use volume::VolumeStructureAnalyzer;

// ==================== ContextKey ====================
#[derive(Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Clone, Copy)]
pub enum ContextKey {
    VolAtrRatio,
    VolIsCompressed,
    VolPercentile,
    VolumeState,
    RegimeStructure,
    MarketCorrelation,
    IsMomentumTsunami,
    OiPositionState,
    SpaceGravityWells,
    GravitySigma,
    PositionSizePct,
    LastEfficiency,
    LastRVol,
    FundingRate,
    MarketStressLevel,
    FakeoutState,
}

// ==================== AnalyzerKind ====================
#[derive(Debug, Hash, Eq, PartialEq, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub enum AnalyzerKind {
    Resonance,
    VolumeStructure,
    Gravity,
    Volatility,
    MarketRegime,
    Fakeout,
}

// ==================== AnalysisResult ====================
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisResult<Extra = ()> {
    pub kind: AnalyzerKind,
    pub score: f64,
    pub is_violation: bool,
    pub weight_multiplier: f64,
    pub description: String,
    pub rationale: Vec<String>,
    pub extra: Extra,
}

impl<Extra: Default> AnalysisResult<Extra> {
    pub fn new(kind: AnalyzerKind) -> Self {
        Self {
            kind,
            score: 0.0,
            is_violation: false,
            weight_multiplier: 1.0,
            description: "PENDING".into(),
            rationale: vec![],
            extra: Extra::default(),
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

    pub fn with_extra(mut self, extra: Extra) -> Self {
        self.extra = extra;
        self
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }
}

// ==================== 类型擦除结果 ====================
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ErasedAnalysisResult {
    pub kind: AnalyzerKind,
    pub score: f64,
    pub is_violation: bool,
    pub weight_multiplier: f64,
    pub description: String,
    pub rationale: Vec<String>,
}

// ==================== Analyzer Trait ====================
pub trait Analyzer: Send + Sync {
    type Extra: Serialize + Clone + Send + Sync + Default + 'static;

    fn kind(&self) -> AnalyzerKind;
    fn name(&self) -> &'static str;
    fn dependencies(&self) -> Vec<ContextKey> {
        vec![]
    }
    fn analyze(
        &self,
        ctx: &mut MarketContext,
    ) -> Result<AnalysisResult<Self::Extra>, AnalysisError>;
}

// ==================== AnalyzerWrapper ====================
pub trait AnalyzerWrapper: Send + Sync {
    fn analyze_erased(
        &self,
        ctx: &mut MarketContext,
    ) -> Result<ErasedAnalysisResult, AnalysisError>;
    fn kind(&self) -> AnalyzerKind;
    fn name(&self) -> &'static str;
    fn dependencies(&self) -> Vec<ContextKey>;
}

impl<T: Analyzer> AnalyzerWrapper for T {
    fn analyze_erased(
        &self,
        ctx: &mut MarketContext,
    ) -> Result<ErasedAnalysisResult, AnalysisError> {
        let result = self.analyze(ctx)?;
        Ok(ErasedAnalysisResult {
            kind: result.kind,
            score: result.score,
            is_violation: result.is_violation,
            weight_multiplier: result.weight_multiplier,
            description: result.description,
            rationale: result.rationale,
        })
    }

    fn kind(&self) -> AnalyzerKind {
        self.kind()
    }

    fn name(&self) -> &'static str {
        self.name()
    }

    fn dependencies(&self) -> Vec<ContextKey> {
        self.dependencies()
    }
}

// ==================== MarketContext ====================
#[derive(Debug)]
pub struct MarketContext {
    pub symbol: Symbol,
    pub timestamp: DateTime<Utc>,
    pub roles: HashMap<Role, RoleData>,
    pub global: DerivativeSnapshot,
    pub cache: HashMap<ContextKey, Box<dyn Any + Send + Sync>>,
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

    pub fn get_cached<T: 'static>(&self, key: ContextKey) -> Option<&T> {
        self.cache
            .get(&key)
            .and_then(|boxed| boxed.downcast_ref::<T>())
    }

    pub fn set_cached<T: 'static + Send + Sync>(&mut self, key: ContextKey, value: T) {
        self.cache.insert(key, Box::new(value));
    }
}

// ==================== ConfigurableAnalyzer ====================
pub trait ConfigurableAnalyzer: Analyzer {
    fn with_config(config: AnalyzerConfig) -> Self;
    fn config(&self) -> &AnalyzerConfig;
}

// ==================== Config ====================
#[derive(Debug, Clone)]
pub struct Config {
    pub weights: HashMap<AnalyzerKind, f64>,
    pub sensitivity: f64,
    pub divergence_threshold: f64,
    pub min_signal_threshold: f64,
}

impl Default for Config {
    fn default() -> Self {
        let mut weights = HashMap::new();
        weights.insert(AnalyzerKind::MarketRegime, 1.5);
        weights.insert(AnalyzerKind::Resonance, 1.0);
        weights.insert(AnalyzerKind::VolumeStructure, 1.0);
        weights.insert(AnalyzerKind::Gravity, 0.8);
        weights.insert(AnalyzerKind::Volatility, 0.5);
        weights.insert(AnalyzerKind::Fakeout, 1.2);

        Self {
            weights,
            sensitivity: 0.02,
            divergence_threshold: 0.2,
            min_signal_threshold: 5.0,
        }
    }
}

// ==================== FinalSignal ====================
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct FinalSignal {
    pub symbol: Symbol,
    pub net_score: f64,
    pub is_rejected: bool,
    pub reason: String,
    pub sub_reports: Vec<ErasedAnalysisResult>,
}

impl FinalSignal {
    pub fn new_with_reports(
        symbol: Symbol,
        score: f64,
        reports: Vec<ErasedAnalysisResult>,
    ) -> Self {
        Self {
            symbol,
            net_score: score,
            is_rejected: false,
            reason: "OK".to_string(),
            sub_reports: reports,
        }
    }

    pub fn rejected_with_reports(symbol: Symbol, reports: Vec<ErasedAnalysisResult>) -> Self {
        let reason = reports
            .iter()
            .filter(|r| r.is_violation)
            .flat_map(|r| &r.rationale)
            .next()
            .cloned()
            .unwrap_or_else(|| "Violation detected".to_string());

        Self {
            symbol,
            net_score: 0.0,
            is_rejected: true,
            reason,
            sub_reports: reports,
        }
    }

    pub fn rejected_with_reason(
        symbol: Symbol,
        reason: impl Into<String>,
        reports: Vec<ErasedAnalysisResult>,
    ) -> Self {
        Self {
            symbol,
            net_score: 0.0,
            is_rejected: true,
            reason: reason.into(),
            sub_reports: reports,
        }
    }
}

// ==================== AnalysisError ====================
#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error("Missing data for role {0:?}")]
    InsufficientData(Role),
    #[error("Calculation error: {0}")]
    Calculation(String),
}

// ==================== AnalysisEngine ====================
pub struct AnalysisEngine {
    pub analyzers: Vec<Box<dyn AnalyzerWrapper>>,
    pub config: Config,
}

impl AnalysisEngine {
    pub fn new(config: Config, analyzers: Vec<Box<dyn AnalyzerWrapper>>) -> Self {
        Self { analyzers, config }
    }

    pub fn run(&self, ctx: &mut MarketContext) -> AnalysisAudit {
        let mut results = Vec::new();

        for analyzer in &self.analyzers {
            match analyzer.analyze_erased(ctx) {
                Ok(res) => results.push(res),
                Err(e) => {
                    let msg = format!("Analyzer {} failed: {}", analyzer.name(), e);
                    warn!("{}", msg);
                }
            }
        }

        let signal = self.aggregate(ctx, results);
        AnalysisAudit::build(ctx, signal)
    }

    fn aggregate(&self, ctx: &MarketContext, results: Vec<ErasedAnalysisResult>) -> FinalSignal {
        if results.iter().any(|r| r.is_violation) {
            return FinalSignal::rejected_with_reports(ctx.symbol, results);
        }

        let mut total_weighted_score = 0.0;
        let mut total_weight = 0.0;
        let mut pos_weighted_sum = 0.0;
        let mut neg_weighted_sum = 0.0;

        for res in &results {
            let base_weight = self.config.weights.get(&res.kind).copied().unwrap_or(1.0);
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

        if self.config.divergence_threshold > 0.0
            && total_active_weight > 0.0
            && resonance_factor < self.config.divergence_threshold
        {
            return FinalSignal::rejected_with_reason(ctx.symbol, "HIGH_DIVERGENCE", results);
        }

        let raw_score = net_score * (0.5 + 0.5 * resonance_factor);
        let final_score = self.normalize_to_standard_range(raw_score);

        if final_score.abs() < self.config.min_signal_threshold {
            return FinalSignal::rejected_with_reason(
                ctx.symbol,
                format!("WEAK_SIGNAL({:.1})", final_score),
                results,
            );
        }

        FinalSignal::new_with_reports(ctx.symbol, final_score, results)
    }

    fn normalize_to_standard_range(&self, score: f64) -> f64 {
        let normalized = (score * self.config.sensitivity).tanh();
        (normalized * 100.0 * 10.0).round() / 10.0
    }
}
