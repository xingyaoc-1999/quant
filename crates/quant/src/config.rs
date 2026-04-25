use serde::{Deserialize, Serialize};

// ==================== Top-Level Config ====================
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
    pub signal_stability: SignalStabilityConfig,
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
            signal_stability: SignalStabilityConfig::default(),
        }
    }
}

// ==================== GravityConfig ====================
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GravityConfig {
    /// Gravity range (0.3~1.8). Larger value = wider influence radius.
    pub gravity_range: f64,
    /// Minimum strength to consider a well valid (0.02~0.20).
    pub min_well_strength: f64,
    /// Merge sensitivity (0~1). Higher = more aggressive merging.
    pub merge_sensitivity: f64,
    /// Wear sensitivity (0~1). Higher = faster strength decay when hit.
    pub wear_sensitivity: f64,
    /// Wear recovery rate (0~1). Higher = faster strength restoration.
    pub wear_recovery_rate: f64,
    /// Threshold for marking a well as active.
    pub active_well_threshold: f64,
    /// Wear scales per role.
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
            entry: 0.7, // increased for 1h reliability
            ma20: 1.0,
        }
    }
}

// ==================== VolatilityConfig ====================
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolatilityConfig {
    pub extreme_low_ratio: f64,
    pub squeeze_ratio: f64,
    pub low_momentum_ratio: f64,
    pub meat_grinder_ratio: f64,
    pub acceleration_ratio: f64,
    pub dead_multiplier: f64,
    pub acceleration_multiplier: f64,
    pub weak_momentum_multiplier: f64,
    pub trend_resonance_multiplier: f64,
    pub squeeze_multiplier: f64,
    pub meat_grinder_multiplier: f64,
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
    /// Volume sensitivity (0~1). Higher = more sensitive to volume surges/shrinks.
    pub volume_sensitivity: f64,
    /// Efficiency threshold (0~1). Higher = stricter requirement for efficient moves.
    pub efficiency_threshold: f64,
    /// Aggressiveness of trend extension scoring (0~1).
    pub trend_extension_aggressiveness: f64,
    /// Magnet sensitivity (0~1). Higher = easier magnet activation.
    pub magnet_sensitivity: f64,

    // Volatility adaptation parameters
    pub vol_factor_mid: f64,
    pub vol_factor_min: f64,
    pub vol_factor_max: f64,
    pub vol_adapt_knots: [(f64, f64); 4],
    pub vol_adapt_min: f64,
    pub vol_adapt_max: f64,

    // Efficiency calculation parameters
    pub min_rvol: f64,
    pub low_volume_threshold: f64,
    pub low_volume_penalty_strength: f64,
    pub min_compactness: f64,
    pub max_efficiency: f64,
}

impl Default for VolumeConfig {
    fn default() -> Self {
        Self {
            volume_sensitivity: 0.5,
            efficiency_threshold: 0.5,
            trend_extension_aggressiveness: 0.6,
            magnet_sensitivity: 0.7,
            vol_factor_mid: 50.0,
            vol_factor_min: 0.6,
            vol_factor_max: 1.8,
            vol_adapt_knots: [(0.0, 1.2), (25.0, 1.0), (60.0, 0.85), (75.0, 0.7)],
            vol_adapt_min: 0.5,
            vol_adapt_max: 1.5,
            min_rvol: 0.1,
            low_volume_threshold: 0.4,
            low_volume_penalty_strength: 0.8,
            min_compactness: 0.1,
            max_efficiency: 5.0,
        }
    }
}

impl VolumeConfig {
    pub(crate) fn magnet_threshold_ratio(&self) -> f64 {
        0.5 + self.magnet_sensitivity * 0.5
    }
    pub(crate) fn magnet_shrink_base(&self) -> f64 {
        0.8 - self.magnet_sensitivity * 0.4
    }
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
}

// ==================== RegimeConfig ====================
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegimeConfig {
    pub trend_bias: f64,
    pub momentum_sensitivity: f64,
    pub taker_flow_weight: f64,
    pub tsunami_threshold: f64,
}

