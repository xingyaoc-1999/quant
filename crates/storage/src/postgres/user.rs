use anyhow::{bail, Result};
use common::Symbol;
use std::str::FromStr;
use tracing::warn;

use crate::postgres::Storage;

impl Storage {
    pub async fn initialize_user_tables(&self) -> Result<()> {
        let conn = self.pool.get().await?;

        conn.execute(
            &format!(
                r#"
                CREATE TABLE IF NOT EXISTS {}.users (
                    id BIGSERIAL PRIMARY KEY,
                    telegram_id BIGINT UNIQUE NOT NULL,
                    created_at TIMESTAMPTZ DEFAULT NOW(),
                    updated_at TIMESTAMPTZ DEFAULT NOW()
                );
                "#,
                self.schema
            ),
            &[],
        )
        .await?;

        conn.execute(
            &format!(
                r#"
                CREATE TABLE IF NOT EXISTS {}.user_strategies (
                    id BIGSERIAL PRIMARY KEY,
                    user_id BIGINT NOT NULL REFERENCES {}.users(telegram_id) ON DELETE CASCADE,
                    symbol TEXT NOT NULL,
                    trend_interval TEXT NOT NULL DEFAULT '1d',
                    filter_interval TEXT NOT NULL DEFAULT '4h',
                    entry_interval TEXT NOT NULL DEFAULT '1h',
                    is_active BOOLEAN DEFAULT TRUE,
                    is_subscribed BOOLEAN DEFAULT TRUE,
                    muted BOOLEAN DEFAULT FALSE,
                    created_at TIMESTAMPTZ DEFAULT NOW(),
                    updated_at TIMESTAMPTZ DEFAULT NOW(),
                    UNIQUE(user_id, symbol)
                );
                "#,
                self.schema, self.schema
            ),
            &[],
        )
        .await?;

        conn.execute(
            &format!(
                r#"
                CREATE TABLE IF NOT EXISTS {}.active_trades (
                    id BIGSERIAL PRIMARY KEY,
                    user_id BIGINT NOT NULL REFERENCES {}.users(telegram_id) ON DELETE CASCADE,
                    symbol TEXT NOT NULL,
                    direction TEXT NOT NULL,
                    entry_price DOUBLE PRECISION NOT NULL,
                    position_size DOUBLE PRECISION NOT NULL,
                    stop_loss DOUBLE PRECISION,
                    take_profit1 DOUBLE PRECISION,
                    take_profit2 DOUBLE PRECISION,
                    take_profit3 DOUBLE PRECISION,
                    status TEXT NOT NULL DEFAULT 'closed',
                    opened_at TIMESTAMPTZ DEFAULT NOW(),
                    updated_at TIMESTAMPTZ DEFAULT NOW(),
                    closed_at TIMESTAMPTZ,
                    close_reason TEXT,
                    parent_signal_id TEXT,
                    metadata JSONB DEFAULT '{{}}'
                );
                "#,
                self.schema, self.schema
            ),
            &[],
        )
        .await?;

        Ok(())
    }

    pub async fn ensure_user(&self, telegram_id: i64) -> Result<()> {
        let conn = self.pool.get().await?;
        let sql = format!(
            r#"
            INSERT INTO {}.users (telegram_id)
            VALUES ($1)
            ON CONFLICT (telegram_id) DO NOTHING
            "#,
            self.schema
        );
        conn.execute(&sql, &[&telegram_id]).await?;
        Ok(())
    }

    pub async fn subscribe_symbol(&self, telegram_id: i64, symbol: Symbol) -> Result<()> {
        self.ensure_user(telegram_id).await?;

        let conn = self.pool.get().await?;
        let sql = format!(
            r#"
            INSERT INTO {}.user_strategies (user_id, symbol, is_subscribed)
            VALUES ($1, $2, TRUE)
            ON CONFLICT (user_id, symbol) DO UPDATE SET
                is_subscribed = TRUE,
                updated_at = NOW()
            "#,
            self.schema
        );
        conn.execute(&sql, &[&telegram_id, &symbol.as_str()])
            .await?;
        Ok(())
    }

    pub async fn unsubscribe_symbol(&self, telegram_id: i64, symbol: Symbol) -> Result<()> {
        let conn = self.pool.get().await?;
        let sql = format!(
            "UPDATE {}.user_strategies SET is_subscribed = FALSE, updated_at = NOW() WHERE user_id = $1 AND symbol = $2",
            self.schema
        );
        conn.execute(&sql, &[&telegram_id, &symbol.as_str()])
            .await?;
        Ok(())
    }

    pub async fn mute_symbol(&self, telegram_id: i64, symbol: Symbol) -> Result<()> {
        self.ensure_user(telegram_id).await?;

        let conn = self.pool.get().await?;
        let sql = format!(
            "UPDATE {}.user_strategies SET muted = TRUE, updated_at = NOW() WHERE user_id = $1 AND symbol = $2",
            self.schema
        );
        let affected = conn
            .execute(&sql, &[&telegram_id, &symbol.as_str()])
            .await?;
        if affected == 0 {
            bail!("cannot mute: no subscription found for symbol {}", symbol);
        }
        Ok(())
    }

    pub async fn unmute_symbol(&self, telegram_id: i64, symbol: Symbol) -> Result<()> {
        self.ensure_user(telegram_id).await?;

        let conn = self.pool.get().await?;
        let sql = format!(
            "UPDATE {}.user_strategies SET muted = FALSE, updated_at = NOW() WHERE user_id = $1 AND symbol = $2",
            self.schema
        );
        let affected = conn
            .execute(&sql, &[&telegram_id, &symbol.as_str()])
            .await?;
        if affected == 0 {
            bail!("cannot unmute: no subscription found for symbol {}", symbol);
        }
        Ok(())
    }

