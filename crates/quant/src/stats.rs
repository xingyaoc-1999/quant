use std::collections::HashMap;

use common::Symbol;

#[derive(Default)]
pub struct SignalStats {
    pub signal_count: usize,
    pub reject_count: usize,
    pub update_count: usize,
    pub recent_rrs: Vec<f64>,
    pub reject_reasons: HashMap<Symbol, String>,
}

impl SignalStats {
    pub fn add_signal(&mut self, rr: f64) {
        self.signal_count += 1;
        self.recent_rrs.push(rr);
        if self.recent_rrs.len() > 50 {
            self.recent_rrs.remove(0);
        }
    }

    pub fn add_reject(&mut self, symbol: Symbol, reason: String) {
        self.reject_count += 1;
        self.reject_reasons.insert(symbol, reason);
    }

    pub fn add_update(&mut self) {
        self.update_count += 1;
    }

    pub fn avg_rr(&self) -> f64 {
        if self.recent_rrs.is_empty() {
            0.0
        } else {
            self.recent_rrs.iter().sum::<f64>() / self.recent_rrs.len() as f64
        }
    }
}
