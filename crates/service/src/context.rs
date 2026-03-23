use chrono::Utc;
use common::{
    config::{Appconfig, RoleConfig},
    Candle, Interval, Symbol,
};
use dashmap::DashMap;
use quant::{
    analyzer::{MarketContext, Role, RoleData},
    calculator::FeatureCalculator,
};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

pub struct RoleProcessor {
    pub interval: Interval,
    pub calculator: FeatureCalculator,
    pub current_acc: Option<Candle>,
    pub last_processed_ts: i64,
}

impl RoleProcessor {
    pub fn new(interval: Interval) -> Self {
        Self {
            interval,
            calculator: FeatureCalculator::new(interval),
            current_acc: None,
            last_processed_ts: 0,
        }
    }

    pub fn sync_historical_anchor(&mut self, candle: &Candle) {
        self.last_processed_ts = candle.timestamp;
        self.current_acc = Some(candle.clone());
    }

    pub fn process_m1(&mut self, m1: &Candle) -> Option<Candle> {
        let interval_ms = self.interval.to_millis();
        if interval_ms == 0 {
            return None;
        }

        let m1_ts = m1.timestamp;
        if let Some(acc) = self.current_acc.as_mut() {
            let current_bucket_idx = m1_ts / interval_ms;
            let acc_bucket_idx = acc.timestamp / interval_ms;

            if current_bucket_idx > acc_bucket_idx {
                let closed_bar = self.current_acc.take().unwrap();
                self.current_acc = Some(self.init_acc(m1));
                return Some(closed_bar);
            }

            acc.high = acc.high.max(m1.high);
            acc.low = acc.low.min(m1.low);
            acc.close = m1.close;
            acc.volume += m1.volume;
            acc.quote_volume += m1.quote_volume;
            acc.trade_count += m1.trade_count;

            None
        } else {
            self.current_acc = Some(self.init_acc(m1));
            None
        }
    }

    pub fn init_acc(&self, m1: &Candle) -> Candle {
        let interval_ms = self.interval.to_millis();
        let aligned_ts = (m1.timestamp / interval_ms) * interval_ms;

        let mut acc = m1.clone();
        acc.timestamp = aligned_ts;
        acc
    }
}

pub struct SymbolContext {
    pub roles: HashMap<Role, RoleProcessor>,
}

impl SymbolContext {
    pub fn new(config: RoleConfig) -> Self {
        let roles: HashMap<Role, RoleProcessor> = [
            (Role::Trend, config.trend),
            (Role::Filter, config.filter),
            (Role::Entry, config.entry),
        ]
        .iter()
        .map(|(role, interval)| (*role, RoleProcessor::new(*interval)))
        .collect();

        Self { roles }
    }
}

pub struct FeatureContextManager {
    pub registry: Arc<DashMap<Symbol, MarketContext>>,
    pub symbol_contexts: Arc<DashMap<Symbol, SymbolContext>>,
    pub global_btc_price: AtomicU64,
}

impl FeatureContextManager {
    pub fn new(symbols: &[Symbol]) -> Self {
        let symbol_contexts = DashMap::new();
        let cfg = Appconfig::global();

        for symbol in symbols {
            symbol_contexts.insert(*symbol, SymbolContext::new(cfg.role));
        }

        Self {
            registry: Arc::new(DashMap::new()),
            symbol_contexts: Arc::new(symbol_contexts),
            global_btc_price: AtomicU64::new(f64::NAN.to_bits()),
        }
    }

    #[inline]
    fn get_global_btc(&self) -> Option<f64> {
        let val = f64::from_bits(self.global_btc_price.load(Ordering::Relaxed));
        (!val.is_nan()).then_some(val)
    }

    pub fn warmup_single_symbol(
        &self,
        symbol: Symbol,
        seeds_map: &HashMap<Interval, Vec<Candle>>,
        m1_candles: &[Candle],
    ) {
        let g_close = self.get_global_btc();
        let mut latest_updates = Vec::new();

        if let Some(mut symbol_ctx) = self.symbol_contexts.get_mut(&symbol) {
            for (role, proc) in symbol_ctx.roles.iter_mut() {
                let mut last_feat = None;
                let mut seed_end_ts = 0;

                if let Some(seeds) = seeds_map.get(&proc.interval) {
                    for candle in seeds {
                        last_feat = Some(proc.calculator.next(candle, proc.interval, g_close));
                        seed_end_ts = candle.timestamp;
                    }

                    if let Some(last_seed) = seeds.last() {
                        proc.sync_historical_anchor(last_seed);
                    }
                }

                for candle in m1_candles {
                    if candle.timestamp > seed_end_ts {
                        if let Some(closed_bar) = proc.process_m1(candle) {
                            last_feat =
                                Some(proc.calculator.next(&closed_bar, proc.interval, g_close));
                            proc.last_processed_ts = closed_bar.timestamp;
                        }
                    }
                }

                if let Some(acc) = proc.current_acc.as_ref() {
                    last_feat = Some(proc.calculator.peek(acc, proc.interval, g_close));
                }

                if let Some(feat) = last_feat {
                    latest_updates.push((
                        *role,
                        RoleData {
                            interval: proc.interval,
                            feature_set: feat,
                        },
                    ));
                }
            }
        }

        if !latest_updates.is_empty() {
            let mut registry_entry = self.registry.entry(symbol).or_default();
            let ctx = registry_entry.value_mut();

            ctx.symbol = symbol;

            if let Some(last_m1) = m1_candles.last() {
                ctx.current_price = last_m1.close;
            }

            ctx.timestamp = Utc::now();

            for (role, data) in latest_updates {
                ctx.roles.insert(role, data);
            }
        }
    }

