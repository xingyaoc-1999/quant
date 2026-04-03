use chrono::Utc;
use common::{config::Appconfig, Candle, Interval, OpenInterestRecord, Symbol};
use dashmap::DashMap;
use quant::{
    analyzer::{MarketContext, OIData, Role, RoleData, TakerFlowData},
    calculator::FeatureCalculator,
    types::DerivativeSnapshot,
};
use rayon::prelude::*;
use std::collections::{HashMap, VecDeque};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    RwLock,
};
use tracing::info;

// ==================== 1. 角色处理器 (RoleProcessor) ====================

pub struct RoleProcessor {
    pub interval: Interval,
    pub calculator: FeatureCalculator,
    pub current_acc: Option<Candle>,
    pub last_processed_ts: i64,
    pub last_anchor_snap: Option<DerivativeSnapshot>,
    pub oi_history: VecDeque<f64>,
    pub max_history_size: usize,
}

impl RoleProcessor {
    pub fn new(interval: Interval) -> Self {
        // 根据角色周期定义记忆深度
        let max_history_size = match interval {
            Interval::D1 => 31,   // 存 1 个月的天级数据
            Interval::H4 => 61,   // 存 10 天的 4h 数据
            Interval::M15 => 121, // 存 30 小时的 15m 数据
            _ => 61,
        };

        Self {
            interval,
            calculator: FeatureCalculator::new(interval),
            current_acc: None,
            last_processed_ts: 0,
            last_anchor_snap: None,
            oi_history: VecDeque::with_capacity(max_history_size),
            max_history_size,
        }
    }

    pub fn get_volume_projection(&self) -> f64 {
        let acc = match &self.current_acc {
            Some(a) => a,
            None => return 0.0,
        };

        let interval_ms = self.interval.to_millis();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let elapsed_ms = (now_ms - acc.timestamp).max(1);

        let progress = (elapsed_ms as f64 / interval_ms as f64).min(1.0);

        acc.volume / progress
    }

    fn calculate_oi_metrics(&self, current_oi: f64) -> Vec<f64> {
        let mut history_changes = Vec::new();
        let steps = [1, 3, 7, 14];
        let len = self.oi_history.len();

        if len == 0 {
            return vec![0.0; 4];
        }

        let is_already_pushed = (self.oi_history[len - 1] - current_oi).abs() < f64::EPSILON;
        let offset = if is_already_pushed { 1 } else { 0 };

        for &step in &steps {
            let target_idx = len.checked_sub(step + offset);

            if let Some(idx) = target_idx {
                let past_val = self.oi_history[idx];
                if past_val > 0.0 {
                    history_changes.push((current_oi - past_val) / past_val);
                } else {
                    history_changes.push(0.0);
                }
            } else {
                history_changes.push(0.0);
            }
        }
        history_changes
    }
    pub fn generate_oi_data(&self, current: Option<&DerivativeSnapshot>) -> Option<OIData> {
        let last = self.last_anchor_snap.as_ref()?;
        let current = current?;

        if current.last_price <= f64::EPSILON || current.current_oi_amount <= f64::EPSILON {
            return None;
        }

        if last.current_oi_amount <= f64::EPSILON || last.last_price <= f64::EPSILON {
            return None;
        }

        let oi_change_pct =
            (current.current_oi_amount - last.current_oi_amount) / last.current_oi_amount;

        let price_change_pct = (current.last_price - last.last_price) / last.last_price;

        if oi_change_pct.is_finite() && price_change_pct.is_finite() {
            let change_history = self.calculate_oi_metrics(current.current_oi_amount);

            let oi_data = OIData::new(
                current.current_oi_amount,
                current.current_oi_value,
                change_history,
            );

            Some(oi_data)
        } else {
            None
        }
    }

    pub fn peek_role_data(
        &self,
        g_close: Option<f64>,
        current_snap: Option<&DerivativeSnapshot>,
    ) -> Option<RoleData> {
        let acc = self.current_acc.as_ref()?;
        let feature_set = self.calculator.peek(acc, self.interval, g_close);

        Some(RoleData {
            interval: self.interval,
            feature_set,
            taker_flow: TakerFlowData::from_candle(acc),
            oi_data: self.generate_oi_data(current_snap),
        })
    }

