//! Qianxing V5 canonical order model.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    Created,
    Submitted,
    Accepted,
    PartialFilled,
    Filled,
    Cancelled,
    Rejected,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Order {
    pub id: u64,
    pub instrument_id: String,
    pub side: OrderSide,
    pub quantity: i64,
    pub price: i64,
    pub status: OrderStatus,
}

impl Order {
    pub fn new(
        id: u64,
        instrument_id: impl Into<String>,
        side: OrderSide,
        quantity: i64,
        price: i64,
    ) -> Self {
        Self {
            id,
            instrument_id: instrument_id.into(),
            side,
            quantity,
            price,
            status: OrderStatus::Created,
        }
    }

    pub fn submit(&mut self) {
        self.status = OrderStatus::Submitted;
    }
}
