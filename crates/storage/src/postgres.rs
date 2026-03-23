use anyhow::{Context, Result};
use chrono::{DateTime, DurationRound, TimeZone, Utc};
use common::{config::DatabaseConfig, Candle, Interval, Symbol};
use deadpool_postgres::{Manager, ManagerConfig, Pool, PoolConfig, RecyclingMethod, Runtime};

use std::collections::HashMap;
use std::str::FromStr;
use tokio_postgres::{NoTls, Row};
use tracing::info;
mod aggregator;
const CANDLE_QUERY_FIELDS: &str = "symbol, bucket, open, high, low, close, volume, quote_volume, taker_buy_volume, taker_buy_quote_volume, trade_count";

pub struct Storage {
    pub pool: Pool,
    pub schema: String,
}

impl Storage {
    pub fn new(cfg: &DatabaseConfig) -> Result<Self> {
        Self::validate_identifier(&cfg.schema)?;

        let mut pg_cfg = cfg
            .db_url
            .parse::<tokio_postgres::Config>()
            .context("Invalid db_url")?;

        pg_cfg.options("-c timezone=UTC");

        let mgr = Manager::from_config(
            pg_cfg,
            NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            },
        );

        let pool = Pool::builder(mgr)
            .config(PoolConfig {
                max_size: cfg.pool_size,
                ..Default::default()
            })
            .runtime(Runtime::Tokio1)
            .build()
            .context("Failed to build pool")?;

