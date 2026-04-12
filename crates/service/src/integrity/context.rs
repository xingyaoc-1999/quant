use chrono::{DateTime, Utc};
use common::{config::Appconfig, Candle, Interval, OpenInterestRecord, Symbol};
use dashmap::DashMap;
use quant::{
    analyzer::{MarketContext, Role},
    calculator::FeatureCalculator,
    types::{DerivativeSnapshot, OIData, RoleData, TakerFlowData},
};
use rayon::prelude::*;
use std::collections::{HashMap, VecDeque};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    RwLock,
};
use tracing::{info, warn};

// ==================== 1. Role 处理器 (逻辑计算单元) ====================

pub struct RoleProcessor {
    pub interval: Interval,
    pub calculator: FeatureCalculator,
    pub current_acc: Option<Candle>,
    pub last_processed_ts: i64,

    // --- 惰性计算相关字段 ---
    pub last_calc_volume: f64,              // 上次计算时的累积成交量
    pub last_calc_ts: i64,                  // 上次计算时的 Acc 时间戳
    pub cached_role_data: Option<RoleData>, // 缓存上一次 Peek 的结果

    pub oi_history: VecDeque<f64>,
    pub max_history_size: usize,
}

impl RoleProcessor {
    pub fn new(interval: Interval) -> Self {
        let max_history_size = match interval {
            Interval::D1 => 31,
            Interval::H4 => 61,
            Interval::M15 => 121,
            _ => 61,
        };

        Self {
            interval,
            calculator: FeatureCalculator::new(interval),
            current_acc: None,
            last_processed_ts: 0,
            last_calc_volume: -1.0,
            last_calc_ts: 0,
            cached_role_data: None,
            oi_history: VecDeque::with_capacity(max_history_size),
            max_history_size,
        }
    }

    /// 检查当前累加器数据是否相对于缓存已更新
    #[inline]
    pub fn is_dirty(&self) -> bool {
        if let Some(acc) = &self.current_acc {
            return acc.timestamp > self.last_calc_ts
                || (acc.volume - self.last_calc_volume).abs() > f64::EPSILON;
        }
        false
    }

    pub fn process_m1(&mut self, m1: &Candle) -> Option<Candle> {
        let interval_ms = self.interval.to_millis();
        if interval_ms == 0 {
            return None;
        }
        let bucket_ts = (m1.timestamp / interval_ms) * interval_ms;

        if let Some(ref mut acc) = self.current_acc {
            if bucket_ts > acc.timestamp {
                // 周期切换，返回旧的闭合 Bar
                return self.current_acc.replace(Candle {
                    timestamp: bucket_ts,
                    ..*m1
                });
            }
            // 增量更新当前 Acc
            acc.high = acc.high.max(m1.high);
            acc.low = acc.low.min(m1.low);
            acc.close = m1.close;
            acc.volume += m1.volume;
            acc.quote_volume += m1.quote_volume;
            acc.trade_count += m1.trade_count;
            acc.taker_buy_volume += m1.taker_buy_volume;
            acc.taker_buy_quote_volume += m1.taker_buy_quote_volume;
            None
        } else {
            self.current_acc = Some(Candle {
                timestamp: bucket_ts,
                ..*m1
            });
            None
        }
    }

    pub fn generate_oi_data(&self, cur_price: f64, cur_oi: f64, cur_oi_val: f64) -> Option<OIData> {
        if cur_price <= f64::EPSILON || cur_oi <= f64::EPSILON {
            return None;
        }

        let steps = [1, 3, 7, 14];
        let len = self.oi_history.len();
        if len == 0 {
            return Some(OIData::new(cur_oi, cur_oi_val, vec![0.0; 4]));
        }

        let is_already_pushed = self
            .oi_history
            .back()
            .map_or(false, |&last| (last - cur_oi).abs() < f64::EPSILON);
        let offset = if is_already_pushed { 1 } else { 0 };

        let change_history = steps
            .iter()
            .map(|&step| {
                if let Some(idx) = len.checked_sub(step + offset) {
                    let past_val = self.oi_history[idx];
                    if past_val > 1e-9 {
                        let diff = (cur_oi - past_val) / past_val;
                        if diff.is_finite() {
                            return diff;
                        }
                    }
                }
                0.0
            })
            .collect();

        Some(OIData::new(cur_oi, cur_oi_val, change_history))
    }
}

// ==================== 2. 上下文管理器 (FeatureContextManager) ====================

