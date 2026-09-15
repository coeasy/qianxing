use crate::catalog::DatasetManifest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 数据摄取只依赖“注册不可覆盖的 manifest”这一最小端口，内存和持久化
/// Registry 因此可以复用同一条摄取流程。
pub trait DatasetRegistrar {
    fn register_manifest(&mut self, manifest: DatasetManifest) -> Result<(), String>;
}

#[derive(Clone, Debug, Default)]
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

impl DatasetRegistrar for DatasetRegistry {
    fn register_manifest(&mut self, manifest: DatasetManifest) -> Result<(), String> {
        self.register(manifest)
    }
}

/// 单机持久化 DatasetRegistry。
///
/// `DatasetManifest` 是回测输入身份的一部分，不能只存在于一次 CLI 进程的
/// 内存中。该实现使用版本化 JSON、临时文件和原子替换，适用于 single_node
/// 研究/回测；分布式环境仍应由外部一致性存储实现同一语义。
#[derive(Clone, Debug)]
pub struct JsonDatasetRegistry {
    path: PathBuf,
    inner: DatasetRegistry,
}

#[derive(Serialize, Deserialize)]
struct RegistryDocument {
    schema_version: u32,
    manifests: Vec<DatasetManifest>,
}

impl JsonDatasetRegistry {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "create dataset registry directory {} failed: {error}",
                    parent.display()
                )
            })?;
        }
        let inner = Self::load_inner(&path)?;
        Ok(Self { path, inner })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn register(&mut self, manifest: DatasetManifest) -> Result<(), String> {
        manifest.validate()?;
        let lock_path = self.lock_path();
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|error| format!("acquire dataset registry lock failed: {error}"))?;
        let result = (|| {
            // 锁内重读，避免两个 CLI 进程都基于同一旧内存快照写入而互相覆盖。
            let mut current = Self::load_inner(&self.path)?;
            current.register(manifest)?;
            self.inner = current;
            self.persist_unlocked()
        })();
        drop(lock);
        let _ = std::fs::remove_file(&lock_path);
        result
    }

    pub fn resolve(&self, dataset_id: &str, version: &str) -> Option<&DatasetManifest> {
        self.inner.resolve(dataset_id, version)
    }

    pub fn verify(
        &self,
        dataset_id: &str,
        version: &str,
        fingerprint: &str,
    ) -> Result<&DatasetManifest, String> {
        self.inner.verify(dataset_id, version, fingerprint)
    }

    pub fn versions(&self, dataset_id: &str) -> Vec<&str> {
        self.inner.versions(dataset_id)
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn lock_path(&self) -> PathBuf {
        self.path.with_extension("json.lock")
    }

    fn load_inner(path: &Path) -> Result<DatasetRegistry, String> {
        let mut inner = DatasetRegistry::default();
        if !path.exists() {
            return Ok(inner);
        }
        let payload = std::fs::read_to_string(path)
            .map_err(|error| format!("read dataset registry {} failed: {error}", path.display()))?;
        let document: RegistryDocument = serde_json::from_str(&payload).map_err(|error| {
            format!("decode dataset registry {} failed: {error}", path.display())
        })?;
        if document.schema_version != 1 {
            return Err(format!(
                "unsupported dataset registry schema {}: {}",
                document.schema_version,
                path.display()
            ));
        }
        for manifest in document.manifests {
            inner.register(manifest)?;
        }
        Ok(inner)
    }

    fn persist_unlocked(&self) -> Result<(), String> {
        let mut manifests = self.inner.manifests.values().cloned().collect::<Vec<_>>();
        manifests.sort_by(|left, right| left.identity().cmp(&right.identity()));
        let payload = serde_json::to_vec_pretty(&RegistryDocument {
            schema_version: 1,
            manifests,
        })
        .map_err(|error| format!("encode dataset registry failed: {error}"))?;
        let temporary = self
            .path
            .with_extension(format!("json.tmp.{}", std::process::id()));
        std::fs::write(&temporary, payload).map_err(|error| {
            format!(
                "write dataset registry {} failed: {error}",
                temporary.display()
            )
        })?;
        if let Err(error) = std::fs::rename(&temporary, &self.path) {
            let _ = std::fs::remove_file(&self.path);
            std::fs::rename(&temporary, &self.path).map_err(|replacement| {
                format!(
                    "commit dataset registry {} failed: initial={error}; replacement={replacement}",
                    self.path.display()
                )
            })?;
        }
        Ok(())
    }
}

impl DatasetRegistrar for JsonDatasetRegistry {
    fn register_manifest(&mut self, manifest: DatasetManifest) -> Result<(), String> {
        self.register(manifest)
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
        assert!(registry
            .verify("bars.daily", "2026-09-12", "wrong")
            .is_err());
    }

    #[test]
    fn json_registry_survives_process_restart_and_rejects_conflicting_versions() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-dataset-registry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join("datasets.manifest.json");
        let mut registry = JsonDatasetRegistry::open(&path).unwrap();
        registry.register(manifest("fp1")).unwrap();
        registry.register(manifest("fp1")).unwrap();
        drop(registry);

        let mut reopened = JsonDatasetRegistry::open(&path).unwrap();
        assert_eq!(reopened.len(), 1);
        assert!(reopened.verify("bars.daily", "2026-09-12", "fp1").is_ok());
        assert!(reopened.register(manifest("fp2")).is_err());
        assert_eq!(reopened.len(), 1);
        assert!(!path.with_extension("json.tmp").exists());
        assert!(!path.with_extension("json.lock").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
