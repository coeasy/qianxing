use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunManifest {
    pub run_id: String,
    pub version: String,
    pub data_fingerprint: String,
}
