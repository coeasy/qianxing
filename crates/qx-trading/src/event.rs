//! Trading facts emitted after command processing.

#[derive(Debug, Clone)]
pub struct OrderSubmitted {
    pub order_id: String,
}

#[derive(Debug, Clone)]
pub struct OrderAccepted {
    pub order_id: String,
}

#[derive(Debug, Clone)]
pub struct OrderFilled {
    pub order_id: String,
    pub quantity: u64,
}
