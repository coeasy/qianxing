use serde::{Deserialize, Serialize};

pub const DATA_SCHEMA_VERSION: u32 = 1;

pub type Timestamp = u64;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataSchemaVersion {
    pub major: u32,
}

impl Default for DataSchemaVersion {
    fn default() -> Self {
        Self {
            major: DATA_SCHEMA_VERSION,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bar {
    pub instrument: String,
    pub timestamp: Timestamp,
    pub open_raw: i128,
    pub high_raw: i128,
    pub low_raw: i128,
    pub close_raw: i128,
    pub volume_raw: i128,
}

impl Bar {
    pub fn validate(&self) -> Result<(), String> {
        if self.instrument.trim().is_empty() {
            return Err("missing instrument".into());
        }
        if self.timestamp == 0 {
            return Err("invalid timestamp".into());
        }
        Ok(())
    }
}
