use chrono::Timelike;
use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TradingSession {
    #[default]
    Asian,
    European,
    American,
    Weekend,
}

impl TradingSession {
    pub fn as_str(&self) -> &'static str {
        match self {
            TradingSession::Asian => "Asia",
            TradingSession::European => "Europe",
            TradingSession::American => "America",
            TradingSession::Weekend => "Weekend",
        }
    }
    pub fn from_timestamp(timestamp_ms: i64) -> Self {
        let datetime =
            DateTime::<Utc>::from_timestamp_millis(timestamp_ms).unwrap_or_else(|| Utc::now());

        let weekday = datetime.weekday().number_from_monday();
        if weekday >= 6 {
            return TradingSession::Weekend;
        }

        match datetime.hour() {
            0..=7 => TradingSession::Asian,
            8..=12 => TradingSession::European,
            _ => TradingSession::American,
        }
    }

    pub fn factor(&self, config: &crate::config::SessionConfig) -> f64 {
        match self {
            TradingSession::Asian => config.asian_factor,
            TradingSession::European => config.european_factor,
            TradingSession::American => config.american_factor,
            TradingSession::Weekend => config.weekend_factor,
        }
    }
}