impl Default for RegimeConfig {
    fn default() -> Self {
        Self {
            trend_bias: 1.2,
            momentum_sensitivity: 0.6,
            taker_flow_weight: 0.7,
            tsunami_threshold: 0.5,
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
        2
    } // lowered for 1h
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
    pub severity: f64,
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
        1.2
    }
    pub(crate) fn minor_fakeout_mult(&self) -> f64 {
        1.2
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

// ==================== ResonanceConfig ====================
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResonanceConfig {
    pub ma20_trigger_score: f64,
    pub macd_trigger_score: f64,
    pub early_trend_bars: i32,
    pub early_trend_mult: f64,
    pub aging_trend_bars: i32,
    pub max_aging_penalty: f64,
    pub momentum_confirm_mult: f64,
    pub momentum_div_penalty: f64,
    pub bearish_div_short_boost: f64,
    pub bullish_div_long_boost: f64,
    pub mtf_misalign_penalty: f64,
    pub div_opposite_weaken_mult: f64, // 背离弱势方削弱系数，默认0.6
    pub aging_decay_period: f64,       // 老化衰减周期，默认30.0
    pub mtf_unknown_penalty: f64,      // 未知趋势惩罚，默认0.95
}

impl Default for ResonanceConfig {
    fn default() -> Self {
        Self {
            ma20_trigger_score: 45.0,
            macd_trigger_score: 30.0,
            early_trend_bars: 6, // lowered for 1h
            early_trend_mult: 1.3,
            aging_trend_bars: 24,
            max_aging_penalty: 0.7,
            momentum_confirm_mult: 1.25,
            momentum_div_penalty: 0.8,
            bearish_div_short_boost: 1.3,
            bullish_div_long_boost: 1.3,
            mtf_misalign_penalty: 0.7,
            div_opposite_weaken_mult: 0.6,
            aging_decay_period: 30.0,
            mtf_unknown_penalty: 0.95,
        }
    }
}

// ==================== RiskConfig ====================
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum EntryStrategy {
    Limit,
    Stop,
    #[default]
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    pub rr_min_acceptable: f64,
    pub ma20_extreme_mult: f64,
    pub atr_sl_buffers: [f64; 3],
    pub min_sl_atr_mult: f64,
    pub max_strength_cap: f64,
    pub base_size_max: f64,
    pub min_base_size: f64,
    pub confidence_prior: f64,
    pub min_position_size: f64,
    pub max_position_size: f64,
    pub mult_min: f64,
    pub mult_max: f64,
    pub tsunami_allocation: [f64; 3],
    pub trailing_atr_mult: f64,
    pub lr_trend_strong: f64,
    pub lr_trend_weak: f64,
    pub lr_taker_aligned: f64,
    pub lr_taker_mismatch: f64,
    pub lr_tsunami: f64,
    pub enable_funding_rate: bool,
    pub funding_rate_threshold: f64,
    pub funding_rate_penalty: f64,
    pub max_loss_per_trade: f64,
    pub entry_atr_step_mult: f64,
    pub default_entry_allocations: [f64; 3],
    pub direction_base_threshold: f64,
    pub min_weighted_rr: f64,
    pub entry_strategy: EntryStrategy,
    pub stop_entry_offset_pct: f64,
    pub tsunami_tp3_atr_mult: f64,          // 默认 5.0
    pub min_reliable_defense_strength: f64, // 默认 0.3
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
            enable_funding_rate: true,
            funding_rate_threshold: 0.001,
            funding_rate_penalty: 0.7,
            max_loss_per_trade: 0.08,
            entry_atr_step_mult: 0.5,
            default_entry_allocations: [0.5, 0.3, 0.2],
            direction_base_threshold: 10.0,
            min_weighted_rr: 1.2,
            entry_strategy: EntryStrategy::Hybrid,
            stop_entry_offset_pct: 0.001,
            tsunami_tp3_atr_mult: 5.0,
            min_reliable_defense_strength: 0.3,
        }
    }
}

// ==================== SignalStabilityConfig ====================
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalStabilityConfig {
    pub confirm_bars: usize,
    pub latch_bars: usize,
}

impl Default for SignalStabilityConfig {
    fn default() -> Self {
        Self {
            confirm_bars: 1,
            latch_bars: 2,
        }
    }
}
