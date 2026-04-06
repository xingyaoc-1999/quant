use crate::utils::{str_to_f64, str_to_i64};
use anyhow::Result;
use chrono::Duration;
use polars::{io::SerReader, prelude::CsvReadOptions};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fmt::{self},
    io::Cursor,
    str::FromStr,
};
use ta::{Close, High, Low, Open, Volume};
pub mod config;

pub mod utils;
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, PartialOrd, Ord,
)]
pub enum Symbol {
    BTCUSDT,
    ETHUSDT,
    // BNBUSDT,
    // SOLUSDT,
    // XRPUSDT,
}

impl Symbol {
    pub fn is_btc(&self) -> bool {
        matches!(self, Symbol::BTCUSDT)
    }
    pub fn all() -> Vec<Self> {
        let symbols = &[
            Self::BTCUSDT,
            Self::ETHUSDT,
            // Self::BNBUSDT,
            // Self::SOLUSDT,
            // Self::XRPUSDT,
        ];
        symbols.to_vec()
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Symbol::BTCUSDT => "BTCUSDT",
            Symbol::ETHUSDT => "ETHUSDT",
            // Symbol::BNBUSDT => "BNBUSDT",
            // Symbol::SOLUSDT => "SOLUSDT",
            // Symbol::XRPUSDT => "XRPUSDT",
        }
    }
}

impl FromStr for Symbol {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "BTCUSDT" | "BTC" => Ok(Symbol::BTCUSDT),
            "ETHUSDT" | "ETH" => Ok(Symbol::ETHUSDT),
            // "BNBUSDT" | "BNB" => Ok(Symbol::BNBUSDT),
            // "SOLUSDT" | "SOL" => Ok(Symbol::SOLUSDT),
            // "XRPUSDT" | "XRP" => Ok(Symbol::XRPUSDT),
            _ => Err(format!("Unsupported symbol: {}", s)),
        }
    }
}
impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Copy)]

pub struct Candle {
    pub symbol: Symbol,
    pub timestamp: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub quote_volume: f64,
    pub taker_buy_volume: f64,
    pub taker_buy_quote_volume: f64,
    pub trade_count: i64,
}

macro_rules! impl_ta_traits {
    ($struct:ident, $($trait:ident => $method:ident),*) => {
        $(
            impl $trait for $struct {
                fn $method(&self) -> f64 {
                    self.$method
                }
            }
        )*
    };
}
impl_ta_traits!(
    Candle,
    Open => open,
    High => high,
    Low => low,
    Close => close,
    Volume => volume
);
impl Candle {
    pub async fn from_csv(csv_data: Vec<u8>, symbol: &Symbol) -> Result<Vec<Candle>> {
        if csv_data.is_empty() {
            return Ok(vec![]);
        }

        let cursor = Cursor::new(csv_data);

        let df = CsvReadOptions::default()
            .with_has_header(true)
            .into_reader_with_file_handle(cursor)
            .finish()?;

        let open_time = df.column("open_time")?.i64()?;
        let open = df.column("open")?.f64()?;
        let high = df.column("high")?.f64()?;
        let low = df.column("low")?.f64()?;
        let close = df.column("close")?.f64()?;
        let volume = df.column("volume")?.f64()?;

        let get_f64_col = |name: &str| df.column(name).ok().and_then(|c| c.f64().ok());
        let get_i64_col = |name: &str| df.column(name).ok().and_then(|c| c.i64().ok());

        let quote = get_f64_col("quote_volume");
        let taker_buy = get_f64_col("taker_buy_volume");
        let taker_quote = get_f64_col("taker_buy_quote_volume");
        let count = get_i64_col("count");

        let mut candles = Vec::with_capacity(df.height());

        for i in 0..df.height() {
            candles.push(Candle {
                symbol: *symbol,
                timestamp: open_time.get(i).unwrap_or(0),
                open: open.get(i).unwrap_or(0.0),
                high: high.get(i).unwrap_or(0.0),
                low: low.get(i).unwrap_or(0.0),
                close: close.get(i).unwrap_or(0.0),
                volume: volume.get(i).unwrap_or(0.0),

                quote_volume: quote.as_ref().and_then(|c| c.get(i)).unwrap_or(0.0),
                taker_buy_volume: taker_buy.as_ref().and_then(|c| c.get(i)).unwrap_or(0.0),
                taker_buy_quote_volume: taker_quote.as_ref().and_then(|c| c.get(i)).unwrap_or(0.0),
                trade_count: count.as_ref().and_then(|c| c.get(i)).unwrap_or(0),
            });
        }

        Ok(candles)
    }
    pub fn from_binance_rest(v: &[Value], symbol: Symbol) -> Option<Self> {
        if v.len() < 11 {
            return None;
        }

        let parse_f64 = |val: &Value| val.as_str()?.parse::<f64>().ok();

        let timestamp = v[0].as_i64()?;

        Some(Self {
            symbol,
            timestamp,
            open: parse_f64(&v[1])?,
            high: parse_f64(&v[2])?,
            low: parse_f64(&v[3])?,
            close: parse_f64(&v[4])?,
            volume: parse_f64(&v[5])?,

            quote_volume: parse_f64(&v[7]).unwrap_or(0.0),
            trade_count: v[8].as_i64().unwrap_or(0),
            taker_buy_volume: parse_f64(&v[9]).unwrap_or(0.0),
            taker_buy_quote_volume: parse_f64(&v[10]).unwrap_or(0.0),
        })
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema, Hash)]
pub enum Interval {
    M1,
    M5,
    M15,
    M30,
    H1,
    H4,
    D1,
}
impl Interval {
    pub fn all() -> Vec<Self> {
        vec![
            Self::M1,
            Self::M5,
            Self::M15,
            Self::M30,
            Self::H1,
            Self::H4,
            Self::D1,
        ]
    }
    pub fn duration(&self) -> Duration {
        match self {
            Interval::D1 => Duration::days(1),
            Interval::H4 => Duration::hours(4),
            Interval::H1 => Duration::hours(1),
            Interval::M30 => Duration::minutes(30),
            Interval::M15 => Duration::minutes(15),
            Interval::M5 => Duration::minutes(5),
            Interval::M1 => Duration::minutes(1),
        }
    }

