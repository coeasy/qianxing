use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeEvent<T> {
    pub sequence: u64,
    pub payload: T,
}

#[derive(Clone, Debug, Default)]
pub struct EventBus<T> {
    next_sequence: u64,
    queue: VecDeque<RuntimeEvent<T>>,
}

impl<T> EventBus<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn publish(&mut self, payload: T) -> u64 {
        self.next_sequence += 1;
        let sequence = self.next_sequence;
        self.queue.push_back(RuntimeEvent { sequence, payload });
        sequence
    }

    pub fn consume(&mut self) -> Option<RuntimeEvent<T>> {
        self.queue.pop_front()
    }

    pub fn pending(&self) -> usize {
        self.queue.len()
    }
}
