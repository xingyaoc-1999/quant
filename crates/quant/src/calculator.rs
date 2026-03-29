use crate::types::{
    CandleType, DivergenceType, FeatureSet, MacdCross, MacdMomentum, MarketStructure, PriceAction,
    RsiState, SignalStates, SpaceGeometry, TechnicalIndicators, TrendStructure, VolumeState,
};
use chrono::{TimeZone, Utc};
use common::{Candle, Interval};
use std::collections::VecDeque;
use ta::{
    indicators::{
        AverageTrueRange, BollingerBands, MovingAverageConvergenceDivergence,
        RelativeStrengthIndex, SimpleMovingAverage,
    },
    Next,
};
#[derive(Debug, Clone, Copy)]
pub struct CalculatorConfig {
    pub warmup_period: usize,
    pub slope_period: usize,
    pub volume_expand_factor: f64,
    pub volume_shrink_factor: f64,
    pub ma_converge_threshold: f64,
    pub extreme_candle_atr_mult: f64,
    pub extreme_candle_body_ratio: f64,
    pub slope_deadzone: f64,
    pub doji_body_ratio: f64,
    pub rsi_range_3_low: f64,
    pub rsi_range_3_high: f64,
}

impl CalculatorConfig {
    pub fn from_interval(interval: Interval) -> Self {
        match interval {
            Interval::M1 | Interval::M5 => Self {
                warmup_period: 200,
                slope_period: 8,
                volume_expand_factor: 2.5,
                volume_shrink_factor: 0.5,
                ma_converge_threshold: 0.001,
                extreme_candle_atr_mult: 3.0,
                extreme_candle_body_ratio: 0.7,
                slope_deadzone: 0.0002,
                doji_body_ratio: 0.001,
                rsi_range_3_low: 45.0,
                rsi_range_3_high: 55.0,
            },
            Interval::M15 | Interval::M30 => Self {
                warmup_period: 150,
                slope_period: 5,
                volume_expand_factor: 2.0,
                volume_shrink_factor: 0.6,
                ma_converge_threshold: 0.004,
                extreme_candle_atr_mult: 2.5,
                extreme_candle_body_ratio: 0.7,
                slope_deadzone: 0.0004,
                doji_body_ratio: 0.001,
                rsi_range_3_low: 45.0,
                rsi_range_3_high: 55.0,
            },
            Interval::H1 | Interval::H4 => Self {
                warmup_period: 120,
                slope_period: 3,
                volume_expand_factor: 1.5,
                volume_shrink_factor: 0.7,
                ma_converge_threshold: 0.008,
                extreme_candle_atr_mult: 2.0,
                extreme_candle_body_ratio: 0.7,
                slope_deadzone: 0.001,
                doji_body_ratio: 0.001,
                rsi_range_3_low: 45.0,
                rsi_range_3_high: 55.0,
            },
            Interval::D1 => Self {
                warmup_period: 250,
                slope_period: 2,
                volume_expand_factor: 1.2,
                volume_shrink_factor: 0.8,
                ma_converge_threshold: 0.015,
                extreme_candle_atr_mult: 1.8,
                extreme_candle_body_ratio: 0.7,
                slope_deadzone: 0.002,
                doji_body_ratio: 0.001,
                rsi_range_3_low: 45.0,
                rsi_range_3_high: 55.0,
            },
        }
    }
}
#[derive(Debug, Clone)]

pub struct FeatureCalculator {
    // 1. 基础累加项 (Pearson 相关性使用)
    sum_x: f64,
    sum_y: f64,
    sum_x_sq: f64,
    sum_y_sq: f64,
    sum_xy: f64,

    // 2. 计数与状态
    count: usize,
    prev_close: Option<f64>,
    ma20_slope_bars: i32,
    prev_macd: Option<f64>,
    prev_signal: Option<f64>,
    prev_macd_histogram: Option<f64>,
    prev_ma20_satisfied: Option<bool>,

    volume_history: [Option<f64>; 3],
    rsi_history: [Option<f64>; 3],

