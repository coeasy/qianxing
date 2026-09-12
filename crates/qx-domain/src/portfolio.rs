use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Portfolio {
    pub id: String,
    pub positions: BTreeMap<String, i128>,
}

impl Portfolio {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("portfolio id is required".into());
        }
        if self
            .positions
            .keys()
            .any(|instrument| instrument.trim().is_empty())
        {
            return Err("portfolio contains an empty instrument id".into());
        }
        Ok(())
    }

    pub fn position_raw(&self, instrument: &str) -> i128 {
        self.positions.get(instrument).copied().unwrap_or_default()
    }
}
