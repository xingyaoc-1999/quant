use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use common::Symbol;
use quant::position::Position;

pub struct BinanceExecutor {
    api_key: String,
    secret_key: String,
    positions: Arc<Mutex<HashMap<Symbol, Position>>>,
}
