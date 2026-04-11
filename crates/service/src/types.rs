use common::Symbol;

#[derive(Debug, Clone)]
pub enum MarketEvent {
    KlineClosed { symbol: Symbol },
}
