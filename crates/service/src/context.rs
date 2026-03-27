use common::{
    config::{Appconfig, RoleConfig},
    Candle, Interval, Symbol,
};
use dashmap::DashMap;
use quant::{
    analyzer::{MarketContext, Role, RoleData, TakerFlowData},
    calculator::FeatureCalculator,
    types::DerivativeSnapshot,
};
use rayon::prelude::*;
use std::collections::{HashMap, VecDeque};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    RwLock,
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

    pub fn current_taker_flow(&self) -> TakerFlowData {
        if let Some(acc) = &self.current_acc {
            TakerFlowData::from_candle(acc)
        } else {
            TakerFlowData::default()
        }
    }

    pub fn current_role_data(&self, g_close: Option<f64>) -> Option<RoleData> {
        let acc = self.current_acc.as_ref()?;
        let feature_set = self.calculator.peek(acc, self.interval, g_close);
        let taker_flow = TakerFlowData::from_candle(acc);
        Some(RoleData {
            interval: self.interval,
            feature_set,
            taker_flow,
        })
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

        let acc = self.current_acc.get_or_insert_with(|| Candle {
            timestamp: (m1.timestamp / interval_ms) * interval_ms,
            ..m1.clone()
        });

        let current_bucket = m1.timestamp / interval_ms;
        let acc_bucket = acc.timestamp / interval_ms;

        if current_bucket > acc_bucket {
            return self.current_acc.replace(Candle {
                timestamp: (m1.timestamp / interval_ms) * interval_ms,
                ..m1.clone()
            });
        }

        acc.high = acc.high.max(m1.high);
        acc.low = acc.low.min(m1.low);
        acc.close = m1.close;
        acc.volume += m1.volume;
        acc.quote_volume += m1.quote_volume;
        acc.trade_count += m1.trade_count;

        None
    }
}

// ==================== 符号上下文 ====================

pub struct SymbolContext {
    // 增加内部读写锁，使高频获取只需 DashMap 的读锁
    pub roles: RwLock<HashMap<Role, RoleProcessor>>,
    pub latest_snap: RwLock<DerivativeSnapshot>,
}

impl SymbolContext {
    pub fn new(config: RoleConfig) -> Self {
        Self {
            roles: RwLock::new(HashMap::from([
                (Role::Trend, RoleProcessor::new(config.trend)),
                (Role::Filter, RoleProcessor::new(config.filter)),
                (Role::Entry, RoleProcessor::new(config.entry)),
            ])),
            latest_snap: RwLock::new(DerivativeSnapshot::default()),
        }
    }
}

// ==================== 全局特征上下文管理器 ====================

pub struct FeatureContextManager {
    // 移除冗余的 Arc（如果外层本身会共享该 Manager）
    pub registry: DashMap<Symbol, MarketContext>,
    pub symbol_contexts: DashMap<Symbol, SymbolContext>,
    pub global_btc_price: AtomicU64,
}

