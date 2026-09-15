//! Price-time priority order book.

use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct BookOrder {
    pub id: String,
    pub quantity: u64,
}

#[derive(Debug, Default)]
pub struct OrderBook {
    // descending bid prices are handled by matcher logic.
    pub bids: BTreeMap<i64, Vec<BookOrder>>,
    pub asks: BTreeMap<i64, Vec<BookOrder>>,
}

impl OrderBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_bid(&mut self, price: i64, order: BookOrder) {
        self.bids.entry(price).or_default().push(order);
    }

    pub fn add_ask(&mut self, price: i64, order: BookOrder) {
        self.asks.entry(price).or_default().push(order);
    }
}
