use crate::catalog::DatasetManifest;
use crate::registry::DatasetRegistry;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatasetRef {
    pub dataset_id: String,
    pub version: String,
    pub fingerprint: String,
}

impl DatasetRef {
    pub fn new(
        dataset_id: impl Into<String>,
        version: impl Into<String>,
        fingerprint: impl Into<String>,
    ) -> Result<Self, String> {
        let value = Self {
            dataset_id: dataset_id.into(),
            version: version.into(),
            fingerprint: fingerprint.into(),
        };
        if value.dataset_id.trim().is_empty()
            || value.version.trim().is_empty()
            || value.fingerprint.trim().is_empty()
        {
            return Err("dataset reference requires id, version and fingerprint".into());
        }
        Ok(value)
    }
}

pub trait DatasetResolver {
    fn resolve(&self, reference: &DatasetRef) -> Result<DatasetManifest, String>;
}

impl DatasetResolver for DatasetRegistry {
    fn resolve(&self, reference: &DatasetRef) -> Result<DatasetManifest, String> {
        self.verify(
            &reference.dataset_id,
            &reference.version,
            &reference.fingerprint,
        )
        .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolution_locks_fingerprint() {
        let mut registry = DatasetRegistry::default();
        registry
            .register(DatasetManifest {
                dataset_id: "bars.daily".into(),
                version: "v1".into(),
                source: "test".into(),
                fingerprint: "abc".into(),
                schema_version: 1,
                start_timestamp: 1,
                end_timestamp: 2,
            })
            .unwrap();

        let reference = DatasetRef::new("bars.daily", "v1", "abc").unwrap();
        let resolved = DatasetResolver::resolve(&registry, &reference).unwrap();
        assert_eq!(resolved.fingerprint, "abc");
    }
}
