use crate::config::VolumeConfig;

/// 线性插值辅助函数
#[inline]
fn linear_interp(x: f64, x1: f64, x2: f64, y1: f64, y2: f64) -> f64 {
    y1 + (y2 - y1) * (x - x1) / (x2 - x1)
}

/// 计算波动率因子（用于动态阈值调整）
#[inline]
pub fn compute_vol_factor(vol_p: f64, cfg: &VolumeConfig) -> f64 {
    let factor = if vol_p <= cfg.vol_factor_mid {
        if cfg.vol_factor_mid <= f64::EPSILON {
            1.0
        } else {
            let t = vol_p / cfg.vol_factor_mid;
            cfg.vol_factor_min + (1.0 - cfg.vol_factor_min) * t
        }
    } else {
        let denom = 100.0 - cfg.vol_factor_mid;
        if denom <= f64::EPSILON {
            1.0
        } else {
            let t = (vol_p - cfg.vol_factor_mid) / denom;
            1.0 + (cfg.vol_factor_max - 1.0) * t
        }
    };
    factor.clamp(cfg.vol_factor_min, cfg.vol_factor_max)
}

/// 计算波动率自适应因子（用于分数缩放）
#[inline]
pub fn volatility_adaptation(vol_p: f64, cfg: &VolumeConfig) -> f64 {
    let knots = &cfg.vol_adapt_knots;
    if knots.is_empty() {
        return 1.0;
    }

    // 边界处理
    if vol_p <= knots[0].0 {
        return knots[0].1.clamp(cfg.vol_adapt_min, cfg.vol_adapt_max);
    }
    if vol_p >= knots.last().unwrap().0 {
        return knots
            .last()
            .unwrap()
            .1
            .clamp(cfg.vol_adapt_min, cfg.vol_adapt_max);
    }

    // 分段线性插值
    for i in 0..knots.len() - 1 {
        let (x1, y1) = knots[i];
        let (x2, y2) = knots[i + 1];
        if vol_p >= x1 && vol_p <= x2 {
            let factor = linear_interp(vol_p, x1, x2, y1, y2);
            return factor.clamp(cfg.vol_adapt_min, cfg.vol_adapt_max);
        }
    }

    1.0
}
