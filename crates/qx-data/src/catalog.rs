use qx_core::Fnv1a;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetComponentFormat {
    #[default]
    Json,
    Arrow,
}

fn is_json_component_format(format: &DatasetComponentFormat) -> bool {
    matches!(format, DatasetComponentFormat::Json)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrowFieldManifest {
    pub name: String,
    pub format: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrowDatasetManifest {
    pub format: String,
    pub kind: String,
    #[serde(flatten)]
    pub dataset: DatasetManifest,
    pub row_count: u64,
    #[serde(default)]
    pub schema: Vec<ArrowFieldManifest>,
}

impl ArrowDatasetManifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.format != "arrow" {
            return Err("Arrow dataset manifest format must be arrow".into());
        }
        if self.kind.trim().is_empty() {
            return Err("Arrow dataset manifest kind is required".into());
        }
        if self.row_count == 0 {
            return Err("Arrow dataset manifest row_count must be greater than zero".into());
        }
        if self.schema.is_empty() {
            return Err("Arrow dataset manifest schema is required".into());
        }
        let mut names = std::collections::BTreeSet::new();
        for field in &self.schema {
            if field.name.trim().is_empty() || field.format.trim().is_empty() {
                return Err("Arrow dataset manifest fields require name and format".into());
            }
            if !names.insert(&field.name) {
                return Err(format!(
                    "Arrow dataset manifest contains duplicate field: {}",
                    field.name
                ));
            }
        }
        self.dataset.validate()
    }

    pub fn from_json(payload: &str) -> Result<Self, String> {
        let manifest: Self = serde_json::from_str(payload)
            .map_err(|error| format!("decode Arrow dataset manifest failed: {error}"))?;
        manifest.validate()?;
        Ok(manifest)
    }
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

/// 一个研究/回测运行实际使用的数据组件。组件沿用普通 `DatasetManifest`
/// 的来源、版本、指纹和时间范围，同时声明自己的语义类型，避免把行情、
/// 公司行为或交易日历当成一份无类型的 JSON 文件。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetComponentManifest {
    pub kind: String,
    #[serde(default, skip_serializing_if = "is_json_component_format")]
    pub format: DatasetComponentFormat,
    #[serde(flatten)]
    pub dataset: DatasetManifest,
    pub row_count: u64,
}

impl DatasetComponentManifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.kind.trim().is_empty() {
            return Err("dataset component kind is required".into());
        }
        if self.row_count == 0 {
            return Err(format!(
                "dataset component {} must contain at least one row",
                self.kind
            ));
        }
        if matches!(self.format, DatasetComponentFormat::Arrow) {
            // Arrow 的 schema/IPC 内容由 ArrowDatasetManifest 在输入边界校验；
            // Bundle 本身只保存类型化的 dataset identity 和 fingerprint。
            if self.dataset.source.trim().is_empty() {
                return Err("Arrow dataset component source is required".into());
            }
        }
        self.dataset.validate()
    }
}

/// 绑定同一研究快照的 Bars、CorporateActions、Calendar、Suspension 和
/// LimitRules。`qx-data` 不要求这些组件来自同一个供应商，但每个组件都必须
/// 有自己的版本和指纹；最终 bundle 指纹由有序组件清单确定性生成。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetBundleManifest {
    pub bundle_id: String,
    pub version: String,
    pub source: String,
    pub schema_version: u32,
    pub components: BTreeMap<String, DatasetComponentManifest>,
}

