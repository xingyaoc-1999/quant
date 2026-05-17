use crate::risk_manager::RiskManager;
use crate::types::gravity::PriceGravityWell;
use crate::types::market::TradeDirection;

#[derive(Debug, Clone)]
pub struct TrailingStop {
    direction: TradeDirection,
    entry_price: f64,
    best_price: f64,
    stop_price: f64,
    trail_mult: f64,
    protection_bars_remaining: usize,

    breakeven_activated: bool,
}

impl TrailingStop {
    pub fn new(
        direction: TradeDirection,
        entry_price: f64,
        initial_stop: f64,
        trail_mult: f64,
        protection_bars: usize,
    ) -> Self {
        Self {
            direction,
            entry_price,
            best_price: entry_price,
            stop_price: initial_stop,
            trail_mult,
            protection_bars_remaining: protection_bars,
            breakeven_activated: false,
        }
    }

    pub fn update(&mut self, current_price: f64, atr: f64) -> Option<f64> {
        if self.protection_bars_remaining > 0 {
            self.protection_bars_remaining -= 1;
            return None;
        }
        let old_stop = self.stop_price;

        match self.direction {
            TradeDirection::Long => {
                if current_price > self.best_price {
                    self.best_price = current_price;
                }

                if !self.breakeven_activated && current_price >= self.entry_price + 1.5 * atr {
                    self.breakeven_activated = true;
                    self.stop_price = self.entry_price;
                }

                if self.breakeven_activated {
                    let trailing_stop = self.best_price - self.trail_mult * atr;
                    if trailing_stop > self.stop_price {
                        self.stop_price = trailing_stop;
                    }
                }
            }
            TradeDirection::Short => {
                if current_price < self.best_price {
                    self.best_price = current_price;
                }

                if !self.breakeven_activated && current_price <= self.entry_price - 1.5 * atr {
                    self.breakeven_activated = true;
                    self.stop_price = self.entry_price;
                }

                if self.breakeven_activated {
                    let trailing_stop = self.best_price + self.trail_mult * atr;
                    if trailing_stop < self.stop_price {
                        self.stop_price = trailing_stop;
                    }
                }
            }
        }

        if (self.stop_price - old_stop).abs() > f64::EPSILON {
            Some(self.stop_price)
        } else {
            None
        }
    }

    pub fn current_stop(&self) -> f64 {
        self.stop_price
    }
}

pub fn refresh_take_profits(
    risk_mgr: &RiskManager,
    wells: &[PriceGravityWell],
    last_price: f64,
    atr_v: f64,
    average_atr: f64,
    is_long: bool,
    is_tsunami: bool,
    vol_p: f64,
    current_tps: &[f64; 2],
) -> Option<([f64; 2], [f64; 2])> {
    let mut tags = Vec::new();
    let (_sl, new_tp, tp_alloc, _sl_alloc) = risk_mgr.calculate_trade_structure(
        wells,
        last_price,
        atr_v,
        average_atr,
        is_long,
        is_tsunami,
        vol_p,
        &mut tags,
    );

    let new_tp_array = [new_tp[0], new_tp[1]];
    let current_tp1 = current_tps[0];
    let current_tp2 = current_tps[1];
    let new_tp1 = new_tp_array[0];
    let new_tp2 = new_tp_array[1];

    // 检查是否都更优（做多时更高，做空时更低）
    let tp1_better = (is_long && new_tp1 > current_tp1) || (!is_long && new_tp1 < current_tp1);
    let tp2_better = (is_long && new_tp2 > current_tp2) || (!is_long && new_tp2 < current_tp2);

    if !tp1_better || !tp2_better {
        return None;
    }

    let min_move_pct = 0.002;
    let move_ratio1 = (new_tp1 - current_tp1).abs() / current_tp1;
    let move_ratio2 = (new_tp2 - current_tp2).abs() / current_tp2;

    if move_ratio1 < min_move_pct && move_ratio2 < min_move_pct {
        return None;
    }

    Some((new_tp_array, tp_alloc))
}