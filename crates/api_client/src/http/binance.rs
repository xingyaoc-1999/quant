use anyhow::{Context, Result};
use common::utils::{retry_with_proxy_rotation_cooled, CooledProxyPool, ShouldRotate};
use common::{BinanceOpenInterest, Candle, Interval, OpenInterestRecord, Symbol};
use std::io::{Cursor, Read};
use std::sync::Arc;

use crate::http::{HttpClientFactory, RequestError};

pub struct ArchiveProvider {
    factory: Arc<HttpClientFactory>,
    proxy_pool: Arc<CooledProxyPool>,
}

impl ArchiveProvider {
    pub fn new(proxy_pool: Arc<CooledProxyPool>) -> Self {
        Self {
            factory: Arc::new(HttpClientFactory::new()),
            proxy_pool,
        }
    }
    pub async fn fetch_open_interest_hist(
        &self,
        symbol: Symbol,
        interval: Interval,
    ) -> Result<Vec<OpenInterestRecord>> {
        let url = format!(
            "https://fapi.binance.com/futures/data/openInterestHist?symbol={}&period={}",
            symbol.to_string().to_uppercase(),
            interval,
        );

        let pool = self.proxy_pool.clone();
        let factory = self.factory.clone();

        let result = retry_with_proxy_rotation_cooled(
            &pool,
            move |proxy| {
                let factory = factory.clone();
                let url = url.clone();

                async move {
                    let client = factory.get_client(proxy).await?;
                    let response = client.get(&url).send().await?;

                    let json: serde_json::Value = response.json().await?;

                    if let Some(code) = json.get("code").and_then(|v| v.as_i64()) {
                        let msg = json.get("msg").and_then(|v| v.as_str()).unwrap_or("");
                        return Err(RequestError::Api {
                            code,
                            msg: msg.to_string(),
                        });
                    }

                    let records: Vec<OpenInterestRecord> = serde_json::from_value(json)
                        .map_err(|e| RequestError::Other(format!("OI Deserialize Error: {}", e)))?;

                    Ok(records)
                }
            },
            BinanceRotator,
        )
        .await;

        result.map_err(|e| anyhow::anyhow!("OI Hist fetch failed: {}", e))
    }

    pub async fn fetch_open_interest(&self, symbol: Symbol) -> Result<BinanceOpenInterest> {
        let symbol_str = symbol.to_string().to_uppercase();
        let url = format!(
            "https://fapi.binance.com/fapi/v1/openInterest?symbol={}",
            symbol_str
        );

        let pool = self.proxy_pool.clone();
        let factory = self.factory.clone();

        let result = retry_with_proxy_rotation_cooled(
            &pool,
            move |proxy| {
                let factory = factory.clone();
                let url = url.clone();

                async move {
                    let client = factory.get_client(proxy).await?;
                    let response = client.get(&url).send().await?;

                    let json: serde_json::Value = response.json().await?;

                    // 业务错误检查
                    if let Some(code) = json.get("code").and_then(|v| v.as_i64()) {
                        let msg = json.get("msg").and_then(|v| v.as_str()).unwrap_or("");
                        return Err(RequestError::Api {
                            code,
                            msg: msg.to_string(),
                        });
                    }

                    let oi_data: BinanceOpenInterest = serde_json::from_value(json)
                        .map_err(|e| RequestError::Other(format!("OI Deserialize Error: {}", e)))?;

                    Ok(oi_data)
                }
            },
            BinanceRotator,
        )
        .await;

        result.map_err(|e| anyhow::anyhow!("OI fetch failed: {}", e))
    }
    pub async fn fetch_recent_ohlcv(
        &self,
        symbol: Symbol,
        since_ms: Option<i64>,
    ) -> Result<Vec<Candle>> {
        let mut url = format!(
            "https://fapi.binance.com/fapi/v1/klines?symbol={}&interval=1m&limit=1000",
            symbol
        );
        if let Some(since) = since_ms {
            url.push_str(&format!("&startTime={}", since));
        }

        let pool = self.proxy_pool.clone();
        let factory = self.factory.clone();

        let result = retry_with_proxy_rotation_cooled(
            &pool,
            move |proxy| {
                let factory = factory.clone();
                let url = url.clone();
                async move {
                    let client = factory.get_client(proxy).await?;
                    let response = client.get(&url).send().await?;

                    let json: serde_json::Value = response.json().await?;

                    if let Some(obj) = json.as_object() {
                        if let Some(code) = obj.get("code").and_then(|c| c.as_i64()) {
                            let msg = obj.get("msg").and_then(|m| m.as_str()).unwrap_or("");
                            return Err(RequestError::Api {
                                code,
                                msg: msg.to_string(),
                            });
                        }
                    }

                    let arr = json
                        .as_array()
                        .ok_or(RequestError::Other("Expected array".into()))?;

                    let candles = arr
                        .iter()
                        .filter_map(|v| {
                            v.as_array()
                                .and_then(|row| Candle::from_binance_rest(row, symbol))
                        })
                        .collect();

                    Ok(candles)
                }
            },
            BinanceRotator,
        )
        .await;

        result.map_err(|e| anyhow::anyhow!("{}", e))
    }

    pub async fn download_archive_candles(
        &self,
        symbol: &Symbol,
        date: &str,
    ) -> Result<Vec<Candle>> {
        let url = format!(
            "https://data.binance.vision/data/futures/um/daily/klines/{sym}/1m/{sym}-1m-{date}.zip",
            sym = symbol,
            date = date
        );

        let client = self.factory.get_client(None).await?;
        let response = client.get(&url).send().await?;

        if response.status() == 404 {
            return Ok(vec![]);
        }

        let full_bytes = response.bytes().await?.to_vec();

        let reader = Cursor::new(full_bytes);
        let mut archive = zip::ZipArchive::new(reader).context("Failed to open zip archive")?;

        let mut file = archive
            .by_index(0)
            .context("Failed to get first file in zip")?;

        let mut csv_buffer = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut csv_buffer)?;

        let candles = Candle::from_csv(csv_buffer, symbol).await?;

        Ok(candles)
    }
}

pub struct BinanceRotator;
impl ShouldRotate<RequestError> for BinanceRotator {
    fn should_rotate(&self, error: &RequestError) -> bool {
        match error {
            RequestError::Proxy(_) => true,
            RequestError::Http { status, .. } => *status == 429 || *status == 418,
            RequestError::Api { code, .. } => *code == -1003,
            _ => false,
        }
    }
}
