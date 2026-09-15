//! State snapshots.

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub sequence: u64,
    pub state_hash: String,
}

impl Snapshot {
    pub fn new(sequence: u64, state_hash: impl Into<String>) -> Self {
        Self {
            sequence,
            state_hash: state_hash.into(),
        }
    }
}
