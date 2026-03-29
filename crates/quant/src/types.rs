use chrono::{DateTime, Utc};
use common::{Interval, Symbol};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// --- 基础枚举定义 ---

#[derive(Debug, Clone, Copy, PartialEq, Serialize, JsonSchema, Deserialize)]
pub enum TrendStructure {
    StrongBullish, // 价格 > MA20 > MA50 > MA200 (完全多头排列)
    Bullish,       // 价格 > MA20 > MA50 (局部多头)
    Range,         // 均线纠缠
    Bearish,       // 价格 < MA20 < MA50 (局部空头)
    StrongBearish, // 价格 < MA20 < MA50 < MA200 (完全空头排列)
}

#[derive(Debug, Deserialize, Serialize, JsonSchema, Clone, Copy, Default)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Long,
    Short,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum RsiState {
    Overbought, // 极度超买 (>70)
    Oversold,   // 极度超卖 (<30)
    Strong,     // 强势区 (60-70)
    Weak,       // 弱势区 (30-40)
    Neutral,    // 中轴区 (40-60)
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MacdCross {
    Golden, // 金叉
    Death,  // 死叉
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MacdMomentum {
    Increasing, // 柱体变长/向上
    Decreasing, // 柱体变短/向下
    Flat,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VolumeState {
    Expand,  // 放量
    Shrink,  // 缩量
    Squeeze, // 极度挤压
    Normal,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum CandleType {
    BullishBody,
    BearishBody,
    Doji,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq)]
pub enum DivergenceType {
    Bullish, // 底背离
    Bearish, // 顶背离
}

// --- 核心结构体定义 ---

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
    #[schemars(description = "当前K线基本形态")]
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
    #[schemars(description = "均线收敛状态 (MA20/MA50距离是否小于阈值)")]
    pub ma_converging: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
pub struct SignalStates {
    // 趋势与转向
    pub macd_divergence: Option<DivergenceType>,
    pub rsi_divergence: Option<DivergenceType>,
    pub macd_cross: Option<MacdCross>,
    pub macd_momentum: Option<MacdMomentum>,

    // 关键位置触发
    pub ma20_reclaim: Option<bool>,
    pub ma20_breakdown: Option<bool>,

    #[schemars(description = "RSI是否连续在窄幅区间震荡 (蓄势)")]
    pub rsi_range_3: Option<bool>,
    #[schemars(description = "成交量是否连续萎缩 (缩量回调)")]
    pub volume_shrink_3: Option<bool>,
    #[schemars(description = "是否为极端异常波动K线")]
    pub extreme_candle: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct AIAuditPayload {
    pub header: AuditHeader,
    pub score_engine: ScoreEngineSummary,
    pub market_statistics: MarketStatistics,
    pub trade_management_audit: TradeManagementAudit,

    pub global_context: GlobalContext,
}
pub struct AIAuditPayloadBuilder {
    header: Option<AuditHeader>,
    score_engine: Option<ScoreEngineSummary>,
    market_statistics: Option<MarketStatistics>,
    trade_management_audit: Option<TradeManagementAudit>,
    global_context: Option<GlobalContext>,
}

impl AIAuditPayloadBuilder {
    pub fn new() -> Self {
        Self {
            header: None,
            score_engine: None,
            market_statistics: None,
            trade_management_audit: None,

            global_context: None,
        }
    }

    pub fn header(mut self, header: AuditHeader) -> Self {
        self.header = Some(header);
        self
    }

    pub fn score_engine(mut self, score_engine: ScoreEngineSummary) -> Self {
        self.score_engine = Some(score_engine);
        self
    }

    pub fn market_statistics(mut self, stats: MarketStatistics) -> Self {
        self.market_statistics = Some(stats);
        self
    }

    pub fn trade_audit(mut self, audit: TradeManagementAudit) -> Self {
        self.trade_management_audit = Some(audit);
        self
    }

    pub fn global_context(mut self, global: GlobalContext) -> Self {
        self.global_context = Some(global);
        self
    }

    pub fn build(self) -> Result<AIAuditPayload, String> {
        Ok(AIAuditPayload {
            header: self.header.ok_or("Missing header")?,
            score_engine: self.score_engine.ok_or("Missing score_engine")?,
            market_statistics: self.market_statistics.ok_or("Missing market_statistics")?,
            trade_management_audit: self.trade_management_audit.ok_or("Missing trade_audit")?,
            global_context: self.global_context.ok_or("Missing global_context")?,
        })
    }
}

impl Default for AIAuditPayloadBuilder {
    fn default() -> Self {
        Self::new()
    }
}
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct AuditHeader {
    /// 策略唯一标识符，定义其底层逻辑（如趋势跟踪、均值回归）
    pub strategy_id: String,
    /// 交易标的名称（如 BTCUSDT）
    pub symbol: Symbol,
    pub direction: Option<Direction>,
    /// 信号生成的时间戳
    pub timestamp: DateTime<Utc>,
    pub trigger_source: TriggerSource,
    /// 逻辑角色的物理周期配置映射。Entry 决定精度，Trend 决定胜率底色
    pub interval_setup: IntervalSetup,
}
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct IntervalSetup {
    pub entry: Interval,
    pub trend: Interval,
    pub confirmation: Interval,
}
impl Default for IntervalSetup {
    fn default() -> Self {
        Self {
            entry: Interval::M5,

            confirmation: Interval::H1,

            trend: Interval::H4,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TriggerSource {
    Manual,
    Auto,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, Default)]
pub struct ScoreEngineSummary {
    /// 引擎给出的 0-100 综合得分。高于 80 通常意味着极强的技术共振
    pub final_score: f64,
    /// 正向驱动项：为什么认为值得做
    pub positive_drivers: Vec<LogicComponent>,
    /// 负向扣分项：AI 需评估瑕疵是否致命
    pub red_flags: Vec<LogicComponent>,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, Default)]
pub struct LogicComponent {
    pub id: String,   // 分析器名称或唯一标识
    pub score: f64,   // 该组件贡献的分数（可能为正或负）
    pub desc: String, // 可读描述
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct MarketStatistics {
    /// 当前波动率在历史周期内的百分比排名(0-100)。极低分位意味着即将爆发
    pub volatility_percentile: f64,
    /// 成交量活跃度分位数(0-100)。高分位代表机构参与度高
    pub volume_percentile: f64,
    /// 当前趋势已经持续的 K 线数量。>40 意味着趋势可能过度拉升或老化
    pub trend_exhaustion_risk: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct TradeManagementAudit {
    /// 止盈/止损盈亏比
    pub rr_ratio: f64,
    pub entry_price: f64,
    pub stop_loss: f64,
    pub take_profit: f64,
    pub current_price: f64,
    /// 物理路径阻碍描述。例如：止盈位前有大级别均线压制
    pub execution_hurdles: String,
    pub invalidation_rules: InvalidationRules,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct InvalidationRules {
    /// 时间失效：入场后指定分钟内未兑现动能则离场
    pub time_stop_minutes: Option<u32>,
    /// 结构失效：若收盘价破坏此价位，则逻辑即刻作废
    pub structure_stop_price: Option<f64>,
    pub additional_rules: String,
}
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, JsonSchema)]
pub enum RiskLevel {
    // 1. 深度冷缩 (Deep Coiling)
    // 特征：OI处于24h低位，波动率极低。
    // 含义：暴风雨前的宁静，适合布局长线波段。
    DeepCoiling,

    Healthy,

    LeveledUp,

    // 4. 极端拥挤 (Extreme Overheat)
    // 特征：OI 处于 95% 以上分位，费率激增（如 >0.03%）。
    // 含义：多杀多/空杀空的火药桶，波段交易必须止盈或大幅减仓。
    ExtremeOverheat,

    // 5. 恐慌清算 (Panic Liquidation)
    // 特征：OI 剧烈下降（5m 跌幅 > 3%），价格巨震。
    // 含义：正在发生大规模爆仓，禁止入场，等待尘埃落定。
    PanicLiquidation,
}
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub enum LSRatioStatus {
    RetailCrowded,     // 散户过度拥挤 (散户多空比 > 2.0)
    WhaleAccumulating, // 大户悄悄建仓 (大户比例持续上升)
    Diverging,         // 严重背离 (大户和散户反着走)
    Neutral,
}
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct FuturesGameTheory {
    pub money_flow: MoneyFlowStatus,
    pub taker_aggression: f64, // 主动买卖比 (1.0 为中性)

    // 2. 筹码博弈 (基于 L/S Ratio)
    pub sentiment_divergence: f64, // 大户 vs 散户背离度
    pub ls_status: LSRatioStatus,

    // 3. 风险与压力 (基于 Funding/OI Percentile)
    pub leverage_risk: RiskLevel,
    pub funding_bias: f64, // 费率偏离度

    // 4. 空间阻力 (基于 Liquidation)
    pub liquidation_zones: Vec<LiquidationZone>,
    pub liq_intensity: f64, // 爆仓强度 (识别是否发生踩踏)
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub enum MoneyFlowStatus {
    HeavyAccumulation, // 强力吸筹 (Price ↓/横, OI ↑↑)
    ShortSqueeze,      // 空头踩踏 (Price ↑, OI ↓↓)
    LongUnwinding,     // 多头平仓 (Price ↓, OI ↓)
    SpeculativeDrive,  // 投机驱动 (Price ↑, OI ↑)
    Neutral,
}
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, Default)]
pub struct LiquidationZone {
    pub direction: Direction,
    pub price_level: f64,
    pub strength: f64,
    pub description: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct DynamicContext {
    /// 空间重力场：整合所有技术与爆仓位，标注相对于当前价的百分比距离
    pub gravity_wells: Vec<PriceGravityWell>,
    /// 逻辑审计：专门指出指标间的背离或共振异常
    pub logic_conflicts: Vec<CorrelationConflict>,
    /// 信号新鲜度（秒）
    pub signal_age: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct PriceGravityWell {
    pub level: f64,
    pub source: String, // e.g., "H4_MA200", "Liq_Wall"
    /// 距离当前价格的百分比（如 0.005 表示上方 0.5%）
    pub distance_pct: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct CorrelationConflict {
    pub factor_a: String,
    pub factor_b: String,
    pub nature: String, // e.g., "Divergence", "Confluence"
    pub description: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct GlobalContext {
    pub session: Session,
    /// 与 BTC 相关性（-1 到 1）
    pub btc_correlation: f64,
    pub macro_events: String,
    pub strategy_health: StrategyHealth,
}
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Session {
    NyOpen,
    NyClose,
    LondonOpen,
    LondonClose,
    Asia,
    Overlap,
    Weekend,
}
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct StrategyHealth {
    pub last_3_trades: Vec<TradeOutcome>,
    pub daily_pnl: f64,
    pub win_rate_recent: f64,
    pub consecutive_losses: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TradeOutcome {
    Win,
    Loss,
    Even,
}
#[derive(Debug, Serialize, Deserialize, Clone, Copy, Default)]
pub struct DerivativeSnapshot {
    pub timestamp: i64,

    pub last_price: f64,

    pub current_oi_amount: f64, // 持仓张数/币数
    pub current_oi_value: f64,  // 持仓名义价值 (U)
}
