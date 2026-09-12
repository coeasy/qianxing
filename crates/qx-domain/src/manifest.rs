use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunManifest {
    pub run_id: String,
    pub version: String,
    pub data_fingerprint: String,
}

impl RunManifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.run_id.trim().is_empty()
            || self.version.trim().is_empty()
            || self.data_fingerprint.trim().is_empty()
        {
            return Err("run manifest requires run_id, version and data_fingerprint".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_manifest_requires_reproducible_data_identity() {
        let manifest = RunManifest {
            run_id: "run-1".into(),
            version: "v2".into(),
            data_fingerprint: "".into(),
        };
        assert!(manifest.validate().is_err());
    }
}
