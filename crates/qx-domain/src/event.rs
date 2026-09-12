use serde::{Deserialize, Serialize};

pub type EventId = u128;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainEvent {
    pub id: EventId,
    pub timestamp: u64,
    pub kind: String,
    pub source: String,
}

impl DomainEvent {
    pub fn validate(&self) -> Result<(), String> {
        if self.id == 0 {
            return Err("event id must be non-zero".into());
        }
        if self.timestamp == 0 {
            return Err("event timestamp must be non-zero".into());
        }
        if self.kind.trim().is_empty() || self.source.trim().is_empty() {
            return Err("event kind and source are required".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_requires_causal_identity() {
        let event = DomainEvent {
            id: 1,
            timestamp: 1,
            kind: "market.bar".into(),
            source: "qx-data".into(),
        };
        assert!(event.validate().is_ok());
    }
}
