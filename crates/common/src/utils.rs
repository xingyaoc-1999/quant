use crate::config::Appconfig;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Deserializer};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};
use tracing::{debug, error, warn};

pub type ProxyAddr = Arc<str>;

struct PoolInner {
    proxies: VecDeque<ProxyAddr>,
    cooldowns: HashMap<ProxyAddr, Instant>,
}

pub struct CooledProxyPool {
    inner: Arc<Mutex<PoolInner>>,
    default_cooldown: Duration,
}

impl CooledProxyPool {
    pub fn new(proxies: Vec<String>, default_cooldown: Duration) -> Self {
        let arc_proxies: VecDeque<ProxyAddr> = proxies.into_iter().map(Arc::from).collect();

        let inner = Arc::new(Mutex::new(PoolInner {
            proxies: arc_proxies,
            cooldowns: HashMap::new(),
        }));

        let inner_clone = Arc::clone(&inner);
        let cleanup_interval = default_cooldown * 2;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(cleanup_interval.max(Duration::from_secs(30)));
            loop {
                interval.tick().await;
                let mut guard = inner_clone.lock().await;
                let now = Instant::now();
                let before = guard.cooldowns.len();
                guard.cooldowns.retain(|_, &mut until| until > now);
                let cleaned = before - guard.cooldowns.len();
                if cleaned > 0 {
                    debug!("Proxy cleanup: removed {} expired records", cleaned);
                }
            }
        });

        Self {
            inner,
            default_cooldown,
        }
    }

    pub async fn current_available(&self) -> Option<ProxyAddr> {
        let mut guard = self.inner.lock().await;
        let now = Instant::now();
        let n = guard.proxies.len();

        for _ in 0..n {
            if let Some(proxy) = guard.proxies.pop_front() {
                let is_cooling = guard
                    .cooldowns
                    .get(&proxy)
                    .is_some_and(|&until| until > now);
                if is_cooling {
                    guard.proxies.push_back(proxy);
                } else {
                    guard.proxies.push_back(proxy.clone());
                    return Some(proxy);
                }
            }
        }
        None
    }

    pub async fn mark_failed(&self, proxy: ProxyAddr) {
        let mut guard = self.inner.lock().await;
        let until = Instant::now() + self.default_cooldown;
        guard.cooldowns.insert(proxy.clone(), until);
        warn!(
            "Proxy {} entering cooldown for {:?}",
            proxy, self.default_cooldown
        );
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.proxies.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

pub trait ShouldRotate<E> {
    fn should_rotate(&self, error: &E) -> bool;
}

pub async fn retry_with_proxy_rotation_cooled<P, E, F, Fut, R>(
    proxy_pool: &CooledProxyPool,
    request_fn: F,
    should_rotate: R,
) -> Result<P, E>
where
    F: Fn(Option<ProxyAddr>) -> Fut,
    Fut: Future<Output = Result<P, E>>,
    R: ShouldRotate<E>,
    E: std::fmt::Display + Clone,
{
    let config = Appconfig::global().retry;
    let mut business_retry_count = 0;
    let mut proxy_rotation_count = 0;
    let mut backoff = config.initial_backoff();

    let max_proxy_attempts = proxy_pool.len().await.max(1) + 1;

    loop {
        let current_proxy = proxy_pool.current_available().await;

        match (request_fn)(current_proxy.clone()).await {
            Ok(res) => return Ok(res),
            Err(e) => {
                if should_rotate.should_rotate(&e) {
                    if let Some(p) = current_proxy {
                        proxy_pool.mark_failed(p).await;
                        proxy_rotation_count += 1;

                        if proxy_rotation_count >= max_proxy_attempts {
                            error!("All proxies failed or exhausted. Last error: {}", e);
                            return Err(e);
                        }
                        warn!(
                            "Proxy error (attempt {}): {}. Rotating...",
                            proxy_rotation_count, e
                        );
                        continue;
                    } else {
                        error!("Proxy error but no proxy in use: {}", e);
                        return Err(e);
                    }
                } else {
                    business_retry_count += 1;
                    if business_retry_count > config.max_retries {
                        return Err(e);
                    }

                    warn!(
                        "Business error (Retry {}/{}): {}. Waiting {:?}...",
                        business_retry_count, config.max_retries, e, backoff
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(config.max_backoff());
                }
            }
        }
    }
}

pub fn str_to_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrFloat {
        String(String),
        Float(f64),
    }

    match StringOrFloat::deserialize(deserializer)? {
        StringOrFloat::String(s) => s.parse::<f64>().map_err(serde::de::Error::custom),
        StringOrFloat::Float(f) => Ok(f),
    }
}

pub fn str_to_i64<'de, D>(deserializer: D) -> std::result::Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrInt {
        String(String),
        Int(i64),
    }

    match StringOrInt::deserialize(deserializer)? {
        StringOrInt::String(s) => s.parse::<i64>().map_err(serde::de::Error::custom),
        StringOrInt::Int(i) => Ok(i),
    }
}

pub async fn retry<F, Fut, T, E>(mut operation: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let config = Appconfig::global().retry;
    let mut retries = 0;
    let mut backoff = config.initial_backoff();

    loop {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                retries += 1;
                if retries > config.max_retries {
                    return Err(e);
                }

                warn!("Retry {}/{}: {}", retries, config.max_retries, e);
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(config.max_backoff());
            }
        }
    }
}
pub fn parse_proxy_auth(proxy_str: &str) -> Result<(&str, &str, &str)> {
    let (auth_part, addr) = proxy_str
        .split_once('@')
        .ok_or_else(|| anyhow!("Invalid proxy format: missing '@'"))?;

    let (username, password) = auth_part
        .split_once(':')
        .ok_or_else(|| anyhow!("Invalid auth format: missing ':'"))?;

    Ok((addr, username, password))
}
