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
    // --- 静态价格位 (距离比例) ---
    pub dist_to_resistance: Option<f64>, // 阻力位距离
    pub dist_to_support: Option<f64>,    // 支撑位距离

    // --- 磨损与博弈元数据 (新增核心) ---
    // 支撑位的触碰统计
    pub sup_hit_count: u32,
    pub sup_last_hit: i64,
    // 阻力位的触碰统计
    pub res_hit_count: u32,
    pub res_last_hit: i64,

    // --- 均线动态位 ---
    pub ma20_dist_ratio: Option<f64>,  // 月度趋势线 (MA20)
    pub ma50_dist_ratio: Option<f64>,  // 季度生命线 (MA50)
    pub ma200_dist_ratio: Option<f64>, // 年度牛熊线 (MA200)

    // --- 状态位 ---
    pub ma_converging: Option<bool>, // 均线纠缠状态
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
    /// 逻辑层：根据价格变化率和持仓变化比例判定状态
    /// price_pct: (close - open) / open
    /// oi_ratio:  delta_oi / total_oi (即 delta_ratio 函数的返回值)
    pub fn determine(price_pct: f64, oi_ratio: f64) -> Self {
        // 1. 灵敏度门槛常量（建议根据回测调优）
        // 只有当持仓变动超过 0.05% 时才认为具有博弈信号，否则视为噪音
        const OI_SENSITIVITY: f64 = 0.0005;
        // 只有价格波动超过 0.01% 时才区分方向
        const PRICE_DEADZONE: f64 = 0.0001;

        // 2. 噪音过滤：如果持仓变动极其微小，返回中性
        if oi_ratio.abs() < OI_SENSITIVITY {
            return Self::Neutral;
        }

        // 3. 极小价格波动的处理：如果持仓在变但价格没动，可能是大宗对冲或挂单密集区
        let p_dir = if price_pct.abs() < PRICE_DEADZONE {
            0
        } else if price_pct > 0.0 {
            1
        } else {
            -1
        };
        let o_dir = if oi_ratio > 0.0 { 1 } else { -1 };

        match (p_dir, o_dir) {
            (1, 1) => Self::LongBuildUp,     // 价升量增：多头主动入场 (强)
            (-1, 1) => Self::ShortBuildUp,   // 价跌量增：空头主动压制 (强)
            (-1, -1) => Self::LongUnwinding, // 价跌量减：多头止损踩踏 (弱)
            (1, -1) => Self::ShortCovering,  // 价升量减：空头平仓回补 (弱)
            _ => Self::Neutral,              // 其他（如价格不动但持仓剧变）
        }
    }
}
// --- 5. 整合特征集 (The Unified Feature Set) ---

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

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, PartialEq, Eq, Copy)]
pub enum WellSide {
    Support,    // 支撑/地板
    Resistance, // 压力/天花板
    Magnet,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct PriceGravityWell {
    pub level: f64,        // 物理价格位点 (例如 72500.5)
    pub side: WellSide,    // 明确是支撑还是压力
    pub source: String,    // 来源 (例如 "MTF_Confluence", "D1_EMA200")
    pub distance_pct: f64, // 距离当前价格的百分比 (支撑为负, 压力为正)
    pub strength: f64,     // 引力强度 (0.0 ~ 1.0)
    pub is_active: bool,   // 是否已进入当前感应半径 (即 intensity > 0)
    pub hit_count: u32,
    pub last_hit_ts: i64,
    pub magnet_activated: bool, // 是否已被标记为磁力井
    pub last_tested_above: bool,
    pub last_tested_below: bool,
    pub cross_ts: i64,
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

    // 建议修改：
    pub fn delta_ratio(&self) -> f64 {
        if self.change_history.is_empty() || self.current_oi_amount <= 0.0 {
            return 0.0;
        }
        // 获取最新的变化量
        let last_change = self.change_history.last().cloned().unwrap_or(0.0);

        // 返回百分比变化 (e.g., 0.01 代表持仓增加了 1%)
        (last_change / self.current_oi_amount).clamp(-0.2, 0.2) // 限制极端值噪音
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