    pub fn update_symbol_config(
        &self,
        symbol: Symbol,
        config: HashMap<Role, Interval>,
    ) -> Vec<(Role, Interval)> {
        let mut updated_roles = Vec::new();

        self.symbol_contexts.entry(symbol).and_modify(|ctx| {
            for (role, new_interval) in config {
                let is_changed = match ctx.roles.get(&role) {
                    Some(proc) => proc.interval != new_interval,
                    None => true,
                };

                if is_changed {
                    updated_roles.push((role, new_interval));
                    ctx.roles.insert(role, RoleProcessor::new(new_interval));
                }
            }
        });

        updated_roles
    }

    pub fn warmup_symbols(&self, history_map: HashMap<Symbol, HashMap<Interval, Vec<Candle>>>) {
        let btc_map = history_map
            .get(&Symbol::BTCUSDT)
            .and_then(|m| m.get(&Interval::M1))
            .map(|candles| {
                candles
                    .iter()
                    .map(|c| (c.timestamp, c.close))
                    .collect::<HashMap<i64, f64>>()
            })
            .unwrap_or_default();

        let all_results: Vec<(Symbol, MarketContext)> = history_map
            .into_par_iter()
            .filter_map(|(symbol, interval_data_map)| {
                let mut latest_role_results = HashMap::new();

                if let Some(mut symbol_ctx) = self.symbol_contexts.get_mut(&symbol) {
                    for (role, proc) in symbol_ctx.roles.iter_mut() {
                        let mut last_feat = None;
                        let mut seed_last_ts = 0;

                        // 2. 处理种子数据 (Seeds)
                        if let Some(seeds) = interval_data_map.get(&proc.interval) {
                            for candle in seeds {
                                let sync_btc = btc_map.get(&candle.timestamp).cloned();
                                last_feat =
                                    Some(proc.calculator.next(candle, proc.interval, sync_btc));
                                seed_last_ts = candle.timestamp;
                            }
                            if let Some(last_seed) = seeds.last() {
                                proc.sync_historical_anchor(last_seed);
                            }
                        }

                        if let Some(m1_history) = interval_data_map.get(&Interval::M1) {
                            for m1 in m1_history {
                                if m1.timestamp > seed_last_ts {
                                    if let Some(closed_bar) = proc.process_m1(m1) {
                                        // ⭐ 修正：闭合时，使用闭合 K 线的时间戳匹配 BTC
                                        let sync_btc = btc_map.get(&closed_bar.timestamp).cloned();
                                        last_feat = Some(proc.calculator.next(
                                            &closed_bar,
                                            proc.interval,
                                            sync_btc,
                                        ));
                                        proc.last_processed_ts = closed_bar.timestamp;
                                    }
                                }
                            }
                        }

                        // 4. 处理未闭合的“偷窥”数据 (Peek)
                        if let Some(acc) = proc.current_acc.as_ref() {
                            // ⭐ 修正：Peek 时通常使用最新的 BTC 价格
                            let latest_btc = btc_map.get(&acc.timestamp).cloned();
                            last_feat = Some(proc.calculator.peek(acc, proc.interval, latest_btc));
                        }

                        if let Some(feat) = last_feat {
                            latest_role_results.insert(
                                *role,
                                RoleData {
                                    interval: proc.interval,
                                    feature_set: feat,
                                },
                            );
                        }
                    }
                }

                // ... 后续构建 MarketContext 的逻辑保持不变 ...
                if !latest_role_results.is_empty() {
                    let current_price = interval_data_map
                        .get(&Interval::M1)
                        .and_then(|m1s| m1s.last())
                        .map(|c| c.close)
                        .unwrap_or(0.0);

                    let ctx = MarketContext {
                        symbol,
                        current_price,
                        timestamp: Utc::now(),
                        roles: latest_role_results,
                        ..Default::default()
                    };
                    Some((symbol, ctx))
                } else {
                    None
                }
            })
            .collect();

        for (symbol, ctx) in all_results {
            self.registry.insert(symbol, ctx);
        }
    }

    pub fn update_realtime_m1(&self, candle: Candle) {
        let symbol = &candle.symbol;

        if symbol.is_btc() {
            self.global_btc_price
                .store(candle.close.to_bits(), Ordering::Relaxed);
        }
        let g_close = self.get_global_btc();
        let mut updates = Vec::new();

        if let Some(mut symbol_ctx) = self.symbol_contexts.get_mut(symbol) {
            for (role, proc) in symbol_ctx.roles.iter_mut() {
                // ⭐ A分支：K线闭合了，产生永久特征更新
                if let Some(closed_bar) = proc.process_m1(&candle) {
                    if closed_bar.timestamp > proc.last_processed_ts {
                        let feat = proc.calculator.next(&closed_bar, proc.interval, g_close);
                        proc.last_processed_ts = closed_bar.timestamp;

                        updates.push((
                            *role,
                            RoleData {
                                interval: proc.interval,
                                feature_set: feat,
                            },
                        ));
                    }
                } else if let Some(acc) = proc.current_acc.as_ref() {
                    let feat = proc.calculator.peek(acc, proc.interval, g_close);
                    updates.push((
                        *role,
                        RoleData {
                            interval: proc.interval,
                            feature_set: feat,
                        },
                    ));
                }
            }
        }

        if !updates.is_empty() {
            let mut registry_entry = self.registry.entry(*symbol).or_default();
            let ctx = registry_entry.value_mut();

            ctx.symbol = *symbol;
            ctx.current_price = candle.close;
            ctx.timestamp = Utc::now();

            for (role, data) in updates {
                ctx.roles.insert(role, data);
            }
        }
    }
}
