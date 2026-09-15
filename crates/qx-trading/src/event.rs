//! Trading facts emitted after command processing.

#[derive(Debug, Clone)]
pub enum TradingEvent {
    OrderSubmitted { order_id: String },
    OrderAccepted { order_id: String },
    OrderFilled { order_id: String, quantity: u64 },
    OrderCancelled { order_id: String },
}
