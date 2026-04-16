use chrono::{DateTime, Utc};
use common::{Interval, Symbol};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ----- 枚举定义 -----
#[derive(Debug, Clone, Copy, PartialEq, Serialize, JsonSchema, Deserialize, Default)]
pub enum TrendStructure {
    StrongBullish,
    Bullish,
    #[default]
    Range,
    Bearish,
    StrongBearish,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RsiState {
    Overbought,
    Oversold,
    Strong,
    Weak,
    Neutral,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TradeDirection {
    Long,
    Short,
}

impl TradeDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Long => "LONG",
            Self::Short => "SHORT",
        }
    }
}
#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum VolumeState {
    Expand,
    Shrink,
    Normal,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MacdCross {
    Golden,
    Death,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MacdMomentum {
    Increasing,
    Decreasing,
    Flat,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq)]
pub enum DivergenceType {
    Bullish,
    Bearish,
}

// ----- 数据结构 -----
#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
pub struct PriceAction {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub volatility_percentile: f64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
pub struct TechnicalIndicators {
    pub rsi_14: Option<f64>,
    pub ma_20: Option<f64>,
    pub ma_50: Option<f64>,
    pub ma_200: Option<f64>,
    pub volume_ma_20: Option<f64>,
    pub bb_upper: Option<f64>,
    pub bb_lower: Option<f64>,
    pub bb_width: Option<f64>,
    pub atr_14: Option<f64>,
    pub macd: Option<f64>,
    pub macd_signal: Option<f64>,
    pub macd_histogram: Option<f64>,
    pub atr_median_20: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
pub struct MarketStructure {
    pub trend_structure: Option<TrendStructure>,
    pub rsi_state: Option<RsiState>,
    pub volume_state: Option<VolumeState>,
    pub ma20_slope: Option<f64>,
    pub ma20_slope_bars: i32,
    pub mtf_aligned: Option<bool>,
    pub correlation_with_global: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
pub struct SpaceGeometry {
    pub dist_to_resistance: Option<f64>,
    pub dist_to_support: Option<f64>,
    pub sup_hit_count: u32,
    pub sup_last_hit: i64,
    pub res_hit_count: u32,
    pub res_last_hit: i64,
    pub ma20_dist_ratio: Option<f64>,
    pub ma50_dist_ratio: Option<f64>,
    pub ma200_dist_ratio: Option<f64>,
    pub ma_converging: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
pub struct SignalStates {
    pub macd_divergence: Option<DivergenceType>,
    pub rsi_divergence: Option<DivergenceType>,
    pub macd_cross: Option<MacdCross>,
    pub macd_momentum: Option<MacdMomentum>,
    pub ma20_reclaim: Option<bool>,
    pub ma20_breakdown: Option<bool>,
    pub rsi_range_3: Option<bool>,
    pub volume_shrink_3: Option<bool>,
    pub extreme_candle: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
pub struct FeatureSet {
    pub bucket: DateTime<Utc>,
    pub symbol: Symbol,
    pub interval: Interval,
    pub recent_closes: [f64; 3],
    #[serde(flatten)]
    pub price_action: PriceAction,
    #[serde(flatten)]
    pub indicators: TechnicalIndicators,
    #[serde(flatten)]
    pub structure: MarketStructure,
    #[serde(flatten)]
    pub space: SpaceGeometry,
    #[serde(flatten)]
    pub signals: SignalStates,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, Default)]
pub struct DerivativeSnapshot {
    pub timestamp: i64,
    pub last_price: f64,
    pub current_oi_amount: f64,
    pub current_oi_value: f64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MarketStressLevel {
    #[default]
    Normal, // 正常
    Squeeze,      // 波动压缩
    Dead,         // 死寂
    MeatGrinder,  // 绞肉机
    Acceleration, // 加速段
}

/// 波动环境评估结论（描述性标签）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VolatilityConclusion {
    Dead,              // 死寂期，不建议交易
    Squeeze,           // 压缩，等待突破
    TrendResonance,    // 趋势共振，环境佳
    TrendWeakMomentum, // 趋势但动能不足
    Acceleration,      // 加速段，警惕乖离
    MeatGrinder,       // 绞肉机，风险极高
    #[default]
    NormalRange, // 标准震荡
}
