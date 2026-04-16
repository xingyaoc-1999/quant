use super::market::FeatureSet;
use common::{Candle, Interval};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, JsonSchema)]
pub enum RiskLevel {
    DeepCoiling,
    Healthy,
    LeveledUp,
    ExtremeOverheat,
    PanicLiquidation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, JsonSchema, Default)]
pub enum OIPositionState {
    LongBuildUp,
    ShortBuildUp,
    LongUnwinding,
    ShortCovering,
    #[default]
    Neutral,
}

impl OIPositionState {
    pub fn determine(price_pct: f64, oi_ratio: f64) -> Self {
        const OI_SENSITIVITY: f64 = 0.0005;
        const PRICE_DEADZONE: f64 = 0.0001;
        if oi_ratio.abs() < OI_SENSITIVITY {
            return Self::Neutral;
        }
        let p_dir = if price_pct.abs() < PRICE_DEADZONE {
            0
        } else if price_pct > 0.0 {
            1
        } else {
            -1
        };
        let o_dir = if oi_ratio > 0.0 { 1 } else { -1 };
        match (p_dir, o_dir) {
            (1, 1) => Self::LongBuildUp,
            (-1, 1) => Self::ShortBuildUp,
            (-1, -1) => Self::LongUnwinding,
            (1, -1) => Self::ShortCovering,
            _ => Self::Neutral,
        }
    }
}

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

    pub fn delta_ratio(&self) -> f64 {
        if self.change_history.is_empty() || self.current_oi_amount <= 0.0 {
            return 0.0;
        }
        let last_change = self.change_history.last().cloned().unwrap_or(0.0);
        (last_change / self.current_oi_amount).clamp(-0.2, 0.2)
    }
}

#[derive(Debug, Clone, Default)]
pub struct TakerFlowData {
    pub buy_vol: f64,
    pub sell_vol: f64,
    pub net_vol: f64,
    pub taker_buy_ratio: Option<f64>,
}

impl TakerFlowData {
    pub fn from_candle(candle: &Candle) -> Self {
        let buy_vol = candle.taker_buy_volume;
        let total_vol = candle.volume;
        let ratio = if total_vol > 0.0 {
            Some(buy_vol / total_vol)
        } else {
            Some(0.5)
        };
        Self {
            buy_vol,
            sell_vol: (total_vol - buy_vol).max(0.0),
            net_vol: buy_vol - (total_vol - buy_vol),
            taker_buy_ratio: ratio,
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

// ----- Role 枚举（保持原有位置，但放在 futures 中因为与期货数据紧密相关）-----
use std::fmt;
use std::str::FromStr;

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
