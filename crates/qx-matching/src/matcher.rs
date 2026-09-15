//! Matching engine contract.

use crate::orderbook::{BookOrder, OrderBook};

#[derive(Debug, Clone)]
pub struct Fill {
    pub order_id: String,
    pub price: i64,
    pub quantity: u64,
}

#[derive(Debug, Clone, Default)]
pub struct MatchResult {
    pub fills: Vec<Fill>,
    pub remaining_quantity: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

pub trait Matcher<Order> {
    fn submit(&mut self, order: Order) -> MatchResult;
}

pub struct LimitMatcher {
    pub book: OrderBook,
}

impl LimitMatcher {
    pub fn new() -> Self {
        Self {
            book: OrderBook::new(),
        }
    }

    pub fn add_order(&mut self, side: Side, price: i64, order_id: String, quantity: u64) {
        let order = BookOrder { id: order_id, quantity };
        match side {
            Side::Buy => self.book.add_bid(price, order),
            Side::Sell => self.book.add_ask(price, order),
        }
    }
}
