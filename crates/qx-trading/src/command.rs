//! Trading commands.

#[derive(Debug, Clone)]
pub struct SubmitOrderCommand {
    pub order_id: String,
    pub instrument_id: String,
    pub quantity: u64,
}

#[derive(Debug, Clone)]
pub struct CancelOrderCommand {
    pub order_id: String,
}