impl DatasetBundleManifest {
    pub fn new(
        bundle_id: impl Into<String>,
        version: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            bundle_id: bundle_id.into(),
            version: version.into(),
            source: source.into(),
            schema_version: 1,
            components: BTreeMap::new(),
        }
    }

    pub fn add_component(&mut self, component: DatasetComponentManifest) -> Result<(), String> {
        component.validate()?;
        let kind = component.kind.clone();
        match self.components.entry(kind.clone()) {
            std::collections::btree_map::Entry::Occupied(_) => {
                Err(format!("dataset bundle component already exists: {kind}"))
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(component);
                Ok(())
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.bundle_id.trim().is_empty()
            || self.version.trim().is_empty()
            || self.source.trim().is_empty()
        {
            return Err("dataset bundle identity and source are required".into());
        }
        if self.schema_version == 0 || self.components.is_empty() {
            return Err("dataset bundle schema and components are required".into());
        }
        if !self.components.contains_key("bars") {
            return Err("dataset bundle must contain a bars component".into());
        }
        for (kind, component) in &self.components {
            if kind != &component.kind {
                return Err(format!(
                    "dataset bundle component key mismatch: key={kind} kind={}",
                    component.kind
                ));
            }
            component.validate()?;
        }
        Ok(())
    }

    pub fn component(&self, kind: &str) -> Option<&DatasetComponentManifest> {
        self.components.get(kind)
    }

    pub fn fingerprint(&self) -> Result<String, String> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("encode dataset bundle for fingerprint failed: {error}"))?;
        let mut hash = Fnv1a::new();
        hash.write_bytes(&bytes);
        Ok(format!("{:016x}", hash.finish()))
    }
}

/// 单机 DatasetBundle 持久化。Bundle 只保存组件身份和 fingerprint，组件数据
/// 仍由各自 DataStorage 管理；这样不会把一个 JSON 清单误当成行情事实存储。
#[derive(Clone, Debug)]
pub struct JsonDatasetBundleStore {
    root: PathBuf,
}

