use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Trade {
    pub id: String,
    pub order_id: String,
    pub instrument: String,
    pub timestamp: u64,
    pub quantity: i128,
    pub price_raw: i128,
    pub fee_raw: i128,
}

impl Trade {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty()
            || self.order_id.trim().is_empty()
            || self.instrument.trim().is_empty()
        {
            return Err("trade identity is required".into());
        }
        if self.timestamp == 0 || self.quantity <= 0 || self.price_raw <= 0 || self.fee_raw < 0 {
            return Err("trade timestamp, quantity, price or fee is invalid".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trade_rejects_negative_fee() {
        let trade = Trade {
            id: "t1".into(),
            order_id: "o1".into(),
            instrument: "XSHG:600000".into(),
            timestamp: 1,
            quantity: 100,
            price_raw: 10,
            fee_raw: -1,
        };
        assert!(trade.validate().is_err());
    }
}
