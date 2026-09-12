use serde::{Deserialize, Serialize};

pub type EventId = u128;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainEvent {
    pub id: EventId,
    pub timestamp: u64,
    pub kind: String,
    pub source: String,
}
