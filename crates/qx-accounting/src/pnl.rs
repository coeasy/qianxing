//! Profit and loss calculation primitives.

#[derive(Debug, Clone, Default)]
pub struct PnL {
    pub realized: i64,
    pub unrealized: i64,
}

impl PnL {
    pub fn total(&self) -> i64 {
        self.realized + self.unrealized
    }

    pub fn update_unrealized(&mut self, value: i64) {
        self.unrealized = value;
    }
}
