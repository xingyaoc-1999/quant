use crate::websocket::WsProtocol;
use anyhow::Result;
use common::utils::str_to_f64;
use common::{Candle, Interval, Symbol};
use serde::Deserialize;
use serde_json::json;
use std::str::FromStr;

pub struct BinanceKlineProtocol {
    interval: Interval,
}

impl BinanceKlineProtocol {
    pub fn new(interval: Interval) -> Self {
        Self { interval }
    }

    fn build_stream_name(symbol: &Symbol, interval: &Interval) -> String {
        format!("{}@kline_{}", symbol.as_str().to_lowercase(), interval)
    }
}

impl WsProtocol for BinanceKlineProtocol {
    type Subscription = Symbol;
    type Output = Candle;

    fn url(&self) -> &str {
        "wss://fstream.binance.com/market/stream"
    }

    fn proxy_target(&self) -> &str {
        "fstream.binance.com:443"
    }

    fn build_subscribe_request(&self, subs: &[Self::Subscription]) -> Result<String> {
        if subs.is_empty() {
            return Ok(String::new());
        }

        let params: Vec<String> = subs
            .iter()
            .map(|s| Self::build_stream_name(s, &self.interval))
            .collect();

        let request = json!({
            "method": "SUBSCRIBE",
            "params": params,
            "id": uuid::Uuid::new_v4().to_string(),
        });

        Ok(serde_json::to_string(&request)?)
    }

    fn parse_message(&self, text: &str) -> Option<Candle> {
        #[derive(Deserialize)]
        struct FullEvent {
            data: KlineData,
        }

        #[derive(Deserialize)]
        struct KlineData {
            #[serde(rename = "s")]
            symbol: String,
            k: KlineFields,
        }

        #[derive(Deserialize)]
        struct KlineFields {
            #[serde(rename = "t")]
            open_time: i64,
            #[serde(deserialize_with = "str_to_f64")]
            o: f64,
            #[serde(deserialize_with = "str_to_f64")]
            h: f64,
            #[serde(deserialize_with = "str_to_f64")]
            l: f64,
            #[serde(deserialize_with = "str_to_f64")]
            c: f64,
            #[serde(deserialize_with = "str_to_f64")]
            v: f64,
            #[serde(rename = "q", deserialize_with = "str_to_f64")]
            quote_volume: f64,
            #[serde(rename = "V", deserialize_with = "str_to_f64")]
            taker_v: f64,
            #[serde(rename = "Q", deserialize_with = "str_to_f64")]
            taker_q: f64,
            #[serde(rename = "n")]
            trade_count: i64,
            x: bool,
        }

        let evt: FullEvent = serde_json::from_str(text).ok()?;
        let k = evt.data.k;

        if !k.x {
            return None;
        }

        Some(Candle {
            symbol: Symbol::from_str(&evt.data.symbol).ok()?,
            timestamp: k.open_time,
            open: k.o,
            high: k.h,
            low: k.l,
            close: k.c,
            volume: k.v,
            quote_volume: k.quote_volume,
            taker_buy_volume: k.taker_v,
            taker_buy_quote_volume: k.taker_q,
            trade_count: k.trade_count,
        })
    }
}
