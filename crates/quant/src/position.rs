use crate::trailing::TrailingStop;
use crate::types::market::TradeDirection;

#[derive(Debug, Clone)]
pub struct Position {
    pub direction: TradeDirection,
    pub entry_price: f64,
    pub stop_loss: f64,
    pub take_profit1: f64,
    pub take_profit2: f64,
    pub trailing_stop: Option<TrailingStop>,
}

impl Position {
    pub fn is_long(&self) -> bool {
        self.direction == TradeDirection::Long
    }
}
