// ==================== 核心函数（可配置版本） ====================

use crate::config::{VolAdaptationConfig, VolFactorConfig};

/// 线性插值辅助函数
#[inline]
fn linear_interp(x: f64, x1: f64, x2: f64, y1: f64, y2: f64) -> f64 {
    y1 + (y2 - y1) * (x - x1) / (x2 - x1)
}

/// 计算波动率因子（线性映射 + 钳位）
///
/// # 公式
/// factor = 1.0 + (vol_p - mid_point) * (max_factor - 1.0) / (100.0 - mid_point)  当 vol_p >= mid_point
///         1.0 + (vol_p - mid_point) * (1.0 - min_factor) / mid_point              当 vol_p < mid_point
/// 然后 clamp 到 [min_factor, max_factor]
#[inline]
pub fn compute_vol_factor_with_config(vol_p: f64, cfg: &VolFactorConfig) -> f64 {
    let factor = if vol_p <= cfg.mid_point {
        // 左段：从 (0, min_factor) 到 (mid_point, 1.0)
        if cfg.mid_point <= 0.0 {
            1.0
        } else {
            let t = vol_p / cfg.mid_point;
            cfg.min_factor + (1.0 - cfg.min_factor) * t
        }
    } else {
        // 右段：从 (mid_point, 1.0) 到 (100, max_factor)
        let denom = 100.0 - cfg.mid_point;
        if denom <= 0.0 {
            1.0
        } else {
            let t = (vol_p - cfg.mid_point) / denom;
            1.0 + (cfg.max_factor - 1.0) * t
        }
    };
    factor.clamp(cfg.min_factor, cfg.max_factor)
}

#[inline]
pub fn volatility_adaptation_with_config(vol_p: f64, cfg: &VolAdaptationConfig) -> f64 {
    let knots = &cfg.knots;
    if knots.is_empty() {
        return 1.0;
    }

    if vol_p <= knots[0].0 {
        return knots[0].1.clamp(cfg.min_output, cfg.max_output);
    }
    if vol_p >= knots.last().unwrap().0 {
        return knots
            .last()
            .unwrap()
            .1
            .clamp(cfg.min_output, cfg.max_output);
    }

    // 找到所在区间并线性插值
    for i in 0..knots.len() - 1 {
        let (x1, y1) = knots[i];
        let (x2, y2) = knots[i + 1];
        if vol_p >= x1 && vol_p <= x2 {
            let factor = linear_interp(vol_p, x1, x2, y1, y2);
            return factor.clamp(cfg.min_output, cfg.max_output);
        }
    }

    1.0
}

// ==================== 默认配置的简单版本（保持原有签名） ====================

#[inline]
pub fn compute_vol_factor(vol_p: f64) -> f64 {
    compute_vol_factor_with_config(vol_p, &VolFactorConfig::default())
}

/// 使用默认配置的 volatility_adaptation（平滑连续版本）
#[inline]
pub fn volatility_adaptation(vol_p: f64) -> f64 {
    volatility_adaptation_with_config(vol_p, &VolAdaptationConfig::default())
}