        Ok(Self {
            pool,
            schema: cfg.schema.clone(),
        })
    }

    pub async fn insert_candles(&self, candles: &[Candle]) -> Result<u64> {
        if candles.is_empty() {
            return Ok(0);
        }

        let conn = self.pool.get().await?;
        let schema = &self.schema;

        let mut symbols = Vec::with_capacity(candles.len());
        let mut buckets = Vec::with_capacity(candles.len());
        let mut opens = Vec::with_capacity(candles.len());
        let mut highs = Vec::with_capacity(candles.len());
        let mut lows = Vec::with_capacity(candles.len());
        let mut closes = Vec::with_capacity(candles.len());
        let mut volumes = Vec::with_capacity(candles.len());
        let mut quote_volumes = Vec::with_capacity(candles.len());
        let mut taker_buy_volumes = Vec::with_capacity(candles.len());
        let mut taker_buy_quote_volumes = Vec::with_capacity(candles.len());
        let mut trade_counts = Vec::with_capacity(candles.len());

        for c in candles {
            let dt = Utc
                .timestamp_millis_opt(c.timestamp)
                .single()
                .context("Invalid candle timestamp")?;

            let aligned_dt = dt.duration_trunc(chrono::Duration::minutes(1))?;

            symbols.push(c.symbol.as_str());
            buckets.push(aligned_dt);
            opens.push(c.open);
            highs.push(c.high);
            lows.push(c.low);
            closes.push(c.close);
            volumes.push(c.volume);
            quote_volumes.push(c.quote_volume);
            taker_buy_volumes.push(c.taker_buy_volume);
            taker_buy_quote_volumes.push(c.taker_buy_quote_volume);
            trade_counts.push(c.trade_count);
        }

        let insert_sql = format!(
            "INSERT INTO {}.candles_1m (
            symbol, bucket, open, high, low, close, 
            volume, quote_volume, taker_buy_volume, 
            taker_buy_quote_volume, trade_count
        )
        SELECT * FROM UNNEST(
            $1::text[], $2::timestamptz[], $3::float8[], $4::float8[], $5::float8[], $6::float8[],
            $7::float8[], $8::float8[], $9::float8[], $10::float8[], $11::int8[]
        )
        ON CONFLICT (symbol, bucket) DO NOTHING;",
            schema
        );

        let rows = conn
            .execute(
                &insert_sql,
                &[
                    &symbols,
                    &buckets,
                    &opens,
                    &highs,
                    &lows,
                    &closes,
                    &volumes,
                    &quote_volumes,
                    &taker_buy_volumes,
                    &taker_buy_quote_volumes,
                    &trade_counts,
                ],
            )
            .await?;

        Ok(rows)
    }

    pub async fn get_batch(
        &self,
        interval: Interval,
        symbols: &[Symbol],
        limit: u32,
    ) -> Result<HashMap<Symbol, Vec<Candle>>> {
        let client = self.pool.get().await?;
        let table = interval.view_name();

        let sql = format!(
            r#"
    SELECT p.symbol, p.bucket, p.open, p.high, p.low, p.close, p.volume, 
           p.quote_volume, p.taker_buy_volume, p.taker_buy_quote_volume, p.trade_count
    FROM unnest($2::text[]) AS s(name)
    INNER JOIN LATERAL (
        SELECT * FROM (
            SELECT symbol, bucket, open, high, low, close, volume, 
                   quote_volume, taker_buy_volume, taker_buy_quote_volume, trade_count
            FROM {}.{}
            WHERE symbol = s.name
            ORDER BY bucket DESC
            LIMIT $1
        ) sub
        ORDER BY sub.bucket ASC
    ) p ON true;
    "#,
            self.schema, table
        );

        let stmt = client.prepare_cached(&sql).await?;

        let symbol_refs: Vec<&str> = symbols.iter().map(|s| s.as_str()).collect();

        let rows = client
            .query(&stmt, &[&(limit as i64), &symbol_refs])
            .await?;

        let mut results: HashMap<Symbol, Vec<Candle>> = HashMap::with_capacity(symbols.len());

        for row in rows {
            let candle = Self::map_row_to_candle(&row)?;
            results
                .entry(candle.symbol)
                .or_insert_with(|| Vec::with_capacity(limit as usize))
                .push(candle);
        }

        Ok(results)
    }

    pub async fn get_latest_candles(
        &self,
        symbol: &Symbol,
        interval: Interval,
        limit: u32,
    ) -> Result<Vec<Candle>> {
        let client = self.pool.get().await?;
        let table = interval.view_name();

        let sql = format!(
            "SELECT {} FROM {}.{} WHERE symbol = $1 ORDER BY bucket DESC LIMIT $2;",
            CANDLE_QUERY_FIELDS, self.schema, table
        );

        let stmt = client.prepare_cached(&sql).await?;

        let rows = client
            .query(&stmt, &[&symbol.as_str(), &(limit as i64)])
            .await?;

        let mut candles = rows
            .iter()
            .map(Self::map_row_to_candle)
            .collect::<Result<Vec<_>>>()?;

        candles.reverse();

        Ok(candles)
    }
    async fn initialize_tables(&self) -> Result<()> {
        let mut conn = self.pool.get().await?;
        let tx = conn.transaction().await?;

        tx.execute(
            &format!(
                r#"
                CREATE TABLE IF NOT EXISTS {}.candles_1m (
                    symbol TEXT NOT NULL,
                    bucket TIMESTAMPTZ NOT NULL,
                    open DOUBLE PRECISION NOT NULL,
                    high DOUBLE PRECISION NOT NULL,
                    low DOUBLE PRECISION NOT NULL,
                    close DOUBLE PRECISION NOT NULL,
                    volume DOUBLE PRECISION NOT NULL,
                    quote_volume DOUBLE PRECISION,
                    taker_buy_volume DOUBLE PRECISION,
                    taker_buy_quote_volume DOUBLE PRECISION,
                    trade_count BIGINT,
                    PRIMARY KEY (symbol, bucket)
                );
            "#,
                self.schema
            ),
            &[],
        )
        .await?;

        tx.execute(
            &format!(
                "SELECT create_hypertable('{}.candles_1m', 'bucket', if_not_exists => TRUE);",
                self.schema
            ),
            &[],
        )
        .await?;

        tx.execute(
            &format!(
                "CREATE INDEX IF NOT EXISTS idx_candles_symbol_bucket ON {}.candles_1m (symbol, bucket DESC);",
                self.schema
            ),
            &[],
        ).await?;

        tx.commit().await?;
        info!("Database schema initialized: {}.candles_1m", self.schema);
        Ok(())
    }

    #[inline]
    fn map_row_to_candle(row: &Row) -> Result<Candle> {
        let s_str: &str = row.get(0);
        let symbol = Symbol::from_str(s_str).map_err(|e| anyhow::anyhow!(e))?;
        let dt: DateTime<Utc> = row.get(1);
        Ok(Candle {
            symbol,
            timestamp: dt.timestamp_millis(),
            open: row.get(2),
            high: row.get(3),
            low: row.get(4),
            close: row.get(5),
            volume: row.get(6),
            quote_volume: row.get(7),
            taker_buy_volume: row.get(8),
            taker_buy_quote_volume: row.get(9),
            trade_count: row.get(10),
        })
    }

    fn validate_identifier(id: &str) -> Result<()> {
        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            anyhow::bail!("Invalid schema identifier: {}", id);
        }
        Ok(())
    }

    pub async fn get_incomplete_days(
        &self,
        symbol: &Symbol,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<DateTime<Utc>>> {
        let client = self.pool.get().await?;

        let sql = format!(
            r#"
        WITH expected_range AS (
            -- 生成从开始到结束的所有日期序列，确保对齐到天
            SELECT generate_series(
                DATE_TRUNC('day', $2::timestamptz), 
                DATE_TRUNC('day', $3::timestamptz) - INTERVAL '1 day', 
                INTERVAL '1 day'
            ) AS day
        ),
        actual_data AS (
     
            SELECT 
                DATE_TRUNC('day', bucket) AS day, 
                COUNT(*) as cnt 
            FROM "{}"."candles_1m" 
            WHERE symbol = $1 
              AND bucket >= DATE_TRUNC('day', $2::timestamptz)
              AND bucket < DATE_TRUNC('day', $3::timestamptz)
            GROUP BY 1
        )
        SELECT er.day 
        FROM expected_range er
        LEFT JOIN actual_data ad ON er.day = ad.day
        WHERE ad.cnt IS NULL OR ad.cnt < 1440
        ORDER BY er.day ASC;
        "#,
            self.schema
        );

        let rows = client
            .query(&sql, &[&symbol.as_str(), &start, &end])
            .await?;

        Ok(rows.iter().map(|r| r.get(0)).collect())
    }
    pub async fn check_batch_gaps(&self, symbols: &[Symbol], minutes: i32) -> Result<Vec<Symbol>> {
        let symbol_strs: Vec<String> = symbols.iter().map(|s| s.to_string()).collect();
        let sql = format!(
            r#"SELECT symbol, count(*) 
       FROM "{}"."candles_1m" 
       WHERE bucket >= now() - interval '{} minutes' 
       AND symbol = ANY($1) 
       GROUP BY symbol 
       HAVING count(*) < {}"#,
            self.schema, minutes, minutes
        );

        let client = self.pool.get().await?;
        let rows = client.query(&sql, &[&symbol_strs]).await?;

        let broken_symbols: Vec<Symbol> = rows
            .iter()
            .map(|row| {
                let s: String = row.get(0);
                // 将 Result<Symbol, String> 转为 Result<Symbol, anyhow::Error>
                Symbol::from_str(&s).map_err(|e| anyhow::anyhow!(e))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(broken_symbols)
    }

    pub async fn initialize_all(&self) -> Result<()> {
        self.initialize_tables().await?;

        info!("Starting continuous aggregate initialization...");

        for interval in Interval::all().into_iter().filter(|i| *i != Interval::M1) {
            self.create_materialized_view(interval).await?;
        }

        self.setup_auto_refresh_policies().await?;
        self.enable_compression_for_all().await?;

        info!("Continuous aggregate initialization completed");
        Ok(())
    }

    async fn create_materialized_view(&self, interval: Interval) -> Result<()> {
        let conn = self.pool.get().await?;

        let view_name = interval.view_name();
        let interval_sql = interval.as_sql_interval();
        let schema = &self.schema;

        let sql = format!(
            r#"
CREATE MATERIALIZED VIEW IF NOT EXISTS "{schema}".{view}
WITH (timescaledb.continuous) AS
SELECT
    symbol,
    time_bucket('{interval}', bucket) AS bucket,
    first(open, bucket) AS open,
    max(high) AS high,
    min(low) AS low,
    last(close, bucket) AS close,
    sum(volume) AS volume,
    sum(quote_volume) AS quote_volume,
    sum(taker_buy_volume) AS taker_buy_volume,
    sum(taker_buy_quote_volume) AS taker_buy_quote_volume,
    sum(trade_count)::BIGINT AS trade_count
FROM "{schema}".candles_1m
GROUP BY symbol, time_bucket('{interval}', bucket)
WITH NO DATA;
"#,
            schema = schema,
            view = view_name,
            interval = interval_sql
        );

        conn.execute(&sql, &[]).await?;

        info!(
            "Continuous aggregate created/verified: {}.{}",
            schema, view_name
        );

        // Perform initial full refresh
        let refresh_sql = format!(
            "CALL refresh_continuous_aggregate('{}.{}', NULL, NULL);",
            schema, view_name
        );

        conn.execute(&refresh_sql, &[]).await?;

        info!("Initial refresh completed for view: {}", view_name);

        Ok(())
    }
}
