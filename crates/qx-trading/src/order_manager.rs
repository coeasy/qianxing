//! Order lifecycle coordinator.

use std::collections::HashMap;

use crate::command::SubmitOrderCommand;
use crate::event::TradingEvent;

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

#[derive(Debug, Clone)]
pub struct ManagedOrder {
    pub order_id: String,
    pub status: OrderStatus,
}

impl ManagedOrder {
    pub fn submit(&mut self) -> bool {
        if self.status == OrderStatus::Created {
            self.status = OrderStatus::Submitted;
            return true;
        }
        false
    }

    pub fn accept(&mut self) -> bool {
        if self.status == OrderStatus::Submitted {
            self.status = OrderStatus::Accepted;
            return true;
        }
        false
    }

    pub fn fill(&mut self, partial: bool) -> bool {
        match self.status {
            OrderStatus::Accepted | OrderStatus::PartialFilled => {
                self.status = if partial {
                    OrderStatus::PartialFilled
                } else {
                    OrderStatus::Filled
                };
                true
            }
            _ => false,
        }
    }
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
        let mut order = ManagedOrder {
            order_id: command.order_id.clone(),
            status: OrderStatus::Created,
        };

        order.submit();
        self.orders.insert(command.order_id.clone(), order);

        TradingEvent::OrderSubmitted {
            order_id: command.order_id,
        }
    }

    pub fn count(&self) -> usize {
        self.orders.len()
    }
}
