use crate::context::FeatureContextManager;
use anyhow::Result;
use api_client::{
    http::binance::ArchiveProvider,
    websocket::{biance::BinanceKlineProtocol, GenericWsClient},
};
use futures_util::stream::{self, StreamExt};

use chrono::{DateTime, DurationRound, Utc};
use common::{
    config::Appconfig, utils::CooledProxyPool, Candle, Interval, OpenInterestRecord, Symbol,
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use storage::postgres::Storage;
use tokio::{sync::mpsc, time::interval};
use tracing::{error, info, warn};

struct PreWarmData {
    // Symbol -> (Interval -> Vec<Candle>)
    history_map: HashMap<Symbol, HashMap<Interval, Vec<Candle>>>,

    pub history_oi_map: HashMap<Symbol, HashMap<Interval, Vec<OpenInterestRecord>>>,
}

pub struct DataIntegrityManager {
    symbols: Vec<Symbol>,
    feature_context: Arc<FeatureContextManager>,
    proxy_pool: Arc<CooledProxyPool>,
    storage: Arc<Storage>,
    archive_provider: Arc<ArchiveProvider>,
}

impl DataIntegrityManager {
    pub fn new(
        symbols: Vec<Symbol>,
        feature_context: Arc<FeatureContextManager>,
        proxy_pool: Arc<CooledProxyPool>,
        storage: Arc<Storage>,
        archive_provider: Arc<ArchiveProvider>,
    ) -> Self {
        Self {
            symbols,
            storage,
            feature_context,
            proxy_pool,
            archive_provider,
        }
    }

    pub fn start(self: Arc<Self>) {
        info!("🚀 [Manager] Data Integrity Manager is initializing...");
        let manager = Arc::clone(&self);

        tokio::spawn(async move {
            let end_gap = Utc::now();
            let start_gap = end_gap - chrono::TimeDelta::days(30);

            info!("📥 [Initial-Sync] Filling gaps for last 30 days...");
            if let Err(e) = manager.fill_all_gaps(start_gap, end_gap).await {
                error!("❌ [Initial-Sync] Failed: {:?}", e);
            }

            info!("🔄 [Initial-Refresh] Refreshing views...");
            let _ = manager
                .storage
                .refresh_all_chunked(start_gap, end_gap, chrono::TimeDelta::days(30))
                .await;
            manager.clone().start_oi_poller();

            if let Err(e) = manager.execute_cold_start().await {
                error!("❌ [Cold Start] Warmup failed: {:?}", e);
            }

            manager.clone().start_realtime_engine();

            manager.clone().start_runtime_checker();

            info!("✅ [Manager] System is live and background tasks are scheduled.");
        });
    }
    async fn get_multi_history(&self) -> Result<PreWarmData> {
        let config = Appconfig::global().role;
        let candle_intervals = vec![config.trend, config.filter, config.entry, Interval::M1];
        let oi_intervals = vec![config.trend, config.filter, config.entry];

        // --- PART A: 获取 OHLCV (1m 为主) ---
        let mut history_map: HashMap<Symbol, HashMap<Interval, Vec<Candle>>> = HashMap::new();
        let mut candle_stream = stream::iter(candle_intervals)
            .map(|interval| {
                let symbols = self.symbols.clone();
                let storage = self.storage.clone();
                async move {
                    let limit = if interval == Interval::M1 { 1000 } else { 200 };
                    let res = storage.get_batch(interval, &symbols, limit).await;
                    (interval, res)
                }
            })
            .buffer_unordered(4);

        while let Some((interval, res)) = candle_stream.next().await {
            let batch = res?;
            for (sym, candles) in batch {
                history_map
                    .entry(sym)
                    .or_default()
                    .insert(interval, candles);
            }
        }

        let mut oi_tasks = Vec::new();
        for &symbol in &self.symbols {
            for &interval in &oi_intervals {
                oi_tasks.push((symbol, interval));
            }
        }

        let mut oi_history_map: HashMap<Symbol, HashMap<Interval, Vec<OpenInterestRecord>>> =
            HashMap::new();
        let mut oi_stream = stream::iter(oi_tasks)
            .map(|(symbol, interval)| {
                let provider = self.archive_provider.clone();
                async move {
                    let res = provider.fetch_open_interest_hist(symbol, interval).await;

                    (symbol, interval, res)
                }
            })
            .buffer_unordered(10);

        while let Some((symbol, interval, res)) = oi_stream.next().await {
            match res {
                Ok(records) => {
                    oi_history_map
                        .entry(symbol)
                        .or_default()
                        .insert(interval, records);
                }
                Err(e) => warn!("⚠️ [OI-Fetch] {} {:?} failed: {}", symbol, interval, e),
            }
        }

        Ok(PreWarmData {
            history_map,
            history_oi_map: oi_history_map,
        })
    }
    pub async fn execute_cold_start(&self) -> Result<()> {
        info!(
            "📂 [Cold Start] Pre-warming {} symbols...",
            self.symbols.len()
        );

        let pre_warm_data = self.get_multi_history().await?;

        let ctx = Arc::clone(&self.feature_context);

        tokio::task::spawn_blocking(move || {
            ctx.warmup_symbols(pre_warm_data.history_map, &pre_warm_data.history_oi_map);
            info!("✅ [Warmup] All symbols synchronized and calculated.");
        })
        .await?;

        Ok(())
    }

    fn start_realtime_engine(self: Arc<Self>) {
        let manager = Arc::clone(&self);
        let pp = Arc::clone(&self.proxy_pool);
        let storage = Arc::clone(&self.storage);
        let symbols = self.symbols.clone();

        tokio::spawn(async move {
            let (tx, mut rx) = mpsc::channel::<Candle>(2000);

            let protocol = BinanceKlineProtocol::new(Interval::M1);
            let client = GenericWsClient::new(protocol, pp, symbols.into_iter().collect());

            info!("📡 [Realtime] Launching WebSocket connectivity engine...");
            tokio::spawn(async move {
                if let Err(e) = client.run(tx).await {
                    error!("❌ [WS Client] Runtime error: {:?}", e);
                }
            });

            let mut buffer = Vec::with_capacity(100);
            let mut flush_interval = interval(Duration::from_secs(5));
            flush_interval.tick().await;

            loop {
                tokio::select! {
                    biased;
                    Some(candle) = rx.recv() => {
                        manager.feature_context.update_realtime_m1(candle);

                        buffer.push(candle);
                        if buffer.len() >= 50 {
                            let s = Arc::clone(&storage);
                            let batch = std::mem::take(&mut buffer);
                            tokio::spawn(async move {
                                if let Err(e) = s.insert_candles(&batch).await {
                                    error!("❌ [Storage] Batch insert failed: {:?}", e);
                                }
                            });
                        }
                    }

                    _ = flush_interval.tick() => {
                        if !buffer.is_empty() {
                            let s = Arc::clone(&storage);
                            let batch = std::mem::take(&mut buffer);
                            tokio::spawn(async move {
                                if let Err(e) = s.insert_candles(&batch).await {
                                    error!("❌ [Storage] Flush insert failed: {:?}", e);
                                }
                            });
                        }
                    }

                    else => {
                        warn!("⚠️ [Realtime] Receiver channel closed.");
                        break;
                    }
                }
            }
        });
    }

    async fn fill_all_gaps(&self, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<()> {
        let total_symbols = self.symbols.len();
        let completed_count = Arc::new(AtomicUsize::new(0));

        info!(
            "🛠️ [GapFill] Starting parallel gap fill for {} symbols | Range: {} to {}",
            total_symbols,
            start.format("%Y-%m-%d"),
            end.format("%Y-%m-%d")
        );

        stream::iter(self.symbols.clone())
            .map(|symbol| {
                let manager = self;
                let counter = Arc::clone(&completed_count);

                async move {
                    let res = manager.sync_single_symbol(&symbol, start, end).await;

                    let done = counter.fetch_add(1, Ordering::SeqCst) + 1;
                    let percent = (done as f64 / total_symbols as f64) * 100.0;

                    info!(
                        "📊 [Progress] {:.1}% ({}/{}) | Finished: {}",
                        percent, done, total_symbols, symbol
                    );
                    res
                }
            })
            .buffer_unordered(5)
            .for_each(|res| async {
                if let Err(e) = res {
                    error!("❌ [GapFill] A symbol sync task failed: {:?}", e);
                }
            })
            .await;

        info!("✨ [GapFill] All symbols synchronization completed.");
        Ok(())
    }
    async fn sync_single_symbol(
        &self,
        symbol: &Symbol,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<()> {
        let gap_days = self.storage.get_incomplete_days(symbol, start, end).await?;

        if gap_days.is_empty() {
            info!("✅ [Integrity] {} has no historical gaps.", symbol);
        } else {
            info!(
                "🧩 [Gap Fill] {} has {} days to repair.",
                symbol,
                gap_days.len()
            );

            stream::iter(gap_days)
                .map(|day| async move {
                    let date_str = day.format("%Y-%m-%d").to_string();

                    match self
                        .archive_provider
                        .download_archive_candles(symbol, &date_str)
                        .await
                    {
                        Ok(candles) if !candles.is_empty() => {
                            self.storage.insert_candles(&candles).await?;
                            info!("✅ [CSV] Filled {} for {}", date_str, symbol);
                        }
                        _ => {
                            warn!(
                                "⚠️ [CSV] Not found for {} on {}, failing over to REST...",
                                symbol, date_str
                            );
                            self.fill_day_via_rest(symbol, day).await?;
                        }
                    }
                    Ok::<(), anyhow::Error>(())
                })
                .buffer_unordered(3)
                .for_each(|res| async {
                    if let Err(e) = res {
                        error!("❌ [Gap Fill] Error processing {} day: {:?}", symbol, e);
                    }
                })
                .await;
        }

        self.catch_up_to_now(symbol).await?;

        Ok(())
    }
    async fn catch_up_to_now(&self, symbol: &Symbol) -> Result<()> {
        let latest_ts = self
            .storage
            .get_latest_candles(symbol, Interval::M1, 1)
            .await?
            .first()
            .map(|c| c.timestamp + 60000)
            .unwrap_or_else(|| Utc::now().timestamp_millis() - 3_600_000);
        let now_ms = Utc::now().timestamp_millis();

        let count = self
            .fetch_and_store_range(symbol, latest_ts, now_ms)
            .await?;

        if count > 0 {
            info!("🚀 [Catch-up] {} synced {} candles to now.", symbol, count);
        }
        Ok(())
    }
    async fn fill_day_via_rest(&self, symbol: &Symbol, day: DateTime<Utc>) -> Result<()> {
        let start_ms = day.timestamp_millis();
        let end_ms = start_ms + 24 * 60 * 60 * 1000 - 1;

        let count = self.fetch_and_store_range(symbol, start_ms, end_ms).await?;

        if count > 0 {
            info!(
                "📥 [REST-Fill] {} filled {} candles for {}.",
                symbol,
                count,
                day.format("%Y-%m-%d")
            );
        }
        Ok(())
    }
    async fn fetch_and_store_range(
        &self,
        symbol: &Symbol,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<usize> {
        let (mut next_ts, mut total) = (start_ms, 0);

        while next_ts <= end_ms {
            let batch: Vec<_> = self
                .archive_provider
                .fetch_recent_ohlcv(*symbol, Some(next_ts))
                .await?
                .into_iter()
                .take_while(|c| c.timestamp <= end_ms)
                .collect();

            let Some(last_ts) = batch.last().map(|c| c.timestamp) else {
                break;
            };
            let len = batch.len();

            self.storage.insert_candles(&batch).await?;
            total += len;
            next_ts = last_ts + 60000;

            if len < 1000 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Ok(total)
    }

    fn start_runtime_checker(self: Arc<Self>) {
        let manager = Arc::clone(&self);

        tokio::spawn(async move {
            let mut check_interval =
                tokio::time::interval(std::time::Duration::from_secs(3 * 60 * 60));

            loop {
                check_interval.tick().await;

                let res: anyhow::Result<()> = async {
                    let to_fix = self.storage.check_batch_gaps(&self.symbols, 1000).await?;
                    if to_fix.is_empty() {
                        info!("✨ [Checker] Integrity check passed for all symbols.");

                        return Ok(());
                    }

                    let now = Utc::now();

                    let end = now.duration_trunc(chrono::TimeDelta::days(1))?;

                    let start = end - chrono::TimeDelta::days(2);

                    info!(
                        "🛠️ [Checker] Repairing {} symbols... Aligned Range: {} to {}",
                        to_fix.len(),
                        start.format("%Y-%m-%d %H:%M"),
                        end.format("%Y-%m-%d %H:%M")
                    );

                    stream::iter(to_fix)
                        .map(|symbol| {
                            let m = manager.clone();
                            async move { m.sync_single_symbol(&symbol, start, end).await }
                        })
                        .buffer_unordered(3)
                        .for_each(|res| async {
                            if let Err(e) = res {
                                error!("❌ [Checker] Sync failed: {:?}", e);
                            }
                        })
                        .await;

                    manager
                        .storage
                        .refresh_all_chunked(start, end, chrono::TimeDelta::hours(12))
                        .await?;

                    info!("✅ [Checker] All views (M5-D1) refreshed successfully.");
                    Ok(())
                }
                .await;

                if let Err(e) = res {
                    error!("❌ [Checker] Runtime checker encountered error: {:?}", e);
                }
            }
        });
    }
    pub fn start_oi_poller(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;

                stream::iter(self.symbols.clone())
                    .map(|symbol| {
                        let feature_ctx = self.feature_context.clone();
                        let archive = self.archive_provider.clone();
                        async move {
                            if let Ok(oi_data) = archive.fetch_open_interest(symbol).await {
                                feature_ctx.update_oi_from_poller(
                                    symbol,
                                    oi_data.open_interest,
                                    oi_data.time,
                                );
                            }
                        }
                    })
                    .buffer_unordered(10)
                    .collect::<Vec<()>>()
                    .await;
            }
        });
    }
}