impl JsonDatasetBundleStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, String> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .map_err(|error| format!("create dataset bundle root failed: {error}"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, bundle_id: &str, version: &str) -> Result<PathBuf, String> {
        if bundle_id.trim().is_empty()
            || version.trim().is_empty()
            || bundle_id.contains('/')
            || bundle_id.contains('\\')
            || bundle_id.contains("..")
            || version.contains('/')
            || version.contains('\\')
            || version.contains("..")
        {
            return Err("dataset bundle id/version contains an unsafe path component".into());
        }
        Ok(self
            .root
            .join(format!("{bundle_id}--{version}.bundle.json")))
    }

    pub fn save(&self, bundle: &DatasetBundleManifest) -> Result<String, String> {
        bundle.validate()?;
        let path = self.path_for(&bundle.bundle_id, &bundle.version)?;
        if path.exists() {
            let existing = self.load(&bundle.bundle_id, &bundle.version)?;
            if existing == *bundle {
                return bundle.fingerprint();
            }
            return Err(format!(
                "dataset bundle version already registered with different manifest: {}@{}",
                bundle.bundle_id, bundle.version
            ));
        }
        let payload = serde_json::to_vec_pretty(bundle)
            .map_err(|error| format!("encode dataset bundle failed: {error}"))?;
        let temporary = path.with_extension(format!("bundle.json.tmp.{}", std::process::id()));
        std::fs::write(&temporary, payload).map_err(|error| {
            format!(
                "write dataset bundle {} failed: {error}",
                temporary.display()
            )
        })?;
        if let Err(error) = std::fs::rename(&temporary, &path) {
            let _ = std::fs::remove_file(&path);
            std::fs::rename(&temporary, &path).map_err(|replacement| {
                format!(
                    "commit dataset bundle {} failed: initial={error}; replacement={replacement}",
                    path.display()
                )
            })?;
        }
        bundle.fingerprint()
    }

    pub fn load(&self, bundle_id: &str, version: &str) -> Result<DatasetBundleManifest, String> {
        let path = self.path_for(bundle_id, version)?;
        let payload = std::fs::read_to_string(&path)
            .map_err(|error| format!("read dataset bundle {} failed: {error}", path.display()))?;
        let bundle: DatasetBundleManifest = serde_json::from_str(&payload)
            .map_err(|error| format!("decode dataset bundle {} failed: {error}", path.display()))?;
        bundle.validate()?;
        Ok(bundle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(kind: &str, dataset_id: &str) -> DatasetComponentManifest {
        DatasetComponentManifest {
            kind: kind.into(),
            format: DatasetComponentFormat::Json,
            dataset: DatasetManifest {
                dataset_id: dataset_id.into(),
                version: "snapshot-1".into(),
                source: "test".into(),
                fingerprint: format!("{kind}-fp"),
                schema_version: 1,
                start_timestamp: 1,
                end_timestamp: 2,
            },
            row_count: 2,
        }
    }

    #[test]
    fn bundle_binds_typed_components_and_has_stable_fingerprint() {
        let mut bundle = DatasetBundleManifest::new("ashare.000001", "run-1", "mixed");
        bundle
            .add_component(component("bars", "bars.daily"))
            .unwrap();
        bundle
            .add_component(component("corporate_actions", "actions.daily"))
            .unwrap();
        bundle.validate().unwrap();
        let fingerprint = bundle.fingerprint().unwrap();
        assert_eq!(fingerprint, bundle.fingerprint().unwrap());
        assert!(bundle.component("corporate_actions").is_some());
        assert!(bundle.add_component(component("bars", "other")).is_err());
    }

    #[test]
    fn bundle_rejects_untyped_or_empty_components() {
        let mut bundle = DatasetBundleManifest::new("ashare.000001", "run-1", "mixed");
        assert!(bundle
            .add_component(DatasetComponentManifest {
                kind: "bars".into(),
                format: DatasetComponentFormat::Json,
                dataset: DatasetManifest {
                    dataset_id: "bars".into(),
                    version: "v1".into(),
                    source: "test".into(),
                    fingerprint: "fp".into(),
                    schema_version: 1,
                    start_timestamp: 1,
                    end_timestamp: 2,
                },
                row_count: 0,
            })
            .is_err());
        assert!(bundle.validate().is_err());
    }

    #[test]
    fn arrow_dataset_manifest_validates_schema_and_round_trips() {
        let manifest = ArrowDatasetManifest {
            format: "arrow".into(),
            kind: "factors".into(),
            dataset: DatasetManifest {
                dataset_id: "factors.daily".into(),
                version: "arrow-v1".into(),
                source: "python-factor-provider".into(),
                fingerprint: "sha256-factor-data".into(),
                schema_version: 1,
                start_timestamp: 1,
                end_timestamp: 2,
            },
            row_count: 2,
            schema: vec![
                ArrowFieldManifest {
                    name: "instrument".into(),
                    format: "utf8".into(),
                },
                ArrowFieldManifest {
                    name: "value_raw".into(),
                    format: "decimal128(38,9)".into(),
                },
            ],
        };
        manifest.validate().unwrap();
        let restored =
            ArrowDatasetManifest::from_json(&serde_json::to_string(&manifest).unwrap()).unwrap();
        assert_eq!(restored, manifest);
        let mut invalid = manifest;
        invalid.schema[1].name = invalid.schema[0].name.clone();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn json_bundle_store_round_trips_and_returns_stable_fingerprint() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-dataset-bundle-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut bundle = DatasetBundleManifest::new("ashare.000001", "run-1", "mixed");
        bundle
            .add_component(component("bars", "bars.daily"))
            .unwrap();
        bundle
            .add_component(component("calendar", "calendar.cn"))
            .unwrap();
        let store = JsonDatasetBundleStore::new(&root).unwrap();
        let fingerprint = store.save(&bundle).unwrap();
        let restored = store.load("ashare.000001", "run-1").unwrap();
        assert_eq!(restored, bundle);
        assert_eq!(restored.fingerprint().unwrap(), fingerprint);
        let mut conflicting = bundle.clone();
        conflicting
            .components
            .get_mut("bars")
            .unwrap()
            .dataset
            .fingerprint = "other".into();
        assert!(store.save(&conflicting).is_err());
        let mut next_version = bundle.clone();
        next_version.version = "run-2".into();
        assert!(store.save(&next_version).is_ok());
        assert!(store.load("../escape", "run-1").is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
