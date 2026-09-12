use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Position {
    pub instrument: String,
    pub quantity: i128,
}

impl Position {
    pub fn validate(&self) -> Result<(), String> {
        if self.instrument.trim().is_empty() {
            return Err("position instrument is required".into());
        }
        Ok(())
    }
}