    config: CalculatorConfig,
    rsi: RelativeStrengthIndex,
    ma20: SimpleMovingAverage,
    ma50: SimpleMovingAverage,
    ma200: SimpleMovingAverage,
    vma: SimpleMovingAverage,
    bb: BollingerBands,
    macd: MovingAverageConvergenceDivergence,
    atr: AverageTrueRange,

    // 5. 结构化队列 (堆分配)
    volatility_history: VecDeque<f64>,
    ma20_history: VecDeque<f64>,
    recent_highs: VecDeque<f64>,
    recent_lows: VecDeque<f64>,
    recent_macd_hists: VecDeque<f64>,
    recent_closes: VecDeque<f64>,
    recent_global_closes: VecDeque<f64>,
}

impl FeatureCalculator {
    const STRUCT_WINDOW: usize = 50;
    const VOL_WINDOW: usize = 200;

    pub fn new(interval: Interval) -> Self {
        let config = CalculatorConfig::from_interval(interval);
        Self {
            sum_x: 0.0,
            sum_y: 0.0,
            sum_x_sq: 0.0,
            sum_y_sq: 0.0,
            sum_xy: 0.0,
            count: 0,
            prev_close: None,
            ma20_slope_bars: 0,
            prev_macd: None,
            prev_signal: None,
            prev_macd_histogram: None,
            prev_ma20_satisfied: None,
            volume_history: [None; 3],
            rsi_history: [None; 3],
            config,
            rsi: RelativeStrengthIndex::new(14).unwrap(),
            ma20: SimpleMovingAverage::new(20).unwrap(),
            ma50: SimpleMovingAverage::new(50).unwrap(),
            ma200: SimpleMovingAverage::new(200).unwrap(),
            vma: SimpleMovingAverage::new(20).unwrap(),
            bb: BollingerBands::new(20, 2.0).unwrap(),
            macd: MovingAverageConvergenceDivergence::new(12, 26, 9).unwrap(),
            atr: AverageTrueRange::new(14).unwrap(),
            ma20_history: VecDeque::with_capacity(config.slope_period + 1),
            volatility_history: VecDeque::with_capacity(Self::VOL_WINDOW + 1),
            recent_highs: VecDeque::with_capacity(Self::STRUCT_WINDOW + 1),
            recent_lows: VecDeque::with_capacity(Self::STRUCT_WINDOW + 1),
            recent_macd_hists: VecDeque::with_capacity(Self::STRUCT_WINDOW + 1),
            recent_closes: VecDeque::with_capacity(Self::STRUCT_WINDOW + 1),
            recent_global_closes: VecDeque::with_capacity(Self::STRUCT_WINDOW + 1),
        }
    }