    pub fn process_m1(&mut self, m1: &Candle) -> Option<Candle> {
        let interval_ms = self.interval.to_millis();
        if interval_ms == 0 {
            return None;
        }

        let bucket_ts = (m1.timestamp / interval_ms) * interval_ms;

        if let Some(ref mut acc) = self.current_acc {
            if bucket_ts > acc.timestamp {
                let closed_bar = self.current_acc.replace(Candle {
                    timestamp: bucket_ts,
                    ..*m1
                });
                return closed_bar;
            }

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
}

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

    pub fn update_realtime_m1(&self, candle: Candle) {
        let symbol = candle.symbol;
        if symbol.is_btc() {
            self.global_btc_price
                .store(candle.close.to_bits(), Ordering::Relaxed);
        }
        let g_close = self.get_global_btc();
        self.update_price_from_m1(symbol, candle.close, candle.timestamp);
        let symbol_ctx = match self.symbol_contexts.get(&symbol) {
            Some(ctx) => ctx,
            None => return,
        };

        let current_snap = symbol_ctx.latest_snap.read().expect("Lock poisoned");
        let mut roles_guard = symbol_ctx.roles.write().expect("Lock poisoned");
        let mut role_updates = Vec::with_capacity(roles_guard.len());

        for (role, proc) in roles_guard.iter_mut() {
            if let Some(closed_bar) = proc.process_m1(&candle) {
                if closed_bar.timestamp > proc.last_processed_ts {
                    proc.last_processed_ts = closed_bar.timestamp;

                    let oi_data = proc.generate_oi_data(Some(&*current_snap));

                    if proc.oi_history.len() >= proc.max_history_size {
                        proc.oi_history.pop_front();
                    }
                    proc.oi_history.push_back(current_snap.current_oi_amount);

                    let feature_set = proc.calculator.next(&closed_bar, proc.interval, g_close);
                    proc.last_anchor_snap = Some(*current_snap);

                    role_updates.push((
                        *role,
                        RoleData {
                            interval: proc.interval,
                            feature_set,
                            taker_flow: TakerFlowData::from_candle(&closed_bar),
                            oi_data,
                        },
                    ));
                }
            } else if let Some(role_data) = proc.peek_role_data(g_close, Some(&*current_snap)) {
                role_updates.push((*role, role_data));
            }
        }

        if !role_updates.is_empty() {
            self.registry
                .entry(symbol)
                .and_modify(|ctx| {
                    ctx.timestamp = Utc::now();

                    ctx.global = *current_snap;
                    ctx.roles.extend(role_updates.clone());
                })
                .or_insert_with(|| {
                    let mut ctx = MarketContext::new(symbol, Utc::now());
                    ctx.global = *current_snap;
                    ctx.roles.extend(role_updates);
                    ctx
                });
        }
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
                // 获取该币对对应的 OI 历史数据
                let oi_data = history_oi_map.get(&symbol);

                self.warmup_single_symbol(symbol, &interval_data, oi_data);
            });

        info!("✨ All symbols warmed up and Registry is ready.");
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
        let mut latest_role_results = HashMap::new();

        for (role, proc) in roles_guard.iter_mut() {
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

                        proc.last_anchor_snap = Some(DerivativeSnapshot {
                            last_price: candle.close,
                            current_oi_amount: rec.sum_open_interest,
                            current_oi_value: rec.sum_open_interest_value,
                            timestamp: rec.timestamp,
                        });
                    }
                }

                if let Some(last_candle) = seeds.last() {
                    proc.last_processed_ts = last_candle.timestamp;
                    proc.current_acc = Some(*last_candle);
                }
            }

            if let Some(m1_candles) = interval_data_map.get(&Interval::M1) {
                for m1 in m1_candles {
                    if m1.timestamp > proc.last_processed_ts {
                        if let Some(closed_bar) = proc.process_m1(m1) {
                            proc.calculator.next(&closed_bar, proc.interval, g_close);
                            proc.last_processed_ts = closed_bar.timestamp;
                        }
                    }
                }
            }

            if let Some(data) = proc.peek_role_data(g_close, proc.last_anchor_snap.as_ref()) {
                latest_role_results.insert(*role, data);
            }
        }

        // --- 4. 更新 Registry 注册表 ---
        if !latest_role_results.is_empty() {
            self.registry
                .entry(symbol)
                .and_modify(|ctx| {
                    ctx.timestamp = Utc::now();

                    ctx.roles.extend(latest_role_results.clone());
                })
                .or_insert_with(|| {
                    let mut ctx = MarketContext::new(symbol, Utc::now());
                    ctx.roles.extend(latest_role_results);
                    ctx
                });
        }
    }
    pub fn update_oi_from_poller(&self, symbol: Symbol, amount: f64, ts: i64) {
        if let Some(symbol_ctx) = self.symbol_contexts.get(&symbol) {
            let mut lock = symbol_ctx.latest_snap.write().expect("Lock poisoned");

            // 更新持仓量
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
            // 如果已有 OI 数量，同步更新价值
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
