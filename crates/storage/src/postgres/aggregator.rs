use crate::postgres::Storage;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use common::Interval;
use tracing::{info, warn};

impl Storage {
    pub(super) async fn setup_auto_refresh_policies(&self) -> Result<()> {
        let conn = self.pool.get().await?;

        const POLICIES: &[(Interval, &str, &str, &str)] = &[
            (Interval::M5, "20 minutes", "5 minutes", "2 minutes"),
            (Interval::M15, "1 hour", "15 minutes", "5 minutes"),
            (Interval::M30, "2 hours", "30 minutes", "10 minutes"),
            (Interval::H1, "4 hours", "1 hour", "15 minutes"),
            (Interval::H4, "12 hours", "4 hours", "30 minutes"),
            (Interval::D1, "4 days", "1 day", "1 hour"),
        ];

        let schema = &self.schema;

        for (interval, start_offset, end_offset, schedule_interval) in POLICIES {
            let view_name = interval.view_name();

            let sql = format!(
                r#"
SELECT add_continuous_aggregate_policy(
    '{schema}.{view}',
    start_offset => INTERVAL '{start}',
    end_offset => INTERVAL '{end}',
    schedule_interval => INTERVAL '{schedule}',
    if_not_exists => true
);
"#,
                schema = schema,
                view = view_name,
                start = start_offset,
                end = end_offset,
                schedule = schedule_interval
            );

            match conn.execute(&sql, &[]).await {
                Ok(_) => info!(
                    "Auto refresh policy configured: {} (start={}, end={}, interval={})",
                    view_name, start_offset, end_offset, schedule_interval
                ),
                Err(e) => warn!(
                    "Failed to configure refresh policy for {}: {}",
                    view_name, e
                ),
            }
        }

        Ok(())
    }

    async fn refresh_view(
        &self,
        interval: &Interval,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.pool.get().await?;
        let schema = &self.schema;

        let sql = format!(
            "CALL refresh_continuous_aggregate('{}.{}', $1::timestamptz, $2::timestamptz);",
            schema,
            interval.view_name()
        );

        conn.execute(&sql, &[&start, &end]).await.with_context(|| {
            format!(
                "Failed to refresh continuous aggregate for {}: {} -> {}",
                interval.view_name(),
                start,
                end
            )
        })?;

        Ok(())
    }

    async fn refresh_all_intervals(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let intervals: Vec<Interval> = Interval::all()
            .into_iter()
            .filter(|i| *i != Interval::M1)
            .collect();

        info!(
            "Starting bulk refresh for {} aggregate views, range: [{} to {}]",
            intervals.len(),
            start,
            end
        );

        for interval in intervals {
            match self.refresh_view(&interval, start, end).await {
                Ok(_) => {
                    info!("refreshed continuous aggregate: {:?}", interval);
                }
                Err(e) => {
                    warn!(
                        "Failed to refresh view for {:?}: {:?}. Skipping to next.",
                        interval, e
                    );
                }
            }
        }

        info!("All applicable interval view refresh commands have been processed.");
        Ok(())
    }
    pub async fn refresh_all_chunked(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        step: chrono::TimeDelta,
    ) -> Result<()> {
        if step <= chrono::Duration::zero() {
            return Err(anyhow::anyhow!("Step must be positive"));
        }
        let mut current_start = start;

        while current_start < end {
            let current_end = (current_start + step).min(end);

            info!(
                "⏳ [DB-Refresh] Window: {} -> {}",
                current_start.format("%m-%d %H:%M"),
                current_end.format("%m-%d %H:%M")
            );

            self.refresh_all_intervals(current_start, current_end)
                .await?;

            current_start = current_end;

            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
        Ok(())
    }
    pub(super) async fn enable_compression_for_all(&self) -> Result<()> {
        self.enable_compression_for_hypertable("candles_1m").await?;
        Ok(())
    }

    async fn enable_compression_for_hypertable(&self, table_name: &str) -> Result<()> {
        let conn = self.pool.get().await?;

        let full_name = format!("{}.{}", self.schema, table_name);

        let alter_sql = format!(
            r#"
ALTER TABLE {table}
SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'symbol',
    timescaledb.compress_orderby = 'bucket DESC'
);
"#,
            table = full_name
        );

        if let Err(e) = conn.execute(&alter_sql, &[]).await {
            warn!(
                "Compression may already be enabled for {}: {}",
                table_name, e
            );
        } else {
            info!("Compression enabled for table: {}", table_name);
        }

        let policy_sql = format!(
            "SELECT add_compression_policy('{}', INTERVAL '7 days', if_not_exists => true);",
            full_name
        );

        conn.execute(&policy_sql, &[]).await?;

        info!("Compression policy configured for table: {}", table_name);

        Ok(())
    }
}
