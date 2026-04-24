use std::{sync::OnceLock, time::Duration};

use config::ConfigError;
use serde::{Deserialize, Serialize};

use crate::Interval;

static GLOBAL_CONFIG: OnceLock<Appconfig> = OnceLock::new();
#[derive(Deserialize)]
pub struct Appconfig {
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub role: RoleConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    pub telegram: TelegramConfig,
    //
    #[serde(default)]
    pub retry: RetryConfig,
}
#[derive(Deserialize, Debug)]
#[serde(default)]
pub struct DatabaseConfig {
    pub db_url: String,

    pub schema: String,

    pub pool_size: usize,
}
impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            db_url: String::new(),
            schema: "public".into(),
            pool_size: 10,
        }
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct RoleConfig {
    pub trend: Interval,
    pub filter: Interval,
    pub entry: Interval,
}

impl Default for RoleConfig {
    fn default() -> Self {
        Self {
            trend: Interval::D1,
            filter: Interval::H4,
            entry: Interval::H1,
        }
    }
}
#[derive(Clone, Copy, Deserialize, Debug)]
#[serde(default)]
pub struct RetryConfig {
    pub max_retries: usize,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff_ms: 200,
            max_backoff_ms: 2000,
        }
    }
}

impl RetryConfig {
    pub fn initial_backoff(&self) -> Duration {
        Duration::from_millis(self.initial_backoff_ms)
    }

    pub fn max_backoff(&self) -> Duration {
        Duration::from_millis(self.max_backoff_ms)
    }
}

#[derive(Deserialize)]
pub struct ProxyConfig {
    pub socks_proxy_list: Vec<String>,
}

#[derive(Deserialize)]
pub struct TelegramConfig {
    pub token: String,
}
impl Appconfig {
    fn load() -> Result<Self, ConfigError> {
        let config = config::Config::builder()
            .add_source(config::File::with_name("config"))
            .build()?
            .try_deserialize()?;

        Ok(config)
    }

    pub fn global() -> &'static Appconfig {
        GLOBAL_CONFIG.get_or_init(|| Appconfig::load().expect("Failed to read config.toml"))
    }
}