pub struct SymbolContext {
    pub roles: RwLock<HashMap<Role, RoleProcessor>>,
    pub latest_snap: RwLock<DerivativeSnapshot>,
}

pub struct FeatureContextManager {
    pub registry: DashMap<Symbol, MarketContext>,
    pub symbol_contexts: DashMap<Symbol, SymbolContext>,
    pub global_btc_price: AtomicU64,
}

impl FeatureContextManager {
    pub fn new(symbols: &[Symbol]) -> Self {
        let symbol_contexts = DashMap::new();
        let cfg = Appconfig::global();

        for &symbol in symbols {
            symbol_contexts.insert(
                symbol,
                SymbolContext {
                    roles: RwLock::new(HashMap::from([
                        (Role::Trend, RoleProcessor::new(cfg.role.trend)),
                        (Role::Filter, RoleProcessor::new(cfg.role.filter)),
                        (Role::Entry, RoleProcessor::new(cfg.role.entry)),
                    ])),
                    latest_snap: RwLock::new(DerivativeSnapshot::default()),
                },
            );
        }

        Self {
            registry: DashMap::new(),
            symbol_contexts,
            global_btc_price: AtomicU64::new(f64::NAN.to_bits()),
        }
    }

    #[inline]
    fn get_global_btc(&self) -> Option<f64> {
        let val = f64::from_bits(self.global_btc_price.load(Ordering::Relaxed));
        if val.is_nan() {
            None
        } else {
            Some(val)
        }
    }

    /// 【写优化】实时数据更新：仅做数值累加和闭合判断，不触发 Peek 计算
    pub fn update_realtime_m1(&self, candle: Candle) {
        let symbol = candle.symbol;
        if symbol.is_btc() {
            self.global_btc_price
                .store(candle.close.to_bits(), Ordering::Relaxed);
        }

        // 更新快照价格
        self.update_price_from_m1(symbol, candle.close, candle.timestamp);

        let symbol_ctx = match self.symbol_contexts.get(&symbol) {
            Some(ctx) => ctx,
            None => return,
        };

        let mut roles_guard = symbol_ctx.roles.write().expect("Lock poisoned");
        let g_close = self.get_global_btc();

        for (_role, proc) in roles_guard.iter_mut() {
            if let Some(closed_bar) = proc.process_m1(&candle) {
                if closed_bar.timestamp > proc.last_processed_ts {
                    proc.calculator.next(&closed_bar, proc.interval, g_close);
                    proc.last_processed_ts = closed_bar.timestamp;
                    proc.cached_role_data = None;
                }
            }
        }
    }

    pub fn get_market_context(&self, symbol: Symbol) -> Option<MarketContext> {
        let symbol_ctx = self.symbol_contexts.get(&symbol)?;
        let g_close = self.get_global_btc();

        let snap = symbol_ctx.latest_snap.read().ok()?.clone();
        let mut roles_guard = symbol_ctx.roles.write().ok()?;
        let mut current_roles_data = HashMap::new();

        for (role, proc) in roles_guard.iter_mut() {
            if proc.is_dirty() {
                if let Some(acc) = &proc.current_acc {
                    let feature_set = proc.calculator.peek(acc, proc.interval, g_close);
                    let oi_data = proc.generate_oi_data(
                        snap.last_price,
                        snap.current_oi_amount,
                        snap.current_oi_value,
                    );

                    let new_data = RoleData {
                        interval: proc.interval,
                        feature_set,
                        taker_flow: TakerFlowData::from_candle(acc),
                        oi_data,
                    };

                    proc.last_calc_ts = acc.timestamp;
                    proc.last_calc_volume = acc.volume;
                    proc.cached_role_data = Some(new_data.clone());

                    current_roles_data.insert(*role, new_data);
                }
            } else if let Some(cache) = &proc.cached_role_data {
                current_roles_data.insert(*role, cache.clone());
            }
        }

        let mut mc = MarketContext::new(symbol, Utc::now());
        mc.global = snap;
        mc.roles = current_roles_data;

        // 同步到 Registry
        self.registry.insert(symbol, mc.clone());
        Some(mc)
    }

    pub fn warmup_symbols(
        &self,
        history_map: HashMap<Symbol, HashMap<Interval, Vec<Candle>>>,
        history_oi_map: &HashMap<Symbol, HashMap<Interval, Vec<OpenInterestRecord>>>,
    ) {
        info!(
            "🚀 Starting parallel warmup for {} symbols...",
            history_map.len()
        );
        history_map
            .into_par_iter()
            .for_each(|(symbol, interval_data)| {
                let oi_data = history_oi_map.get(&symbol);
                self.warmup_single_symbol(symbol, &interval_data, oi_data);
            });
        info!("✨ All symbols warmed up.");
    }

