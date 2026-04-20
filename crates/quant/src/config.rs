use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzerConfig {
    pub gravity: GravityConfig,
    pub volume: VolumeConfig,
    pub regime: RegimeConfig,
    pub fakeout: FakeoutConfig,
    pub session: SessionConfig,
    pub volatility: VolatilityConfig,
    pub resonance: ResonanceConfig,
    pub risk: RiskConfig,
}

impl Default for AnalyzerConfig {
    fn default() -> Self {
        Self {
            gravity: GravityConfig::default(),
            volume: VolumeConfig::default(),
            regime: RegimeConfig::default(),
            fakeout: FakeoutConfig::default(),
            session: SessionConfig::default(),
            volatility: VolatilityConfig::default(),
            resonance: ResonanceConfig::default(),
            risk: RiskConfig::default(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WearScales {
    pub trend: f64,
    pub filter: f64,
    pub entry: f64,
    pub ma20: f64,
}

impl Default for WearScales {
    fn default() -> Self {
        Self {
            trend: 1.0,
            filter: 1.0,
            entry: 0.5,
            ma20: 1.0,
        }
    }
}
// ==================== GravityConfig ====================
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GravityConfig {
    /// 引力作用范围 (0.3~1.8)，值越大井的影响半径越大
    pub gravity_range: f64,
    /// 最小有效井强度 (0.02~0.20)，低于此值的井被忽略
    pub min_well_strength: f64,
    /// 井位合并敏感度 (0~1)，值越大越容易合并
    pub merge_sensitivity: f64,
    /// 磨损敏感度 (0~1)，值越大触碰后强度衰减越快
    pub wear_sensitivity: f64,
    /// 磨损恢复速率 (0~1)，值越大强度恢复越快
    pub wear_recovery_rate: f64,
    pub enable_entry_wells: bool,
    pub weight_entry_res: f64,
    pub weight_entry_sup: f64,
    pub entry_wear_scale: f64,
    pub active_well_threshold: f64,
    pub wear_scales: WearScales,
}

impl Default for GravityConfig {
    fn default() -> Self {
        Self {
            gravity_range: 0.8,
            min_well_strength: 0.08,
            merge_sensitivity: 0.5,
            wear_sensitivity: 0.6,
            wear_recovery_rate: 0.5,
            enable_entry_wells: true,
            weight_entry_res: 0.6,
            weight_entry_sup: 0.6,
            entry_wear_scale: 0.5,
            active_well_threshold: 0.08,
            wear_scales: WearScales::default(),
        }
    }
}

impl GravityConfig {
    pub fn min_well_strength(&self) -> f64 {
        self.min_well_strength
    }

    pub fn active_well_threshold(&self) -> f64 {
        self.active_well_threshold
    }
    pub(crate) fn sigma_atr_mult(&self) -> f64 {
        self.gravity_range
    }
    pub(crate) fn confluence_gate_mult(&self) -> f64 {
        self.merge_sensitivity * 0.8
    }
    pub(crate) fn critical_hit_count(&self) -> u32 {
        (2.0 + self.wear_sensitivity * 2.0).round() as u32
    }
    pub(crate) fn steepness(&self) -> f64 {
        1.0 + self.wear_sensitivity * 2.0
    }
    pub(crate) fn wear_restore_halflife_ms(&self) -> f64 {
        7_200_000.0 * (1.0 + (1.0 - self.wear_recovery_rate))
    }
    pub(crate) fn cross_side_merge_factor(&self) -> f64 {
        self.merge_sensitivity
    }
    pub(crate) fn cross_side_strength_dampen(&self) -> f64 {
        0.9 - self.merge_sensitivity * 0.4
    }
    pub(crate) fn max_strength_cap(&self) -> f64 {
        3.5
    }
    pub(crate) fn secondary_well_weight(&self) -> f64 {
        0.3
    }
    pub(crate) fn convergence_boost(&self) -> f64 {
        1.35
    }
    pub(crate) fn magnet_confirm_ms(&self) -> i64 {
        180_000
    }
    pub(crate) fn min_hold_ms(&self) -> i64 {
        30_000
    }
}

// ==================== VolatilityConfig ====================
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolatilityConfig {
    /// 低于中位数该比例视为死寂市场
    pub extreme_low_ratio: f64,
    /// 低于该比例视为波动压缩
    pub squeeze_ratio: f64,
    /// 低于该比例视为趋势动能不足
    pub low_momentum_ratio: f64,
    /// 高于该比例视为绞肉机行情
    pub meat_grinder_ratio: f64,
    /// 高于该比例视为加速段
    pub acceleration_ratio: f64,
    /// 死寂市场的乘数
    pub dead_multiplier: f64,
    /// 加速段的乘数
    pub acceleration_multiplier: f64,
    /// 趋势动能不足的乘数
    pub weak_momentum_multiplier: f64,
    /// 趋势共振的乘数
    pub trend_resonance_multiplier: f64,
    /// 波动压缩的乘数
    pub squeeze_multiplier: f64,
    /// 绞肉机行情的乘数
    pub meat_grinder_multiplier: f64,
    /// 标准震荡的乘数
    pub normal_range_multiplier: f64,
    pub compressed_threshold: f64,
}

impl Default for VolatilityConfig {
    fn default() -> Self {
        Self {
            extreme_low_ratio: 0.3,
            squeeze_ratio: 0.6,
            low_momentum_ratio: 0.7,
            meat_grinder_ratio: 2.0,
            acceleration_ratio: 2.2,
            dead_multiplier: 0.15,
            acceleration_multiplier: 1.1,
            weak_momentum_multiplier: 0.7,
            trend_resonance_multiplier: 1.35,
            squeeze_multiplier: 0.9,
            meat_grinder_multiplier: 0.25,
            normal_range_multiplier: 1.0,
            compressed_threshold: 22.0,
        }
    }
}
// ==================== VolumeConfig ====================
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeConfig {
    /// 量能敏感度 (0~1)
    pub volume_sensitivity: f64,
    /// 效率门槛 (0~1)
    pub efficiency_threshold: f64,
    /// 趋势延伸激进程度 (0~1)
    pub trend_extension_aggressiveness: f64,
    pub efficiency: EfficiencyConfig,
    pub magnet_threshold_ratio: f64, // 磁力突破/高效率阈值的比例系数（相对于普通阈值）
    pub magnet_shrink_base: f64,     // 磁力缩量基准值（未经 vol_factor 调整）
}

impl Default for VolumeConfig {
    fn default() -> Self {
        Self {
            volume_sensitivity: 0.5,
            efficiency_threshold: 0.5,
            trend_extension_aggressiveness: 0.6,
            magnet_threshold_ratio: 0.7, // 磁力阈值比普通阈值宽松 30%
            magnet_shrink_base: 0.5,     // 缩量基准
            efficiency: EfficiencyConfig::default(),
        }
    }
}

impl VolumeConfig {
    pub(crate) fn rvol_break_base(&self) -> f64 {
        1.0 + self.volume_sensitivity * 0.4
    }
    pub(crate) fn rvol_extreme_base(&self) -> f64 {
        1.8 + self.volume_sensitivity * 0.8
    }
    pub(crate) fn eff_high_base(&self) -> f64 {
        0.8 + self.efficiency_threshold * 0.8
    }
    pub(crate) fn eff_low_base(&self) -> f64 {
        0.8 - self.efficiency_threshold * 0.6
    }
    pub(crate) fn trend_extension_base_score(&self) -> f64 {
        25.0 + self.trend_extension_aggressiveness * 30.0
    }
    pub(crate) fn trend_extension_mult(&self) -> f64 {
        1.0 + self.trend_extension_aggressiveness * 0.7
    }
    pub(crate) fn trend_extension_eff_boost(&self) -> f64 {
        1.3
    }
    pub(crate) fn trend_efficiency_threshold(&self) -> f64 {
        0.35
    }
    pub(crate) fn trend_weak_eff_penalty(&self) -> f64 {
        0.8
    }
    pub(crate) fn absorption_taker_buy_min(&self) -> f64 {
        0.55
    }
    pub(crate) fn absorption_taker_sell_max(&self) -> f64 {
        0.45
    }
    pub(crate) fn absorption_oi_delta_min(&self) -> f64 {
        0.008
    }
    pub(crate) fn background_score_base(&self) -> f64 {
        10.0
    }
    pub(crate) fn background_mult(&self) -> f64 {
        1.1
    }
    pub(crate) fn magnet_threshold_ratio(&self) -> f64 {
        self.magnet_threshold_ratio
    }

    pub(crate) fn magnet_shrink_base(&self) -> f64 {
        self.magnet_shrink_base
    }
}

// ==================== RegimeConfig ====================
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegimeConfig {
    /// 趋势偏好 (>1 偏好趋势市，<1 偏好震荡市)
    pub trend_bias: f64,
    /// 动量敏感度 (0~1)
    pub momentum_sensitivity: f64,
    /// 主动流向权重 (0~1)
    pub taker_flow_weight: f64,
    /// 海啸触发门槛 (0~1)，值越小越易触发
    pub tsunami_threshold: f64,
    pub trend_persistence_boost: f64,
    pub range_persistence_boost: f64,
}

impl Default for RegimeConfig {
    fn default() -> Self {
        Self {
            trend_bias: 1.2,
            momentum_sensitivity: 0.6,
            taker_flow_weight: 0.7,
            tsunami_threshold: 0.5,
            trend_persistence_boost: 1.05,
            range_persistence_boost: 1.0,
        }
    }
}

impl RegimeConfig {
    pub(crate) fn mult_regime_trend(&self) -> f64 {
        1.2 + self.trend_bias * 0.4
    }
    pub(crate) fn mult_regime_range(&self) -> f64 {
        1.2 - self.trend_bias * 0.4
    }
    pub(crate) fn mult_regime_normal(&self) -> f64 {
        1.1
    }
    pub(crate) fn momentum_strong_boost(&self) -> f64 {
        1.0 + self.momentum_sensitivity * 0.8
    }
    pub(crate) fn momentum_weak_penalty(&self) -> f64 {
        1.0 - self.momentum_sensitivity * 0.5
    }
    pub(crate) fn momentum_compressed_penalty(&self) -> f64 {
        0.3
    }
    pub(crate) fn momentum_range_confluence(&self) -> f64 {
        2.0
    }
    pub(crate) fn momentum_dead_zone(&self) -> f64 {
        0.1
    }
    pub(crate) fn game_taker_smooth(&self) -> f64 {
        2.5
    }
    pub(crate) fn taker_trend_bull_min(&self) -> f64 {
        0.52
    }
    pub(crate) fn taker_trend_bear_max(&self) -> f64 {
        0.48
    }
    pub(crate) fn tsunami_base_oi_delta(&self) -> f64 {
        0.02 - self.tsunami_threshold * 0.015
    }

    pub(crate) fn slope_momentum_boost(&self) -> f64 {
        0.15
    }
    pub(crate) fn slope_bars_threshold(&self) -> i32 {
        3
    }
    pub(crate) fn max_mult_cap(&self) -> f64 {
        3.0
    }
    pub(crate) fn range_well_dist_threshold(&self) -> f64 {
        0.015
    }
}

// ==================== FakeoutConfig ====================
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FakeoutConfig {
    /// 假突破严厉程度 (0~1)
    pub severity: f64,
    /// 假突破持续性要求 (0~1)
    pub persistence: f64,
}

impl Default for FakeoutConfig {
    fn default() -> Self {
        Self {
            severity: 0.5,
            persistence: 0.5,
        }
    }
}

impl FakeoutConfig {
    pub(crate) fn fakeout_base_penalty(&self) -> f64 {
        15.0 + self.severity * 30.0
    }
    pub(crate) fn fakeout_confirm_bars(&self) -> usize {
        (1.0 + self.persistence * 2.0) as usize
    }
    pub(crate) fn fakeout_cooldown_bars(&self) -> usize {
        (2.0 + self.persistence * 3.0) as usize
    }
    pub(crate) fn fakeout_mult_penalty(&self) -> f64 {
        0.6
    }
    pub(crate) fn minor_fakeout_mult(&self) -> f64 {
        0.85
    }
    pub(crate) fn breach_atr_mult(&self) -> f64 {
        0.25
    }
    pub(crate) fn close_return_threshold(&self) -> f64 {
        0.001
    }
    pub(crate) fn slope_strong_threshold(&self) -> f64 {
        0.1
    }
    pub(crate) fn slope_strong_factor(&self) -> f64 {
        0.7
    }
    pub(crate) fn slope_weak_factor(&self) -> f64 {
        1.3
    }
    pub(crate) fn rsi_overbought_factor(&self) -> f64 {
        1.2
    }
    pub(crate) fn rsi_oversold_factor(&self) -> f64 {
        1.2
    }
    pub(crate) fn vol_eff_low_threshold(&self) -> f64 {
        0.3
    }
    pub(crate) fn vol_eff_high_threshold(&self) -> f64 {
        0.6
    }
    pub(crate) fn vol_surge_mult(&self) -> f64 {
        1.5
    }
    pub(crate) fn vol_shrink_mult(&self) -> f64 {
        0.7
    }
}

// ==================== SessionConfig ====================
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    pub asian_factor: f64,
    pub european_factor: f64,
    pub american_factor: f64,
    pub weekend_factor: f64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            asian_factor: 0.85,
            european_factor: 1.0,
            american_factor: 1.15,
            weekend_factor: 0.7,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EfficiencyConfig {
    /// 最低允许的相对成交量（避免除零，同时限制低量时的分母下限）
    pub min_rvol: f64,
    /// 低量惩罚的起始阈值（当 rvol < low_volume_threshold 时开始衰减）
    pub low_volume_threshold: f64,
    /// 低量惩罚的强度（0: 无惩罚，1: 完全按比例惩罚）
    pub low_volume_penalty_strength: f64,
    /// 紧凑度下限（防止影线过长导致效率被低估过度）
    pub min_compactness: f64,
    /// 效率上限
    pub max_efficiency: f64,
}

impl Default for EfficiencyConfig {
    fn default() -> Self {
        Self {
            min_rvol: 0.1,
            low_volume_threshold: 0.4,
            low_volume_penalty_strength: 0.8,
            min_compactness: 0.1,
            max_efficiency: 5.0,
        }
    }
}
/// 波动率因子计算配置
#[derive(Debug, Clone, Copy)]
pub struct VolFactorConfig {
    /// 波动率百分位数中点（此处因子为 1.0），默认 50.0
    pub mid_point: f64,
    /// 最小因子（vol_p = 0 时），默认 0.6
    pub min_factor: f64,
    /// 最大因子（vol_p = 100 时），默认 1.8
    pub max_factor: f64,
}

impl Default for VolFactorConfig {
    fn default() -> Self {
        Self {
            mid_point: 50.0,
            min_factor: 0.6,
            max_factor: 1.8,
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct VolAdaptationConfig {
    /// 关键点列表 (vol_p, factor)，必须按 vol_p 递增排序
    /// 默认使用四个点: (0.0, 1.2), (25.0, 1.0), (60.0, 0.85), (75.0, 0.7)
    pub knots: [(f64, f64); 4],
    /// 输出因子的安全下限，默认 0.5
    pub min_output: f64,
    /// 输出因子的安全上限，默认 1.5
    pub max_output: f64,
}

impl Default for VolAdaptationConfig {
    fn default() -> Self {
        Self {
            knots: [(0.0, 1.2), (25.0, 1.0), (60.0, 0.85), (75.0, 0.7)],
            min_output: 0.5,
            max_output: 1.5,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResonanceConfig {
    pub ma20_trigger_score: f64,
    pub macd_trigger_score: f64,
    pub early_trend_bars: i32,
    pub early_trend_mult: f64,
    pub aging_trend_bars: i32,
    pub aging_decay_period: f64,
    pub max_aging_penalty: f64,
    pub momentum_confirm_mult: f64,
    pub momentum_div_penalty: f64,
    pub bearish_div_long_penalty: f64,
    pub bearish_div_long_score_penalty: f64,
    pub bearish_div_short_boost: f64,
    pub bearish_div_short_score_boost: f64,
    pub bullish_div_long_boost: f64,
    pub bullish_div_long_score_boost: f64,
    pub bullish_div_short_penalty: f64,
    pub bullish_div_short_score_penalty: f64,
    pub mtf_misalign_penalty: f64,
}

impl Default for ResonanceConfig {
    fn default() -> Self {
        Self {
            ma20_trigger_score: 45.0,
            macd_trigger_score: 30.0,
            early_trend_bars: 12,
            early_trend_mult: 1.3,
            aging_trend_bars: 24,
            aging_decay_period: 30.0,
            max_aging_penalty: 0.7,
            momentum_confirm_mult: 1.25,
            momentum_div_penalty: 0.8,
            bearish_div_long_penalty: 0.6,
            bearish_div_long_score_penalty: 30.0,
            bearish_div_short_boost: 1.3,
            bearish_div_short_score_boost: 20.0,
            bullish_div_long_boost: 1.3,
            bullish_div_long_score_boost: 20.0,
            bullish_div_short_penalty: 0.6,
            bullish_div_short_score_penalty: 30.0,
            mtf_misalign_penalty: 0.7,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// 最低可接受加权盈亏比
    pub rr_min_acceptable: f64,
    /// MA20 极端乖离倍数（用于似然比）
    pub ma20_extreme_mult: f64,
    /// 三档止损的 ATR 缓冲倍数
    pub atr_sl_buffers: [f64; 3],
    /// 止损的最小 ATR 倍数（防止止损过近）
    pub min_sl_atr_mult: f64,
    /// 最大井强度上限（用于基础仓位计算）
    pub max_strength_cap: f64,
    /// 基础仓位上下限（占总资金比例）
    pub base_size_max: f64,
    pub min_base_size: f64,
    /// 贝叶斯先验置信度
    pub confidence_prior: f64,
    /// 最终仓位上下限
    pub min_position_size: f64,
    pub max_position_size: f64,
    /// 置信乘数裁剪范围
    pub mult_min: f64,
    pub mult_max: f64,
    /// 海啸模式下的止盈分配比例
    pub tsunami_allocation: [f64; 3],
    /// 跟踪止损的 ATR 倍数
    pub trailing_atr_mult: f64,
    /// 贝叶斯似然比
    pub lr_trend_strong: f64,
    pub lr_trend_weak: f64,
    pub lr_taker_aligned: f64,
    pub lr_taker_mismatch: f64,
    pub lr_tsunami: f64,
    /// 资金费率风控
    pub enable_funding_rate: bool,
    pub funding_rate_threshold: f64,
    pub funding_rate_penalty: f64,
    pub max_loss_per_trade: f64,
    pub entry_atr_step_mult: f64,
    pub default_entry_allocations: [f64; 3],
    pub direction_base_threshold: f64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            rr_min_acceptable: 2.2,
            ma20_extreme_mult: 3.5,
            atr_sl_buffers: [0.8, 1.5, 2.2],
            min_sl_atr_mult: 0.5,
            max_strength_cap: 3.5,
            base_size_max: 0.8,
            min_base_size: 0.15,
            confidence_prior: 0.5,
            min_position_size: 0.05,
            max_position_size: 0.8,
            mult_min: 0.4,
            mult_max: 1.6,
            tsunami_allocation: [0.2, 0.3, 0.5],
            trailing_atr_mult: 2.0,
            lr_trend_strong: 2.5,
            lr_trend_weak: 0.6,
            lr_taker_aligned: 1.8,
            lr_taker_mismatch: 0.7,
            lr_tsunami: 1.5,
            enable_funding_rate: false,
            funding_rate_threshold: 0.001,
            funding_rate_penalty: 0.7,
            max_loss_per_trade: 0.08,
            entry_atr_step_mult: 0.5,
            default_entry_allocations: [0.5, 0.3, 0.2],
            direction_base_threshold: 10.0,
        }
    }
}
