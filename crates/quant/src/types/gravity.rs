use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, PartialEq, Eq, Copy)]
pub enum WellSide {
    Support,
    Resistance,
    Magnet,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct PriceGravityWell {
    pub level: f64,
    pub side: WellSide,
    pub sources: BTreeSet<WellSource>,
    pub distance_pct: f64,
    pub strength: f64,
    pub is_active: bool,
    pub hit_count: u32,
    pub last_hit_ts: i64,
    pub magnet_activated: bool,
    pub last_tested_above: bool,
    pub last_tested_below: bool,
    pub cross_ts: i64,
}
impl PriceGravityWell {
    pub fn source_string(&self) -> String {
        self.sources
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("+")
    }
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, PartialOrd, Ord,
)]
pub enum WellSource {
    TrendResistance,
    TrendSupport,
    FilterResistance,
    FilterSupport,
    EntryResistance,
    EntrySupport,
    Ma20,
}
pub struct WellSourceInput {
    dist_opt: Option<f64>,
    source: WellSource,
    hits: u32,
    last_ts: i64,
}
impl WellSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TrendResistance => "TREND_R",
            Self::TrendSupport => "TREND_S",
            Self::FilterResistance => "FILTER_R",
            Self::FilterSupport => "FILTER_S",
            Self::EntryResistance => "ENTRY_R",
            Self::EntrySupport => "ENTRY_S",
            Self::Ma20 => "MA20",
        }
    }

    pub fn default_weight(&self) -> f64 {
        match self {
            Self::TrendResistance | Self::TrendSupport => 1.2,
            Self::FilterResistance | Self::FilterSupport => 0.8,
            Self::EntryResistance | Self::EntrySupport => 0.6,
            Self::Ma20 => 1.0,
        }
    }
    pub fn wear_scale(&self, cfg: &crate::config::GravityConfig) -> f64 {
        match self {
            Self::TrendResistance | Self::TrendSupport => cfg.wear_scales.trend,
            Self::FilterResistance | Self::FilterSupport => cfg.wear_scales.filter,
            Self::EntryResistance | Self::EntrySupport => cfg.wear_scales.entry,
            Self::Ma20 => cfg.wear_scales.ma20,
        }
    }
}