    pub async fn get_subscribed_symbols(&self, telegram_id: i64) -> Result<Vec<Symbol>> {
        let conn = self.pool.get().await?;
        let sql = format!(
            "SELECT symbol FROM {}.user_strategies WHERE user_id = $1 AND is_subscribed = TRUE ORDER BY symbol",
            self.schema
        );
        let rows = conn.query(&sql, &[&telegram_id]).await?;
        let symbols: Vec<Symbol> = rows
            .into_iter()
            .filter_map(|row| {
                let s: &str = row.get(0);
                match Symbol::from_str(s) {
                    Ok(sym) => Some(sym),
                    Err(e) => {
                        warn!(
                            "invalid symbol '{}' in database for user {}: {}",
                            s, telegram_id, e
                        );
                        None
                    }
                }
            })
            .collect();
        Ok(symbols)
    }

    pub async fn get_subscribed_users(&self, symbol: Symbol) -> Result<Vec<i64>> {
        let conn = self.pool.get().await?;
        let sql = format!(
            "SELECT user_id FROM {}.user_strategies WHERE symbol = $1 AND is_subscribed = TRUE",
            self.schema
        );
        let rows = conn.query(&sql, &[&symbol.as_str()]).await?;
        Ok(rows.into_iter().map(|row| row.get(0)).collect())
    }

    pub async fn open_trade(
        &self,
        telegram_id: i64,
        symbol: Symbol,
        direction: &str,
        entry_price: f64,
        position_size: f64,
        stop_loss: Option<f64>,
        take_profits: &[f64],
        parent_signal_id: Option<&str>,
    ) -> Result<()> {
        self.ensure_user(telegram_id).await?;
        let mut conn = self.pool.get().await?;

        let tx = conn.transaction().await?;

        let close_sql = format!(
            "UPDATE {}.active_trades SET status = 'closed', closed_at = NOW(), close_reason = 'replaced' WHERE user_id = $1 AND symbol = $2 AND status = 'open'",
            self.schema
        );
        tx.execute(&close_sql, &[&telegram_id, &symbol.as_str()])
            .await?;

        let tp1 = take_profits.get(0).copied();
        let tp2 = take_profits.get(1).copied();
        let tp3 = take_profits.get(2).copied();

        let insert_sql = format!(
            "INSERT INTO {}.active_trades (user_id, symbol, direction, entry_price, position_size, stop_loss, take_profit1, take_profit2, take_profit3, parent_signal_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            self.schema
        );
        tx.execute(
            &insert_sql,
            &[
                &telegram_id,
                &symbol.as_str(),
                &direction,
                &entry_price,
                &position_size,
                &stop_loss,
                &tp1,
                &tp2,
                &tp3,
                &parent_signal_id,
            ],
        )
        .await?;

        tx.commit().await?;
        Ok(())
    }

    pub async fn close_trade(&self, telegram_id: i64, symbol: Symbol, reason: &str) -> Result<()> {
        let conn = self.pool.get().await?;
        let sql = format!(
            "UPDATE {}.active_trades SET status = 'closed', closed_at = NOW(), close_reason = $3 WHERE user_id = $1 AND symbol = $2 AND status = 'open'",
            self.schema
        );
        conn.execute(&sql, &[&telegram_id, &symbol.as_str(), &reason])
            .await?;
        Ok(())
    }

    pub async fn update_trade_stops(
        &self,
        telegram_id: i64,
        symbol: Symbol,
        stop_loss: Option<f64>,
        take_profits: &[f64],
    ) -> Result<()> {
        let conn = self.pool.get().await?;
        let mut updates = vec![];
        // 使用基于最终参数数组长度的占位符编号
        // 固定参数: $1 = user_id, $2 = symbol（在 WHERE 子句中）
        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![];

        if let Some(sl) = stop_loss.as_ref() {
            params.push(sl);
            // 参数序号从 3 开始，因为 $1 和 $2 已用于 user_id 和 symbol
            updates.push(format!("stop_loss = ${}", params.len() + 2));
        }
        if let Some(tp1) = take_profits.get(0) {
            params.push(tp1);
            updates.push(format!("take_profit1 = ${}", params.len() + 2));
        }
        if let Some(tp2) = take_profits.get(1) {
            params.push(tp2);
            updates.push(format!("take_profit2 = ${}", params.len() + 2));
        }
        if let Some(tp3) = take_profits.get(2) {
            params.push(tp3);
            updates.push(format!("take_profit3 = ${}", params.len() + 2));
        }

        if updates.is_empty() {
            return Ok(());
        }

        updates.push("updated_at = NOW()".into());
        let set_clause = updates.join(", ");
        let sql = format!(
            "UPDATE {}.active_trades SET {} WHERE user_id = $1 AND symbol = $2 AND status = 'open'",
            self.schema, set_clause
        );
        let symbol_str = symbol.as_str();

        let mut all_params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            vec![&telegram_id, &symbol_str];
        all_params.extend(params);
        conn.execute(&sql, &all_params[..]).await?;
        Ok(())
    }
    pub async fn has_open_trade(&self, telegram_id: i64, symbol: Symbol) -> Result<bool> {
        let conn = self.pool.get().await?;
        let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM {}.active_trades WHERE user_id = $1 AND symbol = $2 AND status = 'open')",
        self.schema
    );
        let row = conn
            .query_one(&sql, &[&telegram_id, &symbol.as_str()])
            .await?;
        Ok(row.get(0))
    }
}
