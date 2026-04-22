use crate::config::VolumeConfig;
use crate::types::market::{PriceAction, VolumeState};

/// 计算量价效率，返回 (efficiency, rvol)
pub fn calculate_efficiency(
    price_action: &PriceAction,
    avg_volume: f64,
    atr: f64,
    cfg: &VolumeConfig,
) -> (f64, f64) {
    let rvol = if avg_volume > f64::EPSILON {
        price_action.volume / avg_volume
    } else {
        1.0
    };
    if rvol < cfg.min_rvol {
        return (0.05, rvol);
    }

    let body_spread = (price_action.close - price_action.open).abs();
    let total_travel = (price_action.high - price_action.low).max(f64::EPSILON);
    let body_ratio = body_spread / total_travel;
    let compactness = if body_ratio < cfg.min_compactness {
        cfg.min_compactness
    } else {
        body_ratio
    };
    let normalized_move = if atr > f64::EPSILON {
        body_spread / atr
    } else {
        0.0
    };

    // 低量惩罚
    let volume_penalty = if rvol < cfg.low_volume_threshold {
        1.0 - cfg.low_volume_penalty_strength * (1.0 - rvol / cfg.low_volume_threshold)
    } else {
        1.0
    };

    let raw_efficiency = (normalized_move / rvol) * compactness * volume_penalty;
    (raw_efficiency.min(cfg.max_efficiency), rvol)
}

/// 量价一致性惩罚
pub fn consistency_penalty(rvol: f64, volume_state: Option<VolumeState>) -> f64 {
    match volume_state {
        Some(VolumeState::Expand) if rvol < 1.0 => 0.7,
        Some(VolumeState::Shrink) if rvol > 0.8 => 0.7,
        Some(VolumeState::Normal) if rvol > 1.5 || rvol < 0.5 => 0.8,
        _ => 1.0,
    }
}
