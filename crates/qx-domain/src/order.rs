use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderStatus {
    New,
    Accepted,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
}

impl OrderStatus {
    pub fn terminal(&self) -> bool {
        matches!(self, Self::Filled | Self::Cancelled | Self::Rejected)
    }

    pub fn can_transition_to(&self, next: &Self) -> bool {
        matches!(
            (self, next),
            (Self::New, Self::Accepted)
                | (Self::New, Self::Cancelled)
                | (Self::New, Self::Rejected)
                | (Self::Accepted, Self::PartiallyFilled)
                | (Self::Accepted, Self::Filled)
                | (Self::Accepted, Self::Cancelled)
                | (Self::Accepted, Self::Rejected)
                | (Self::PartiallyFilled, Self::PartiallyFilled)
                | (Self::PartiallyFilled, Self::Filled)
                | (Self::PartiallyFilled, Self::Cancelled)
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Order {
    pub id: String,
    pub instrument: String,
    pub side: OrderSide,
    pub quantity: i128,
    pub status: OrderStatus,
}

impl Order {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() || self.instrument.trim().is_empty() {
            return Err("order identity is required".into());
        }
        if self.quantity <= 0 {
            return Err("order quantity must be positive".into());
        }
        Ok(())
    }

    pub fn transition(&mut self, next: OrderStatus) -> Result<(), String> {
        if !self.status.can_transition_to(&next) {
            return Err(format!(
                "invalid order transition: {:?} -> {:?}",
                self.status, next
            ));
        }
        self.status = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order() -> Order {
        Order {
            id: "o1".into(),
            instrument: "XSHG:600000".into(),
            side: OrderSide::Buy,
            quantity: 100,
            status: OrderStatus::New,
        }
    }

    #[test]
    fn order_lifecycle_rejects_terminal_reentry() {
        let mut order = order();
        order.transition(OrderStatus::Accepted).unwrap();
        order.transition(OrderStatus::Filled).unwrap();
        assert!(order.transition(OrderStatus::Cancelled).is_err());
    }
}