    pub fn next(
        &mut self,
        candle: &Candle,
        interval: Interval,
        global_close: Option<f64>,
    ) -> FeatureSet {
        self.count += 1;

        let rsi_v = self.rsi.next(candle.close);
        let m20_v = self.ma20.next(candle.close);
        let m50_v = self.ma50.next(candle.close);
        let m200_v = self.ma200.next(candle.close);
        let vma_v = self.vma.next(candle.volume);
        let atr_v = self.atr.next(candle);
        let bb_v = self.bb.next(candle.close);
        let macd_out = self.macd.next(candle.close); // 这里是导致类型问题的点，我们直接用其内部字段

        let is_warmed = self.count >= self.config.warmup_period;

        // 2. 波动率与历史滑窗
        let bb_w = if bb_v.average.abs() > f64::EPSILON {
            (bb_v.upper - bb_v.lower) / bb_v.average
        } else {
            0.0
        };
        if bb_w > 0.0 {
            Self::push_fixed_window(&mut self.volatility_history, bb_w, Self::VOL_WINDOW);
        }
        let vol_p = if self.volatility_history.len() > 20 {
            let smaller = self
                .volatility_history
                .iter()
                .filter(|&&v| v < bb_w)
                .count();
            (smaller as f64 / self.volatility_history.len() as f64) * 100.0
        } else {
            50.0
        };

        Self::shift_history(&mut self.volume_history, Some(candle.volume));
        Self::shift_history(
            &mut self.rsi_history,
            if rsi_v.is_nan() { None } else { Some(rsi_v) },
        );

        // 3. 维护队列与 Pearson 相关性 (纯增量模式)
        Self::push_fixed_window(&mut self.recent_highs, candle.high, Self::STRUCT_WINDOW);
        Self::push_fixed_window(&mut self.recent_lows, candle.low, Self::STRUCT_WINDOW);
        Self::push_fixed_window(
            &mut self.recent_macd_hists,
            macd_out.histogram,
            Self::STRUCT_WINDOW,
        );
        let correlation =
            global_close.map(|gc| self.update_correlation_incremental(candle.close, gc));

        let (dist_res, dist_sup) = if self.recent_highs.len() == Self::STRUCT_WINDOW {
            let res = self.recent_highs.iter().copied().fold(f64::MIN, f64::max);
            let sup = self.recent_lows.iter().copied().fold(f64::MAX, f64::min);
            if candle.close > f64::EPSILON {
                (
                    Some((res - candle.close) / candle.close),
                    Some((candle.close - sup) / candle.close),
                )
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        // 5. 信号与斜率
        let slope = self.update_ma20_slope(m20_v, atr_v);
        let macd_cross = if is_warmed {
            self.check_macd_cross(macd_out.macd, macd_out.signal)
        } else {
            None
        };

        let curr_above_m20 = candle.close > m20_v;
        let mut reclaim = None;
        let mut breakdown = None;
        if let Some(prev) = self.prev_ma20_satisfied {
            if !prev && curr_above_m20 {
                reclaim = Some(true);
            }
            if prev && !curr_above_m20 {
                breakdown = Some(true);
            }
        }

        // 6. 构造 FeatureSet
        let bucket = match Utc.timestamp_millis_opt(candle.timestamp) {
            chrono::LocalResult::Single(ts) => ts,
            _ => Utc::now(),
        };

        let fs = FeatureSet {
            bucket,
            symbol: candle.symbol,
            interval,
            price_action: PriceAction {
                open: candle.open,
                high: candle.high,
                low: candle.low,
                close: candle.close,
                volume: candle.volume,
                volatility_percentile: vol_p,
            },
            indicators: TechnicalIndicators {
                rsi_14: (self.count >= 14).then_some(rsi_v),
                ma_20: (self.count >= 20).then_some(m20_v),
                ma_50: (self.count >= 50).then_some(m50_v),
                ma_200: (self.count >= 200).then_some(m200_v),
                volume_ma_20: (self.count >= 20).then_some(vma_v),
                bb_upper: (self.count >= 20).then_some(bb_v.upper),
                bb_lower: (self.count >= 20).then_some(bb_v.lower),
                bb_width: Some(bb_w),
                atr_14: (self.count >= 14).then_some(atr_v),
                macd: (self.count >= 26).then_some(macd_out.macd),
                macd_signal: (self.count >= 34).then_some(macd_out.signal),
                macd_histogram: (self.count >= 34).then_some(macd_out.histogram),
            },
            structure: MarketStructure {
                trend_structure: self.get_trend_struct(candle.close, m20_v, m50_v, m200_v),
                rsi_state: (self.count >= 14).then_some(match rsi_v {
                    v if v >= 70.0 => RsiState::Overbought,
                    v if v <= 30.0 => RsiState::Oversold,
                    v if v > 60.0 => RsiState::Strong,
                    v if v < 40.0 => RsiState::Weak,
                    _ => RsiState::Neutral,
                }),
                volume_state: Some(
                    if candle.volume > vma_v * self.config.volume_expand_factor {
                        VolumeState::Expand
                    } else {
                        VolumeState::Normal
                    },
                ),
                candle_type: Some(self.identify_candle_type(candle)),
                ma20_slope: slope,
                ma20_slope_bars: self.ma20_slope_bars,
                mtf_aligned: (self.count >= 200).then_some(
                    (candle.close > m200_v && candle.close > m50_v)
                        || (candle.close < m200_v && candle.close < m50_v),
                ),
                correlation_with_global: correlation,
            },
            space: SpaceGeometry {
                ma20_dist_ratio: (m20_v.abs() > f64::EPSILON)
                    .then_some((candle.close - m20_v) / m20_v),
                dist_to_resistance: dist_res,
                dist_to_support: dist_sup,
                ma_converging: (m50_v.abs() > f64::EPSILON)
                    .then_some(((m20_v - m50_v).abs() / m50_v) < self.config.ma_converge_threshold),
            },
            signals: SignalStates {
                macd_divergence: self.check_macd_divergence(candle.close, macd_out.histogram),
                rsi_divergence: None,
                macd_cross,
                macd_momentum: self.prev_macd_histogram.map(|ph| {
                    if macd_out.histogram > ph {
                        MacdMomentum::Increasing
                    } else {
                        MacdMomentum::Decreasing
                    }
                }),
                ma20_reclaim: reclaim,
                ma20_breakdown: breakdown,
                rsi_range_3: Some(self.rsi_history.iter().all(|r| {
                    r.is_some_and(|v| {
                        (self.config.rsi_range_3_low..=self.config.rsi_range_3_high).contains(&v)
                    })
                })),
                volume_shrink_3: Some(self.is_volume_shrinking()),
                extreme_candle: Some(self.is_extreme_candle(candle, atr_v)),
            },
        };

        // 7. 更新状态
        self.prev_macd = Some(macd_out.macd);
        self.prev_signal = Some(macd_out.signal);
        self.prev_macd_histogram = Some(macd_out.histogram);
        self.prev_ma20_satisfied = Some(curr_above_m20);
        fs
    }

    // --- 内部逻辑函数 ---

    fn check_macd_cross(&self, cur_macd: f64, cur_signal: f64) -> Option<MacdCross> {
        match (self.prev_macd, self.prev_signal) {
            (Some(pm), Some(ps)) if pm <= ps && cur_macd > cur_signal => Some(MacdCross::Golden),
            (Some(pm), Some(ps)) if pm >= ps && cur_macd < cur_signal => Some(MacdCross::Death),
            _ => None,
        }
    }

    fn update_correlation_incremental(&mut self, new_x: f64, new_y: f64) -> f64 {
        let n = Self::STRUCT_WINDOW as f64;
        if self.recent_closes.len() == Self::STRUCT_WINDOW {
            let ox = self.recent_closes[0];
            let oy = self.recent_global_closes[0];
            self.sum_x -= ox;
            self.sum_y -= oy;
            self.sum_x_sq -= ox * ox;
            self.sum_y_sq -= oy * oy;
            self.sum_xy -= ox * oy;
        }
        self.sum_x += new_x;
        self.sum_y += new_y;
        self.sum_x_sq += new_x * new_x;
        self.sum_y_sq += new_y * new_y;
        self.sum_xy += new_x * new_y;

        Self::push_fixed_window(&mut self.recent_closes, new_x, Self::STRUCT_WINDOW);
        Self::push_fixed_window(&mut self.recent_global_closes, new_y, Self::STRUCT_WINDOW);

        if self.recent_closes.len() < Self::STRUCT_WINDOW {
            return 0.0;
        }
        let num = n * self.sum_xy - self.sum_x * self.sum_y;
        let den = ((n * self.sum_x_sq - self.sum_x.powi(2))
            * (n * self.sum_y_sq - self.sum_y.powi(2)))
        .sqrt();
        if den.abs() < 1e-9 {
            0.0
        } else {
            num / den
        }
    }

    fn check_macd_divergence(&self, cur_p: f64, cur_h: f64) -> Option<DivergenceType> {
        if self.recent_lows.len() < Self::STRUCT_WINDOW {
            return None;
        }
        let (min_idx, min_p) =
            self.recent_lows
                .iter()
                .enumerate()
                .fold(
                    (0, f64::MAX),
                    |acc, (i, &p)| if p < acc.1 { (i, p) } else { acc },
                );
        let (max_idx, max_p) =
            self.recent_highs
                .iter()
                .enumerate()
                .fold(
                    (0, f64::MIN),
                    |acc, (i, &p)| if p > acc.1 { (i, p) } else { acc },
                );

        let h_at_min = self.recent_macd_hists.get(min_idx).copied().unwrap_or(0.0);
        let h_at_max = self.recent_macd_hists.get(max_idx).copied().unwrap_or(0.0);

        if cur_p < min_p && cur_h > h_at_min {
            return Some(DivergenceType::Bullish);
        }
        if cur_p > max_p && cur_h < h_at_max {
            return Some(DivergenceType::Bearish);
        }
        None
    }

    fn identify_candle_type(&self, c: &Candle) -> CandleType {
        let body = (c.close - c.open).abs();
        let range = c.high - c.low;
        if range > f64::EPSILON && (body / range) < self.config.doji_body_ratio {
            CandleType::Doji
        } else if c.close > c.open {
            CandleType::BullishBody
        } else {
            CandleType::BearishBody
        }
    }

    fn is_volume_shrinking(&self) -> bool {
        self.volume_history[0].is_some()
            && self.volume_history[1].is_some()
            && self.volume_history[2].is_some()
            && self.volume_history[0].unwrap()
                < self.volume_history[1].unwrap() * self.config.volume_shrink_factor
            && self.volume_history[1].unwrap()
                < self.volume_history[2].unwrap() * self.config.volume_shrink_factor
    }

    fn is_extreme_candle(&self, c: &Candle, atr: f64) -> bool {
        let body = (c.close - c.open).abs();
        let range = c.high - c.low;
        body > atr * self.config.extreme_candle_atr_mult
            && (if range > f64::EPSILON {
                body / range
            } else {
                0.0
            }) > self.config.extreme_candle_body_ratio
    }

    fn update_ma20_slope(&mut self, m20: f64, atr: f64) -> Option<f64> {
        if m20.is_nan() || atr <= f64::EPSILON {
            return None;
        }
        Self::push_fixed_window(&mut self.ma20_history, m20, self.config.slope_period);
        if self.ma20_history.len() < self.config.slope_period {
            return None;
        }
        let s = (m20 - self.ma20_history[0]) / (atr * self.config.slope_period as f64);
        if s > self.config.slope_deadzone {
            self.ma20_slope_bars = self.ma20_slope_bars.max(0) + 1;
        } else if s < -self.config.slope_deadzone {
            self.ma20_slope_bars = self.ma20_slope_bars.min(0) - 1;
        } else {
            self.ma20_slope_bars = 0;
        }
        Some(s)
    }

    fn get_trend_struct(
        &self,
        close: f64,
        m20: f64,
        m50: f64,
        m200: f64,
    ) -> Option<TrendStructure> {
        if self.count < 50 {
            return None;
        }
        if self.count >= 200 {
            if close > m20 && m20 > m50 && m50 > m200 {
                return Some(TrendStructure::StrongBullish);
            }
            if close < m20 && m20 < m50 && m50 < m200 {
                return Some(TrendStructure::StrongBearish);
            }
        }
        if close > m20 && m20 > m50 {
            Some(TrendStructure::Bullish)
        } else if close < m20 && m20 < m50 {
            Some(TrendStructure::Bearish)
        } else {
            Some(TrendStructure::Range)
        }
    }

    #[inline(always)]
    fn push_fixed_window(queue: &mut VecDeque<f64>, value: f64, window: usize) {
        if queue.len() >= window {
            queue.pop_front();
        }
        queue.push_back(value);
    }

    #[inline(always)]
    fn shift_history(history: &mut [Option<f64>; 3], new_val: Option<f64>) {
        history[2] = history[1];
        history[1] = history[0];
        history[0] = new_val;
    }

    pub fn peek(
        &self,
        acc_candle: &Candle,
        interval: Interval,
        g_close: Option<f64>,
    ) -> FeatureSet {
        let mut cloned_calculator = self.clone();

        cloned_calculator.next(acc_candle, interval, g_close)
    }
}
