use chrono::{DateTime, Utc};
use common::{Candle, Interval, Symbol};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// --- 1. 市场结构与趋势 (Market Structure) ---

#[derive(Debug, Clone, Copy, PartialEq, Serialize, JsonSchema, Deserialize)]
pub enum TrendStructure {
    StrongBullish, // 价格 > MA20 > MA50 > MA200 (完全多头排列)
    Bullish,       // 价格 > MA20 > MA50 (局部多头)
    Range,         // 均线纠缠
    Bearish,       // 价格 < MA20 < MA50 (局部空头)
    StrongBearish, // 价格 < MA20 < MA50 < MA200 (完全空头排列)
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RsiState {
    Overbought, // > 70
    Oversold,   // < 30
    Strong,     // 60-70
    Weak,       // 30-40
    Neutral,    // 40-60
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq)]
pub enum CandleType {
    BullishBody,
    BearishBody,
    Doji,
}

// --- 2. 信号与动能 (Signals & Momentum) ---

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

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum VolumeState {
    Expand,
    Shrink,
    Squeeze,
    Normal,
}

// --- 3. 特征集容器 (Feature Containers) ---

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
pub struct PriceAction {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    #[schemars(description = "波动率百分位 (基于BB Width历史窗口)")]
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
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
pub struct MarketStructure {
    pub trend_structure: Option<TrendStructure>,
    pub rsi_state: Option<RsiState>,
    pub volume_state: Option<VolumeState>,
    pub candle_type: Option<CandleType>,
    pub ma20_slope: Option<f64>,
    pub ma20_slope_bars: i32,
    pub mtf_aligned: Option<bool>,
    pub correlation_with_global: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
pub struct SpaceGeometry {
    pub ma20_dist_ratio: Option<f64>,
    pub dist_to_resistance: Option<f64>,
    pub dist_to_support: Option<f64>,
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

// --- 4. 期货博弈论 (Futures Game Theory) ---

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, JsonSchema)]
pub enum RiskLevel {
    DeepCoiling,      // 深度冷缩：OI低位，波动极低
    Healthy,          // 健康趋势
    LeveledUp,        // 杠杆推升
    ExtremeOverheat,  // 极端拥挤：OI 95%分位，费率激增
    PanicLiquidation, // 恐慌清算：OI剧降，禁止入场
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, JsonSchema)]
pub enum OIPositionState {
    /// 价升量增 (Speculative Drive / Long Build-up): 多头主动进攻，投机力量推动
    LongBuildUp,
    /// 价跌量增 (Heavy Accumulation / Short Build-up): 空头主动开仓，或大户在低位吸筹接盘
    ShortBuildUp,
    /// 价跌量减 (Long Unwinding): 多头爆仓或止损，引发连环踩踏
    LongUnwinding,
    /// 价升量减 (Short Squeeze / Short Covering): 空头爆仓或止盈，空头平仓引发的被迫买入
    ShortCovering,
    /// 震荡/无明显持仓变化
    Neutral,
}

impl OIPositionState {
    /// 物理层：根据价格和持仓变化判定状态
    pub fn determine(price_change: f64, oi_change: f64) -> Self {
        // 使用较小的阈值避免微小波动触发状态切换
        const THRESHOLD: f64 = 1e-6;
        if oi_change.abs() < THRESHOLD {
            return Self::Neutral;
        }

        match (price_change > 0.0, oi_change > 0.0) {
            (true, true) => Self::LongBuildUp,
            (false, true) => Self::ShortBuildUp,
            (false, false) => Self::LongUnwinding,
            (true, false) => Self::ShortCovering,
        }
    }

    /// 评分层：用于趋势插件和引擎打分
    pub fn signal_score(&self) -> f64 {
        match self {
            Self::LongBuildUp => 1.0,   // 极强多头
            Self::ShortCovering => 0.5, // 被动多头(空平)
            Self::Neutral => 0.0,
            Self::LongUnwinding => -0.5, // 被动空头(多平)
            Self::ShortBuildUp => -1.0,  // 极强空头
        }
    }
}

// --- 5. 整合特征集 (The Unified Feature Set) ---

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
pub struct FeatureSet {
    pub bucket: DateTime<Utc>,
    pub symbol: Symbol,
    pub interval: Interval,

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

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct PriceGravityWell {
    pub level: f64,
    pub source: String,    // e.g., "H4_MA200", "Liq_Wall"
    pub distance_pct: f64, // 距离百分比
    pub strength: f64,     // 0.0 ~ 1.0
}

#[derive(Debug, Deserialize, Serialize, JsonSchema, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Long,
    Short,
    Neutral, // 用于无方向震荡或平仓观望
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, Default)]
pub struct DerivativeSnapshot {
    pub timestamp: i64,

    pub last_price: f64,

    /// 当前持仓总量 (以张数或币数为单位)
    pub current_oi_amount: f64,

    /// 当前持仓名义价值 (以 U 为单位，用于跨币种横向对比风险)
    pub current_oi_value: f64,
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
        if self.change_history.is_empty() {
            return 0.0;
        }
        self.change_history.last().cloned().unwrap_or(0.0) / self.current_oi_amount.max(1.0)
    }
}
#[derive(Debug, Clone, Default)]
pub struct TakerFlowData {
    pub buy_vol: f64,
    pub sell_vol: f64,
    pub net_vol: f64,
    pub taker_buy_ratio: Option<f64>, // 改为 Option，避免除以零
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
