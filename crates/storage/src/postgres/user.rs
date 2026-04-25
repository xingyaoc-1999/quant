use anyhow::{bail, Result};
use common::Symbol;
use std::str::FromStr;
use tracing::warn; // 可替换为 log::warn

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

        Ok(())
    }

    /// 确保用户存在，幂等操作
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

    /// 静音某交易对，要求用户已订阅该交易对，否则返回错误
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

    /// 取消静音，同样要求订阅记录存在
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
}
