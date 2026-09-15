//! Runtime state storage.

use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct StateStore {
    values: HashMap<String, String>,
}

impl StateStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: String, value: String) {
        self.values.insert(key, value);
    }

    pub fn get(&self, key: &str) -> Option<&String> {
        self.values.get(key)
    }
}
