//! Event replay support.

#[derive(Debug, Default)]
pub struct ReplayEngine {
    sequence: u64,
}

impl ReplayEngine {
    pub fn new() -> Self {
        Self { sequence: 0 }
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn advance(&mut self) {
        self.sequence += 1;
    }
}
