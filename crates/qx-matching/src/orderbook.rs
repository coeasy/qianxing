//! Price-time priority order book.

#[derive(Debug, Clone)]
pub struct PriceLevel {
    pub price: i64,
    pub quantity: u64,
}

#[derive(Debug, Default)]
pub struct OrderBook {
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
}

impl OrderBook {
    pub fn new() -> Self {
        Self::default()
    }
}