    pub fn warmup_single_symbol(
        &self,
        symbol: Symbol,
        interval_data_map: &HashMap<Interval, Vec<Candle>>,
        oi_data_map: Option<&HashMap<Interval, Vec<OpenInterestRecord>>>,
    ) {
        let g_close = self.get_global_btc();
        let symbol_ctx = match self.symbol_contexts.get(&symbol) {
            Some(ctx) => ctx,
            None => return,
        };

        let mut roles_guard = symbol_ctx.roles.write().expect("Lock poisoned");

        for (_, proc) in roles_guard.iter_mut() {
            // 1. 处理种子数据
            if let Some(seeds) = interval_data_map.get(&proc.interval) {
                let oi_lookup: HashMap<i64, &OpenInterestRecord> = oi_data_map
                    .and_then(|m| m.get(&proc.interval))
                    .map(|recs| recs.iter().map(|r| (r.timestamp, r)).collect())
                    .unwrap_or_default();

                for candle in seeds {
                    proc.calculator.next(candle, proc.interval, g_close);
                    if let Some(rec) = oi_lookup.get(&candle.timestamp) {
                        if proc.oi_history.len() >= proc.max_history_size {
                            proc.oi_history.pop_front();
                        }
                        proc.oi_history.push_back(rec.sum_open_interest);
                    }
                }
                if let Some(last_candle) = seeds.last() {
                    proc.last_processed_ts = last_candle.timestamp;
                    proc.current_acc = Some(*last_candle);
                }
            }

            if let Some(m1_candles) = interval_data_map.get(&Interval::M1) {
                if let Some(last_m1) = m1_candles.last() {
                    self.update_price_from_m1(symbol, last_m1.close, last_m1.timestamp);
                }
                for m1 in m1_candles {
                    if m1.timestamp > proc.last_processed_ts {
                        if let Some(closed_bar) = proc.process_m1(m1) {
                            proc.calculator.next(&closed_bar, proc.interval, g_close);
                            proc.last_processed_ts = closed_bar.timestamp;
                        }
                    }
                }
            }

            if let Some(acc) = &proc.current_acc {
                let feature_set = proc.calculator.peek(acc, proc.interval, g_close);
                proc.cached_role_data = Some(RoleData {
                    interval: proc.interval,
                    feature_set,
                    taker_flow: TakerFlowData::from_candle(acc),
                    oi_data: None,
                });
                proc.last_calc_ts = acc.timestamp;
                proc.last_calc_volume = acc.volume;
            }
        }
    }

    pub fn update_oi_from_poller(&self, symbol: Symbol, amount: f64, ts: i64) {
        if let Some(symbol_ctx) = self.symbol_contexts.get(&symbol) {
            let mut lock = symbol_ctx.latest_snap.write().expect("Lock poisoned");

            lock.current_oi_amount = amount;
            if lock.last_price > 0.0 {
                lock.current_oi_value = amount * lock.last_price;
            }
            lock.timestamp = ts;
        }
    }

    pub fn update_price_from_m1(&self, symbol: Symbol, price: f64, ts: i64) {
        if let Some(symbol_ctx) = self.symbol_contexts.get(&symbol) {
            let mut lock = symbol_ctx.latest_snap.write().expect("Lock poisoned");

            lock.last_price = price;
            lock.timestamp = ts;
            if lock.current_oi_amount > 0.0 {
                lock.current_oi_value = lock.current_oi_amount * price;
            }
        }
    }

    pub fn update_symbol_config(
        &self,
        symbol: Symbol,
        config: HashMap<Role, Interval>,
    ) -> Vec<(Role, Interval)> {
        let mut updated_roles = Vec::new();
        if let Some(ctx) = self.symbol_contexts.get(&symbol) {
            let mut roles_guard = ctx.roles.write().expect("Lock poisoned");
            for (role, new_interval) in config {
                let entry = roles_guard
                    .entry(role)
                    .or_insert_with(|| RoleProcessor::new(new_interval));
                if entry.interval != new_interval {
                    updated_roles.push((role, new_interval));
                    *entry = RoleProcessor::new(new_interval);
                }
            }
        }
        updated_roles
    }
}
