//! Matching engine implementation.

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
        Self { book: OrderBook::new() }
    }

    pub fn submit_limit(
        &mut self,
        side: Side,
        price: i64,
        order_id: String,
        mut quantity: u64,
    ) -> MatchResult {
        let mut result = MatchResult::default();

        match side {
            Side::Buy => {
                while quantity > 0 {
                    let Some((&ask_price, orders)) = self.book.asks.iter_mut().next() else { break };
                    if ask_price > price { break; }
                    if let Some(order) = orders.first_mut() {
                        let filled = quantity.min(order.quantity);
                        result.fills.push(Fill {
                            order_id: order.id.clone(),
                            price: ask_price,
                            quantity: filled,
                        });
                        quantity -= filled;
                        order.quantity -= filled;
                        if order.quantity == 0 { orders.remove(0); }
                    }
                    if orders.is_empty() { self.book.asks.remove(&ask_price); }
                }
            }
            Side::Sell => {
                while quantity > 0 {
                    let Some((&bid_price, orders)) = self.book.bids.iter_mut().next_back() else { break };
                    if bid_price < price { break; }
                    if let Some(order) = orders.first_mut() {
                        let filled = quantity.min(order.quantity);
                        result.fills.push(Fill {
                            order_id: order.id.clone(),
                            price: bid_price,
                            quantity: filled,
                        });
                        quantity -= filled;
                        order.quantity -= filled;
                        if order.quantity == 0 { orders.remove(0); }
                    }
                    if orders.is_empty() { self.book.bids.remove(&bid_price); }
                }
            }
        }

        if quantity > 0 {
            let order = BookOrder { id: order_id, quantity };
            match side {
                Side::Buy => self.book.add_bid(price, order),
                Side::Sell => self.book.add_ask(price, order),
            }
        }

        result.remaining_quantity = quantity;
        result
    }
}
