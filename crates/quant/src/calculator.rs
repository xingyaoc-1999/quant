use crate::types::{
    CandleType, DivergenceType, FeatureSet, MacdCross, MacdMomentum, MarketStructure, PriceAction,
    RsiState, SignalStates, SpaceGeometry, TechnicalIndicators, TrendStructure, VolumeState,
};
use chrono::{TimeZone, Utc};
use common::{Candle, Interval};
use std::collections::VecDeque;
use ta::{
    indicators::{
        AverageTrueRange, BollingerBands, ExponentialMovingAverage,
        MovingAverageConvergenceDivergence, RelativeStrengthIndex, SimpleMovingAverage,
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
    // 新增：窗口大小配置
    pub struct_window: usize,
    pub vol_window: usize,
}

impl CalculatorConfig {
    /// 基础配置（适用于 H1/H4）
    fn base_config() -> Self {
        Self {
            warmup_period: 120,
            slope_period: 3,
            volume_expand_factor: 1.5,
            volume_shrink_factor: 0.7,
            ma_converge_threshold: 0.008,
            extreme_candle_atr_mult: 2.0,
            extreme_candle_body_ratio: 0.7,
            slope_deadzone: 0.001,
            doji_body_ratio: 0.05,
            rsi_range_3_low: 45.0,
            rsi_range_3_high: 55.0,
            struct_window: 50,
            vol_window: 200,
        }
    }

    pub fn from_interval(interval: Interval) -> Self {
        let mut cfg = Self::base_config();
        match interval {
            Interval::M1 | Interval::M5 => {
                cfg.warmup_period = 200;
                cfg.slope_period = 8;
                cfg.volume_expand_factor = 2.5;
                cfg.volume_shrink_factor = 0.5;
                cfg.ma_converge_threshold = 0.001;
                cfg.extreme_candle_atr_mult = 3.0;
                cfg.slope_deadzone = 0.0002;
                cfg.struct_window = 50;
                cfg.vol_window = 200;
            }
            Interval::M15 | Interval::M30 => {
                cfg.warmup_period = 150;
                cfg.slope_period = 5;
                cfg.volume_expand_factor = 2.0;
                cfg.volume_shrink_factor = 0.6;
                cfg.ma_converge_threshold = 0.004;
                cfg.extreme_candle_atr_mult = 2.5;
                cfg.slope_deadzone = 0.0004;
            }
            Interval::H1 | Interval::H4 => {
                // 保持基础配置
            }
            Interval::D1 => {
                cfg.warmup_period = 250;
                cfg.slope_period = 2;
                cfg.volume_expand_factor = 1.2;
                cfg.volume_shrink_factor = 0.8;
                cfg.ma_converge_threshold = 0.015;
                cfg.extreme_candle_atr_mult = 1.8;
                cfg.slope_deadzone = 0.002;
            }
        }
        cfg
    }
}

#[derive(Debug, Clone)]
pub struct FeatureCalculator {
    count: usize,
    ma20_slope_bars: i32,
    prev_macd: Option<f64>,
    prev_signal: Option<f64>,
    prev_macd_histogram: Option<f64>,
    prev_ma20_satisfied: Option<bool>,

    volume_history: [Option<f64>; 3],
    rsi_history: [Option<f64>; 3],

    // --- 极值与磨损状态追踪 ---
    current_res: Option<f64>,
    res_hit_count: u32,
    res_last_hit: i64,

    current_sup: Option<f64>,
    sup_hit_count: u32,
    sup_last_hit: i64,
    // ---------------------------------
    config: CalculatorConfig,
    rsi: RelativeStrengthIndex,
    ma20: SimpleMovingAverage,
    ma50: ExponentialMovingAverage,
    ma200: ExponentialMovingAverage,
    vma: SimpleMovingAverage,
    bb: BollingerBands,
    macd: MovingAverageConvergenceDivergence,
    atr: AverageTrueRange,

    volatility_history: VecDeque<f64>,
    ma20_history: VecDeque<f64>,
    recent_highs: VecDeque<f64>, // 修复：原为 f65，现为 f64
    recent_lows: VecDeque<f64>,
    recent_macd_hists: VecDeque<f64>,
    recent_closes: VecDeque<f64>,
    recent_global_closes: VecDeque<f64>,
}

impl FeatureCalculator {
    pub fn new(interval: Interval) -> Self {
        let config = CalculatorConfig::from_interval(interval);
        let struct_window = config.struct_window;
        let vol_window = config.vol_window;

        Self {
            count: 0,
            ma20_slope_bars: 0,
            prev_macd: None,
            prev_signal: None,
            prev_macd_histogram: None,
            prev_ma20_satisfied: None,
            volume_history: [None; 3],
            rsi_history: [None; 3],

            current_res: None,
            res_hit_count: 0,
            res_last_hit: 0,
            current_sup: None,
            sup_hit_count: 0,
            sup_last_hit: 0,

            config,
            rsi: RelativeStrengthIndex::new(14).unwrap(),
            ma20: SimpleMovingAverage::new(20).unwrap(),
            ma50: ExponentialMovingAverage::new(50).unwrap(),
            ma200: ExponentialMovingAverage::new(200).unwrap(),
            vma: SimpleMovingAverage::new(20).unwrap(),
            bb: BollingerBands::new(20, 2.0).unwrap(),
            macd: MovingAverageConvergenceDivergence::new(12, 26, 9).unwrap(),
            atr: AverageTrueRange::new(14).unwrap(),

            ma20_history: VecDeque::with_capacity(config.slope_period + 1),
            volatility_history: VecDeque::with_capacity(vol_window + 1),
            recent_highs: VecDeque::with_capacity(struct_window + 1),
            recent_lows: VecDeque::with_capacity(struct_window + 1),
            recent_macd_hists: VecDeque::with_capacity(struct_window + 1),
            recent_closes: VecDeque::with_capacity(struct_window + 1),
            recent_global_closes: VecDeque::with_capacity(struct_window + 1),
        }
    }

    pub fn next(
        &mut self,
        candle: &Candle,
        interval: Interval,
        global_close: Option<f64>,
    ) -> FeatureSet {
        self.count += 1;

        // 1. 基础指标
        let rsi_v = self.rsi.next(candle.close);
        let m20_v = self.ma20.next(candle.close);
        let m50_v = self.ma50.next(candle.close);
        let m200_v = self.ma200.next(candle.close);
        let vma_v = self.vma.next(candle.volume);
        let atr_v = self.atr.next(candle);
        let bb_v = self.bb.next(candle.close);
        let macd_out = self.macd.next(candle.close);

        let is_warmed = self.count >= self.config.warmup_period;

        // 2. 波动率与百分位
        let bb_w = if bb_v.average.abs() > f64::EPSILON {
            (bb_v.upper - bb_v.lower) / bb_v.average
        } else {
            0.0
        };

        let vol_p = if self.volatility_history.len() >= 20 {
            let smaller = self
                .volatility_history
                .iter()
                .filter(|&&v| v < bb_w)
                .count();
            (smaller as f64 / self.volatility_history.len() as f64) * 100.0
        } else {
            50.0
        };

        // 3. 极值状态机与磨损计算
        let mut dist_res = None;
        let mut dist_sup = None;

        if self.recent_highs.len() >= 10 {
            let window_res = self.recent_highs.iter().copied().fold(f64::MIN, f64::max);
            let window_sup = self.recent_lows.iter().copied().fold(f64::MAX, f64::min);
            let hit_margin = atr_v.max(candle.close * 0.0015);

            // 阻力位
            if window_res > self.current_res.unwrap_or(0.0) + f64::EPSILON {
                self.current_res = Some(window_res);
                self.res_hit_count = 1;
                self.res_last_hit = candle.timestamp;
            } else if let Some(r) = self.current_res {
                if candle.high >= r - hit_margin && candle.timestamp != self.res_last_hit {
                    self.res_hit_count += 1;
                    self.res_last_hit = candle.timestamp;
                }
            }

            // 支撑位
            if window_sup < self.current_sup.unwrap_or(f64::MAX) - f64::EPSILON {
                self.current_sup = Some(window_sup);
                self.sup_hit_count = 1;
                self.sup_last_hit = candle.timestamp;
            } else if let Some(s) = self.current_sup {
                if candle.low <= s + hit_margin && candle.timestamp != self.sup_last_hit {
                    self.sup_hit_count += 1;
                    self.sup_last_hit = candle.timestamp;
                }
            }

            if candle.close > f64::EPSILON {
                dist_res = Some((window_res - candle.close) / candle.close);
                dist_sup = Some((candle.close - window_sup) / candle.close);
            }
        }

        // 4. MACD 背离
        let macd_divergence = self.check_macd_divergence(candle.close, macd_out.histogram);

        // 5. 更新滑窗（使用配置的窗口大小）
        Self::push_fixed_window(&mut self.volatility_history, bb_w, self.config.vol_window);
        Self::push_fixed_window(
            &mut self.recent_highs,
            candle.high,
            self.config.struct_window,
        );
        Self::push_fixed_window(&mut self.recent_lows, candle.low, self.config.struct_window);
        Self::push_fixed_window(
            &mut self.recent_macd_hists,
            macd_out.histogram,
            self.config.struct_window,
        );
        Self::push_fixed_window(
            &mut self.recent_closes,
            candle.close,
            self.config.struct_window,
        );
        if let Some(gc) = global_close {
            Self::push_fixed_window(
                &mut self.recent_global_closes,
                gc,
                self.config.struct_window,
            );
        }

        // 6. 相关性计算
        let correlation = self.calculate_correlation_stable();

        // 7. 信号与斜率
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

        // 8. 构造 FeatureSet
        let bucket = match Utc.timestamp_millis_opt(candle.timestamp) {
            chrono::LocalResult::Single(ts) => ts,
            _ => Utc::now(),
        };

        let vs = if candle.volume > vma_v * self.config.volume_expand_factor {
            VolumeState::Expand
        } else if candle.volume < vma_v * self.config.volume_shrink_factor {
            VolumeState::Shrink
        } else {
            VolumeState::Normal
        };

        // ========== 新增：提取最近3根收盘价 ==========
        let mut rec_closes = [candle.close; 3];
        let len = self.recent_closes.len();
        if len >= 1 {
            rec_closes[1] = self.recent_closes[len - 1];
        }
        if len >= 2 {
            rec_closes[2] = self.recent_closes[len - 2];
        }
        // ===========================================

        let space = SpaceGeometry {
            dist_to_resistance: dist_res,
            dist_to_support: dist_sup,
            sup_hit_count: self.sup_hit_count,
            sup_last_hit: self.sup_last_hit,
            res_hit_count: self.res_hit_count,
            res_last_hit: self.res_last_hit,
            ma20_dist_ratio: (m20_v > 0.0).then_some((candle.close - m20_v) / m20_v),
            ma50_dist_ratio: (m50_v > 0.0).then_some((candle.close - m50_v) / m50_v),
            ma200_dist_ratio: (m200_v > 0.0).then_some((candle.close - m200_v) / m200_v),
            ma_converging: (m50_v > 0.0)
                .then_some(((m20_v - m50_v).abs() / m50_v) < self.config.ma_converge_threshold),
        };

        FeatureSet {
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
                volume_state: Some(vs),
                candle_type: Some(self.identify_candle_type(candle)),
                ma20_slope: slope,
                ma20_slope_bars: self.ma20_slope_bars,
                mtf_aligned: (self.count >= 200).then_some(
                    (candle.close > m200_v && candle.close > m50_v)
                        || (candle.close < m200_v && candle.close < m50_v),
                ),
                correlation_with_global: correlation,
            },
            space,
            signals: SignalStates {
                macd_divergence,
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
            recent_closes: rec_closes, // 新增字段
        }
    }

    fn calculate_correlation_stable(&self) -> Option<f64> {
        let n = self.recent_closes.len();
        if n < self.config.struct_window || n != self.recent_global_closes.len() {
            return None;
        }

        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        let mut sum_x_sq = 0.0;
        let mut sum_y_sq = 0.0;
        let mut sum_xy = 0.0;

        for (&x, &y) in self
            .recent_closes
            .iter()
            .zip(self.recent_global_closes.iter())
        {
            sum_x += x;
            sum_y += y;
            sum_x_sq += x * x;
            sum_y_sq += y * y;
            sum_xy += x * y;
        }

        let nf = n as f64;
        let num = nf * sum_xy - sum_x * sum_y;
        let den = ((nf * sum_x_sq - sum_x.powi(2)).max(0.0)
            * (nf * sum_y_sq - sum_y.powi(2)).max(0.0))
        .sqrt();

        if den < 1e-9 {
            Some(0.0)
        } else {
            Some(num / den)
        }
    }

    fn check_macd_cross(&self, cur_macd: f64, cur_signal: f64) -> Option<MacdCross> {
        match (self.prev_macd, self.prev_signal) {
            (Some(pm), Some(ps)) if pm <= ps && cur_macd > cur_signal => Some(MacdCross::Golden),
            (Some(pm), Some(ps)) if pm >= ps && cur_macd < cur_signal => Some(MacdCross::Death),
            _ => None,
        }
    }

    fn check_macd_divergence(&self, cur_p: f64, cur_h: f64) -> Option<DivergenceType> {
        if self.recent_lows.len() < self.config.struct_window {
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
        Self::push_fixed_window(&mut self.ma20_history, m20, self.config.slope_period + 1);

        if self.ma20_history.len() < self.config.slope_period + 1 {
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
        while queue.len() >= window {
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
