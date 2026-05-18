use std::collections::VecDeque;

use crate::types::market::{TradeDirection, TrendStructure};

#[inline]
pub fn push_fixed_window(queue: &mut VecDeque<f64>, value: f64, window: usize) {
    if window == 0 {
        return;
    }
    if queue.len() >= window {
        queue.pop_front();
    }
    queue.push_back(value);
}

pub fn price_to_key(price: f64) -> i64 {
    (price * 100_000_000.0).round() as i64
}
pub fn median(values: &mut [f64]) -> Option<f64> {
    let len = values.len();
    if len == 0 {
        return None;
    }

    let mid = len / 2;

    values.select_nth_unstable_by(mid, |a, b| {
        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
    });

    if len % 2 == 1 {
        // 奇数长度：中间元素即为中位数
        Some(values[mid])
    } else {
        let left_max = values[..mid]
            .iter()
            .copied()
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .expect("左半部分非空，因为 len >= 2");
        Some((left_max + values[mid]) / 2.0)
    }
}

// ==================== 单元测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_fixed_window() {
        let mut queue = VecDeque::new();

        // 正常推送
        push_fixed_window(&mut queue, 1.0, 3);
        push_fixed_window(&mut queue, 2.0, 3);
        push_fixed_window(&mut queue, 3.0, 3);
        assert_eq!(queue, vec![1.0, 2.0, 3.0]);

        // 超出容量时顶替
        push_fixed_window(&mut queue, 4.0, 3);
        assert_eq!(queue, vec![2.0, 3.0, 4.0]);

        let mut empty_queue = VecDeque::new();
        push_fixed_window(&mut empty_queue, 1.0, 0);
        assert!(empty_queue.is_empty());
    }
}

pub fn dynamic_direction_threshold(
    score: f64,
    vol_p: f64,
    regime: TrendStructure,
    confidence_mult: f64,
    base_threshold: f64,
) -> Option<TradeDirection> {
    let vol_factor = if vol_p > 70.0 {
        1.3
    } else if vol_p < 30.0 {
        0.7
    } else {
        1.0
    };

    let regime_factor = match regime {
        TrendStructure::StrongBullish | TrendStructure::StrongBearish => 0.8,
        TrendStructure::Range => 1.2,
        _ => 1.0,
    };

    let confidence_factor = 1.0 / confidence_mult.clamp(0.5, 2.0).sqrt();
    let threshold = base_threshold * vol_factor * regime_factor * confidence_factor;

    if score > threshold {
        Some(TradeDirection::Long)
    } else if score < -threshold {
        Some(TradeDirection::Short)
    } else {
        None
    }
}
#[inline]
pub fn sigmoid(x: f64, mid: f64, k: f64) -> f64 {
    1.0 / (1.0 + (-k * (x - mid)).exp())
}
#[inline]
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t.clamp(0.0, 1.0)
}
