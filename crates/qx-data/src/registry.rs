use crate::catalog::DatasetManifest;
use std::collections::BTreeMap;

#[derive(Default)]
pub struct DatasetRegistry {
    manifests: BTreeMap<(String, String), DatasetManifest>,
}

impl DatasetRegistry {
    pub fn register(&mut self, manifest: DatasetManifest) -> Result<(), String> {
        manifest.validate()?;
        let key = (manifest.dataset_id.clone(), manifest.version.clone());
        if let Some(existing) = self.manifests.get(&key) {
            if existing == &manifest {
                return Ok(());
            }
            return Err(format!(
                "dataset version already registered with different manifest: {}@{}",
                key.0, key.1
            ));
        }
        self.manifests.insert(key, manifest);
        Ok(())
    }

    pub fn resolve(&self, dataset_id: &str, version: &str) -> Option<&DatasetManifest> {
        self.manifests
            .get(&(dataset_id.to_string(), version.to_string()))
    }

    pub fn verify(
        &self,
        dataset_id: &str,
        version: &str,
        fingerprint: &str,
    ) -> Result<&DatasetManifest, String> {
        let manifest = self
            .resolve(dataset_id, version)
            .ok_or_else(|| format!("dataset not registered: {dataset_id}@{version}"))?;
        if manifest.fingerprint != fingerprint {
            return Err(format!(
                "dataset fingerprint mismatch for {dataset_id}@{version}: expected={} actual={fingerprint}",
                manifest.fingerprint
            ));
        }
        Ok(manifest)
    }

    pub fn versions(&self, dataset_id: &str) -> Vec<&str> {
        self.manifests
            .iter()
            .filter_map(|((id, version), _)| (id == dataset_id).then_some(version.as_str()))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.manifests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.manifests.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(fingerprint: &str) -> DatasetManifest {
        DatasetManifest {
            dataset_id: "bars.daily".into(),
            version: "2026-09-12".into(),
            source: "test".into(),
            fingerprint: fingerprint.into(),
            schema_version: 1,
            start_timestamp: 1,
            end_timestamp: 2,
        }
    }

    #[test]
    fn exact_duplicate_registration_is_idempotent() {
        let mut registry = DatasetRegistry::default();
        registry.register(manifest("fp1")).unwrap();
        registry.register(manifest("fp1")).unwrap();
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn conflicting_manifest_is_rejected() {
        let mut registry = DatasetRegistry::default();
        registry.register(manifest("fp1")).unwrap();
        assert!(registry.register(manifest("fp2")).is_err());
    }

    #[test]
    fn fingerprint_is_verified() {
        let mut registry = DatasetRegistry::default();
        registry.register(manifest("fp1")).unwrap();
        assert!(registry.verify("bars.daily", "2026-09-12", "fp1").is_ok());
        assert!(registry.verify("bars.daily", "2026-09-12", "wrong").is_err());
    }
}
