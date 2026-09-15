use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub u128);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope<T> {
    pub id: EventId,
    pub timestamp: u64,
    pub sequence: u64,
    pub payload: T,
}

impl<T> EventEnvelope<T> {
    pub fn new(id: EventId, timestamp: u64, sequence: u64, payload: T) -> Self {
        Self {
            id,
            timestamp,
            sequence,
            payload,
        }
    }
}
