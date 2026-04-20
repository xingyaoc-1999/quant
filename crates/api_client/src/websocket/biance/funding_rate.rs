use crate::websocket::WsProtocol;
use anyhow::Result;
use common::{FundingRateUpdate, Symbol};
use serde::Deserialize;
use serde_json::json;
use std::str::FromStr;

pub struct BinanceFundingRateProtocol;

impl WsProtocol for BinanceFundingRateProtocol {
    type Subscription = Symbol;
    type Output = FundingRateUpdate;

    fn url(&self) -> &str {
        "wss://fstream.binance.com/market/stream"
    }

    fn proxy_target(&self) -> &str {
        "fstream.binance.com:443"
    }

    fn build_subscribe_request(&self, subs: &[Self::Subscription]) -> Result<String> {
        let params: Vec<String> = subs
            .iter()
            .map(|s| format!("{}@markPrice", s.as_str().to_lowercase()))
            .collect();

        let request = json!({
            "method": "SUBSCRIBE",
            "params": params,
            "id": uuid::Uuid::new_v4().to_string(),
        });

        Ok(serde_json::to_string(&request)?)
    }

    fn parse_message(&self, text: &str) -> Option<FundingRateUpdate> {
        #[derive(Deserialize)]
        struct MarkPriceEvent {
            data: MarkPriceData,
        }

        #[derive(Deserialize)]
        struct MarkPriceData {
            #[serde(rename = "s")]
            symbol: String,
            #[serde(rename = "r", deserialize_with = "common::utils::str_to_f64")]
            funding_rate: f64,
            #[serde(rename = "T")]
            next_funding_time: i64,
        }

        let evt: MarkPriceEvent = serde_json::from_str(text).ok()?;
        Some(FundingRateUpdate {
            symbol: Symbol::from_str(&evt.data.symbol).ok()?,
            funding_rate: evt.data.funding_rate,
            next_funding_time: evt.data.next_funding_time,
        })
    }
}
