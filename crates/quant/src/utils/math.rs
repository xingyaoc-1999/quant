use std::collections::VecDeque;

// ==================== 滑动窗口 ====================

/// 向固定大小的滑动窗口中推入新值。
/// 如果窗口已满，则弹出最旧的值。
///
/// # 参数
/// - `queue`: 双端队列，用于存储窗口数据
/// - `value`: 待推入的新值
/// - `window`: 窗口最大长度（必须 > 0，否则函数直接返回）
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

// ==================== 中位数计算 ====================

/// 计算浮点数切片的中位数（原地修改，平均时间复杂度 O(N)）。
///
/// 使用快速选择算法（`select_nth_unstable_by`）使得索引 `mid` 处的元素成为
/// 第 mid 小的元素，且左侧元素均 ≤ 它，右侧元素均 ≥ 它。
///
/// # 注意
/// - 调用此函数后，`values` 中的元素顺序会被改变（处于半排序状态）。
/// - 所有的 `NaN` 会被视为与任何值相等进行比较，因此如果中位数位置恰为 `NaN`，返回值为 `NaN`。
/// - 空切片返回 `None`。
///
/// # 返回值
/// - `Some(median)`: 中位数
/// - `None`: 输入切片为空
pub fn median(values: &mut [f64]) -> Option<f64> {
    let len = values.len();
    if len == 0 {
        return None;
    }

    let mid = len / 2;

    // 第一次快速选择：使 values[mid] 成为第 mid 小的元素
    values.select_nth_unstable_by(mid, |a, b| {
        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
    });

    if len % 2 == 1 {
        // 奇数长度：中间元素即为中位数
        Some(values[mid])
    } else {
        // 偶数长度：需要左中位数（第 mid-1 小的元素）
        // 左中位数必然在 values[..mid] 中，且是其中的最大值
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
