use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
        if self.dataset_id.trim().is_empty() || self.version.trim().is_empty() {
            return Err("invalid dataset identity".into());
        }
        if self.source.trim().is_empty() || self.fingerprint.trim().is_empty() {
            return Err("dataset source and fingerprint are required".into());
        }
        if self.schema_version == 0 {
            return Err("schema_version must be greater than zero".into());
        }
        if self.start_timestamp == 0 || self.start_timestamp > self.end_timestamp {
            return Err("invalid dataset range".into());
        }
        Ok(())
    }

    pub fn identity(&self) -> (&str, &str) {
        (&self.dataset_id, &self.version)
    }
}
