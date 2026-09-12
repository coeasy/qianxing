use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DatasetManifest {
    pub dataset_id: String,
    pub version: String,
    pub source: String,
    pub fingerprint: String,
    pub schema_version: u32,
    pub start_timestamp: u64,
    pub end_timestamp: u64,
}

impl DatasetManifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.dataset_id.is_empty() || self.version.is_empty() {
            return Err("invalid dataset identity".into());
        }
        if self.start_timestamp > self.end_timestamp {
            return Err("invalid dataset range".into());
        }
        Ok(())
    }
}
