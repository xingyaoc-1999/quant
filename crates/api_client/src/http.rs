pub mod binance;

use std::{collections::HashMap, sync::Arc, time::Duration};

use reqwest::{Client, Proxy};
use thiserror::Error;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Error)]
pub enum RequestError {
    #[error("Proxy error: {0}")]
    Proxy(String),
    #[error("API error {code}: {msg}")]
    Api { code: i64, msg: String },
    #[error("HTTP error {status}: {msg}")]
    Http { status: u16, msg: String },
    #[error("Other error: {0}")]
    Other(String),
}

impl From<reqwest::Error> for RequestError {
    fn from(err: reqwest::Error) -> Self {
        if let Some(status) = err.status() {
            Self::Http {
                status: status.as_u16(),
                msg: err.to_string(),
            }
        } else if err.is_connect() || err.is_timeout() {
            Self::Proxy(err.to_string())
        } else {
            Self::Other(err.to_string())
        }
    }
}
pub struct HttpClientFactory {
    base_client: Client,
    client_cache: Mutex<HashMap<Arc<str>, Client>>,
}

impl HttpClientFactory {
    pub fn new() -> Self {
        Self {
            base_client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("Failed to build base client"),
            client_cache: Mutex::new(HashMap::new()),
        }
    }

    fn get_base_client(&self) -> Client {
        self.base_client.clone()
    }

    async fn get_client_with_proxy(&self, proxy_url: Arc<str>) -> Result<Client, RequestError> {
        {
            let cache = self.client_cache.lock().await;
            if let Some(client) = cache.get(&proxy_url) {
                return Ok(client.clone());
            }
        }

        let proxy = Proxy::all(format!("socks5h://{}", proxy_url))
            .map_err(|_| RequestError::Proxy("Invalid proxy URL".into()))?;

        let client = Client::builder()
            .proxy(proxy)
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| RequestError::Other(e.to_string()))?;

        let mut cache = self.client_cache.lock().await;
        cache.insert(proxy_url, client.clone());
        Ok(client)
    }
    pub async fn get_client(&self, proxy_url: Option<Arc<str>>) -> Result<Client, RequestError> {
        match proxy_url {
            Some(url) => self.get_client_with_proxy(url).await,
            None => Ok(self.get_base_client()),
        }
    }
}

impl Default for HttpClientFactory {
    fn default() -> Self {
        Self::new()
    }
}
