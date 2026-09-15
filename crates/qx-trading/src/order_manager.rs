//! Order lifecycle coordinator.

#[derive(Debug, Default)]
pub struct OrderManager {
    pending: usize,
}

impl OrderManager {
    pub fn new() -> Self {
        Self { pending: 0 }
    }

    pub fn pending_count(&self) -> usize {
        self.pending
    }
}
