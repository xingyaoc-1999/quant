use anyhow::Result;
use common::Symbol;
use std::str::FromStr;

use crate::postgres::Storage;

impl Storage {
    async fn initialize_user_tables(&self) -> Result<()> {
        let conn = self.pool.get().await?;

        // 用户表
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

        // 用户策略表（含静音字段）
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

    pub async fn get_subscribed_symbols(&self, telegram_id: i64) -> Result<Vec<Symbol>> {
        let conn = self.pool.get().await?;
        let sql = format!(
            "SELECT symbol FROM {}.user_strategies WHERE user_id = $1 AND is_subscribed = TRUE ORDER BY symbol",
            self.schema
        );
        let rows = conn.query(&sql, &[&telegram_id]).await?;
        rows.into_iter()
            .map(|row| {
                let symbol_str: &str = row.get(0);
                Symbol::from_str(symbol_str).map_err(|e| anyhow::anyhow!(e))
            })
            .collect()
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

    pub async fn mute_symbol(&self, telegram_id: i64, symbol: Symbol) -> Result<()> {
        let conn = self.pool.get().await?;
        let sql = format!(
            r#"
              INSERT INTO {}.user_strategies (user_id, symbol, is_subscribed, muted)
              VALUES ($1, $2, TRUE, TRUE)
              ON CONFLICT (user_id, symbol) DO UPDATE SET
                  muted = TRUE,
                  updated_at = NOW()
              "#,
            self.schema
        );
        conn.execute(&sql, &[&telegram_id, &symbol.as_str()])
            .await?;
        Ok(())
    }

    pub async fn unmute_symbol(&self, telegram_id: i64, symbol: Symbol) -> Result<()> {
        let conn = self.pool.get().await?;
        let sql = format!(
              "UPDATE {}.user_strategies SET muted = FALSE, updated_at = NOW() WHERE user_id = $1 AND symbol = $2",
              self.schema
          );
        conn.execute(&sql, &[&telegram_id, &symbol.as_str()])
            .await?;
        Ok(())
    }
}
