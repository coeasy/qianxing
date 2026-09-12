use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayCursor {
    pub sequence: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ReplayEngine {
    cursor: ReplayCursor,
}

impl Default for ReplayCursor {
    fn default() -> Self {
        Self { sequence: 0 }
    }
}

impl ReplayEngine {
    pub fn cursor(&self) -> &ReplayCursor {
        &self.cursor
    }

    pub fn advance(&mut self) {
        self.cursor.sequence += 1;
    }
}
