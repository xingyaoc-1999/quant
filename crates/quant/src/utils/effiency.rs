use crate::{
    config::EfficiencyConfig,
    types::market::{PriceAction, VolumeState},
};

pub fn calculate_efficiency(
    price_action: &PriceAction,
    avg_volume: f64,
    atr: f64,
    cfg: &EfficiencyConfig,
) -> (f64, f64) {
    let rvol = if avg_volume > f64::EPSILON {
        price_action.volume / avg_volume
    } else {
        1.0
    };

    // 价格幅度
    let body_spread = (price_action.close - price_action.open).abs();
    let total_travel = (price_action.high - price_action.low).max(f64::EPSILON);
    let compactness = (body_spread / total_travel).clamp(cfg.min_compactness, 1.0);

    // 归一化价格变动（相对ATR）
    let normalized_move = if atr > f64::EPSILON {
        body_spread / atr
    } else {
        0.0
    };

    // 量能加权因子（S型曲线：低量时权重小，高量时趋近1）
    // 使用半饱和点 rvol_half = 0.5 作为参考，可通过配置调整
    let rvol_half = 0.5;
    let volume_weight = (rvol / (rvol + rvol_half)).sqrt();

    // 低量惩罚因子（连续衰减，当 rvol < low_volume_threshold 时开始降低）
    let low_vol_penalty = if rvol < cfg.low_volume_threshold {
        let t = rvol / cfg.low_volume_threshold; // t in [0, 1)
                                                 // 惩罚曲线: 1 - strength * (1 - t)^2
        (1.0 - cfg.low_volume_penalty_strength * (1.0 - t).powi(2)).max(0.0)
    } else {
        1.0
    };

    let raw_efficiency = normalized_move * compactness * volume_weight;
    // 应用低量惩罚
    let efficiency = (raw_efficiency * low_vol_penalty).min(cfg.max_efficiency);

    (efficiency, rvol)
}

pub fn consistency_penalty(rvol: f64, volume_state: Option<VolumeState>) -> f64 {
    let (ideal_min, ideal_max) = match volume_state {
        Some(VolumeState::Expand) => (1.2, 2.5), // 扩张期：相对成交量应偏高
        Some(VolumeState::Shrink) => (0.3, 0.8), // 收缩期：相对成交量应偏低
        Some(VolumeState::Normal) => (0.7, 1.3), // 正常期：接近均值
        None => return 1.0,                      // 无状态信息时不惩罚
    };

    let deviation = if rvol < ideal_min {
        (ideal_min - rvol) / ideal_min
    } else if rvol > ideal_max {
        (rvol - ideal_max) / ideal_max
    } else {
        0.0
    };

    let penalty = 1.0 - (deviation * 0.8).min(0.5);
    penalty.max(0.5).min(1.0)
}