    pub fn to_minutes(&self) -> i64 {
        match self {
            Interval::M1 => 1,
            Interval::M5 => 5,
            Interval::M15 => 15,
            Interval::M30 => 30,
            Interval::H1 => 60,
            Interval::H4 => 240,
            Interval::D1 => 1440,
        }
    }
    pub fn to_seconds(&self) -> i64 {
        self.to_minutes() * 60
    }
    pub fn to_millis(&self) -> i64 {
        self.to_seconds() * 1000
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Interval::M1 => "1m",
            Interval::M5 => "5m",
            Interval::M15 => "15m",
            Interval::M30 => "30m",
            Interval::H1 => "1h",
            Interval::H4 => "4h",
            Interval::D1 => "1d",
        }
    }
    pub fn as_sql_interval(&self) -> &'static str {
        match self {
            Interval::M1 => "1 minutes",
            Interval::M5 => "5 minutes",
            Interval::M15 => "15 minutes",
            Interval::M30 => "30 minutes",

            Interval::H1 => "1 hour",
            Interval::H4 => "4 hour",
            Interval::D1 => "1 day",
        }
    }
    pub fn view_name(&self) -> &'static str {
        match self {
            Interval::M1 => "candles_1m",
            Interval::M5 => "candles_5m",
            Interval::M15 => "candles_15m",
            Interval::M30 => "candles_30m",
            Interval::H1 => "candles_1h",
            Interval::H4 => "candles_4h",
            Interval::D1 => "candles_1d",
        }
    }
}
impl FromStr for Interval {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "1m" => Ok(Interval::M1),
            "5m" => Ok(Interval::M5),
            "15m" => Ok(Interval::M15),
            "30m" => Ok(Interval::M30),

            "1h" => Ok(Interval::H1),
            "4h" => Ok(Interval::H4),
            "1d" => Ok(Interval::D1),
            _ => Err(format!("invalid interval: {s}")),
        }
    }
}
impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenInterestRecord {
    pub symbol: Symbol,
    #[serde(deserialize_with = "str_to_f64")]
    pub sum_open_interest: f64,
    #[serde(deserialize_with = "str_to_f64")]
    pub sum_open_interest_value: f64,
    #[serde(deserialize_with = "str_to_i64")]
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BinanceOpenInterest {
    #[serde(deserialize_with = "str_to_f64")]
    pub open_interest: f64,
    pub symbol: Symbol,
    pub time: i64,
}
