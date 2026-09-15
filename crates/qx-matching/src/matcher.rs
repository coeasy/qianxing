//! Matching engine contract.

#[derive(Debug, Clone)]
pub struct MatchResult {
    pub filled_quantity: u64,
}

pub trait Matcher<Order> {
    fn submit(&mut self, order: Order) -> MatchResult;
}
