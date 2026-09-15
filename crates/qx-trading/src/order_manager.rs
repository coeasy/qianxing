//! Order lifecycle coordinator.

use std::collections::HashMap;

use crate::command::SubmitOrderCommand;
use crate::event::TradingEvent;

#[derive(Debug, Clone)]
pub struct ManagedOrder {
    pub order_id: String,
    pub status: String,
}

#[derive(Debug, Default)]
pub struct OrderManager {
    orders: HashMap<String, ManagedOrder>,
}

impl OrderManager {
    pub fn new() -> Self {
        Self { orders: HashMap::new() }
    }

    pub fn submit(&mut self, command: SubmitOrderCommand) -> TradingEvent {
        let order = ManagedOrder {
            order_id: command.order_id.clone(),
            status: "Submitted".to_string(),
        };

        self.orders.insert(command.order_id.clone(), order);

        TradingEvent::OrderSubmitted {
            order_id: command.order_id,
        }
    }

    pub fn count(&self) -> usize {
        self.orders.len()
    }
}