impl FeatureContextManager {
    pub fn new(symbols: &[Symbol]) -> Self {
        let symbol_contexts = DashMap::new();
        let cfg = Appconfig::global();

        for &symbol in symbols {
            symbol_contexts.insert(symbol, SymbolContext::new(cfg.role));
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
        (!val.is_nan()).then_some(val)
    }

    pub fn update_realtime_m1(&self, candle: Candle) {
        let symbol = candle.symbol;

        if symbol.is_btc() {
            self.global_btc_price
                .store(candle.close.to_bits(), Ordering::Relaxed);
        }
        let g_close = self.get_global_btc();

        // 优化点 1: 改为只读获取，避免分片级别的锁阻塞
        let symbol_ctx = match self.symbol_contexts.get(&symbol) {
            Some(ctx) => ctx,
            None => return,
        };

        // 2. 更新各角色的特征
        let mut roles_guard = symbol_ctx.roles.write().expect("Lock poisoned");
        // 预分配内存避免动态扩容
        let mut role_updates = Vec::with_capacity(roles_guard.len());

        for (role, proc) in roles_guard.iter_mut() {
            if let Some(closed_bar) = proc.process_m1(&candle) {
                if closed_bar.timestamp > proc.last_processed_ts {
                    proc.last_processed_ts = closed_bar.timestamp;
                    let feature_set = proc.calculator.next(&closed_bar, proc.interval, g_close);
                    let taker_flow = TakerFlowData::from_candle(&closed_bar);
                    role_updates.push((
                        *role,
                        RoleData {
                            interval: proc.interval,
                            feature_set,
                            taker_flow,
                        },
                    ));
                }
            } else if let Some(role_data) = proc.current_role_data(g_close) {
                role_updates.push((*role, role_data));
            }
        }
        drop(roles_guard); // 尽早释放锁

        // 3. 将更新写入 registry
        if !role_updates.is_empty() {
            let mut ctx = self.registry.entry(symbol).or_default();
            ctx.current_price = candle.close;
            ctx.timestamp = chrono::Utc::now();

            ctx.roles.extend(role_updates);
        }
    }

    pub fn update_derivative_snap(&self, symbol: &Symbol, snap: DerivativeSnapshot) {
        if let Some(ctx) = self.symbol_contexts.get(symbol) {
            let mut writer = ctx.latest_snap.write().expect("Lock poisoned");
            *writer = snap;
        }
    }

    pub fn warmup_single_symbol(
        &self,
        symbol: Symbol,
        interval_data_map: &HashMap<Interval, Vec<Candle>>,
    ) {
        let g_close = self.get_global_btc();

        let m1_candles = interval_data_map
            .get(&Interval::M1)
            .map(Vec::as_slice)
            .unwrap_or(&[]);

        // 只读获取 Context
        let symbol_ctx = match self.symbol_contexts.get(&symbol) {
            Some(ctx) => ctx,
            None => return,
        };

        // 初始化各角色的特征
        let mut latest_role_results = HashMap::new();
        let mut roles_guard = symbol_ctx.roles.write().expect("Lock poisoned");

        for (role, proc) in roles_guard.iter_mut() {
            let mut last_feat = None;
            let mut seed_last_ts = 0;

            if let Some(seeds) = interval_data_map.get(&proc.interval) {
                for candle in seeds {
                    last_feat = Some(proc.calculator.next(candle, proc.interval, g_close));
                }
                if let Some(last_seed) = seeds.last() {
                    proc.sync_historical_anchor(last_seed);
                    seed_last_ts = last_seed.timestamp;
                }
            }

            for m1 in m1_candles {
                if m1.timestamp > seed_last_ts {
                    if let Some(closed_bar) = proc.process_m1(m1) {
                        if closed_bar.timestamp > proc.last_processed_ts {
                            last_feat =
                                Some(proc.calculator.next(&closed_bar, proc.interval, g_close));
                            proc.last_processed_ts = closed_bar.timestamp;
                        }
                    }
                }
            }

            if let Some(acc) = proc.current_acc.as_ref() {
                last_feat = Some(proc.calculator.peek(acc, proc.interval, g_close));
            }

            if let Some(feat) = last_feat {
                latest_role_results.insert(
                    *role,
                    RoleData {
                        interval: proc.interval,
                        feature_set: feat,
                        taker_flow: proc.current_taker_flow(),
                    },
                );
            }
        }
        drop(roles_guard);

        if !latest_role_results.is_empty() {
            let current_price = m1_candles.last().map(|c| c.close).unwrap_or(0.0);

            let mut ctx = self.registry.entry(symbol).or_default();
            ctx.current_price = current_price;
            ctx.timestamp = chrono::Utc::now();

            ctx.roles.extend(latest_role_results);
        }
    }

    pub fn warmup_symbols(&self, history_map: HashMap<Symbol, HashMap<Interval, Vec<Candle>>>) {
        history_map
            .into_par_iter()
            .for_each(|(symbol, interval_data)| {
                self.warmup_single_symbol(symbol, &interval_data);
            });
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
