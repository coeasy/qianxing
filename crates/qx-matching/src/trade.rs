//! Matching result events.

#[derive(Debug, Clone)]
pub struct Fill {
    pub order_id: String,
    pub price: i64,
    pub quantity: u64,
}
