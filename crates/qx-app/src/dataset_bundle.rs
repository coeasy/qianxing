//! DatasetBundle component identity and configured-input binding rules.

use crate::{AppError, AppErrorCategory};
use qx_data::{
    ArrowDatasetManifest, DatasetBundleManifest, DatasetComponentFormat, DatasetComponentManifest,
    DatasetManifest,
};
use std::collections::BTreeMap;
use std::path::Path;

/// Runtime-independent paths used to resolve non-Bar components of a dataset bundle.
pub struct DatasetBundleComponentBindings<'a> {
    pub strategy_id: &'a str,
    pub component_paths: &'a BTreeMap<String, String>,
    pub ashare_actions_path: Option<&'a str>,
    pub ashare_calendar_path: Option<&'a str>,
}

/// Assemble and validate a bundle from already identified component manifests.
/// Component identity calculation stays with the producer; ownership of bundle
/// composition and validation stays in the shared Rust application layer.
pub fn assemble_dataset_bundle(
    bundle_id: impl Into<String>,
    version: impl Into<String>,
    source: impl Into<String>,
    components: impl IntoIterator<Item = DatasetComponentManifest>,
) -> Result<DatasetBundleManifest, String> {
    let mut bundle = DatasetBundleManifest::new(bundle_id, version, source);
    for component in components {
        bundle.add_component(component)?;
    }
    bundle.validate()?;
    Ok(bundle)
}

/// Verify that the bundle's Bar component matches the selected ingested dataset.
pub fn verify_dataset_bundle_bars(
    bundle: &DatasetBundleManifest,
    bars_manifest: &DatasetManifest,
    bars_len: usize,
) -> Result<String, AppError> {
    bundle.validate().map_err(|error| {
        invalid_component(format!("策略 DatasetBundleManifest 校验失败: {error}"))
    })?;
    let bars = bundle
        .component("bars")
        .ok_or_else(|| invalid_component("策略 DatasetBundleManifest 缺少 bars 组件"))?;
    if bars.dataset.fingerprint != bars_manifest.fingerprint {
        return Err(invalid_component(format!(
            "策略 DatasetBundleManifest bars fingerprint 不匹配: bundle={} input={}",
            bars.dataset.fingerprint, bars_manifest.fingerprint
        )));
    }
    if bars.row_count != bars_len as u64 {
        return Err(invalid_component(format!(
            "策略 DatasetBundleManifest bars row_count 不匹配: bundle={} input={bars_len}",
            bars.row_count
        )));
    }
    bundle
        .fingerprint()
        .map_err(|error| invalid_component(error.to_string()))
}

/// Recompute each configured component's identity and verify its fingerprint and row count.
pub fn verify_dataset_bundle_components(
    bundle: &DatasetBundleManifest,
    bindings: &DatasetBundleComponentBindings<'_>,
    mut read_component: impl FnMut(&Path, &str, DatasetComponentFormat) -> Result<String, AppError>,
) -> Result<(), AppError> {
    for (kind, component) in &bundle.components {
        if kind == "bars" {
            continue;
        }
        let configured_path = bindings
            .component_paths
            .get(kind)
            .map(String::as_str)
            .or(match kind.as_str() {
                "corporate_actions" => bindings.ashare_actions_path,
                "calendar" => bindings.ashare_calendar_path,
                _ => None,
            })
            .ok_or_else(|| {
                invalid_component(format!(
                    "策略 {} 的 DatasetBundle 组件 {kind} 没有对应输入文件；请配置 dataset_component_paths.{kind}",
                    bindings.strategy_id
                ))
            })?;
        let payload = read_component(Path::new(configured_path), kind, component.format)?;
        let (fingerprint, row_count) = match component.format {
            DatasetComponentFormat::Json => {
                dataset_component_fingerprint_from_json(&payload, kind, Path::new(configured_path))?
            }
            DatasetComponentFormat::Arrow => arrow_dataset_manifest_fingerprint_from_json(
                &payload,
                kind,
                Path::new(configured_path),
            )?,
        };
        if fingerprint != component.dataset.fingerprint {
            return Err(invalid_component(format!(
                "策略 {} 的 DatasetBundle 组件 {kind} 指纹不匹配: bundle={} input={fingerprint}",
                bindings.strategy_id, component.dataset.fingerprint
            )));
        }
        if row_count != component.row_count {
            return Err(invalid_component(format!(
                "策略 {} 的 DatasetBundle 组件 {kind} 行数不匹配: bundle={} input={row_count}",
                bindings.strategy_id, component.row_count
            )));
        }
    }
    Ok(())
}

/// Verify an Arrow component's small producer-authored manifest JSON.
pub fn arrow_dataset_manifest_fingerprint_from_json(
    payload: &str,
    kind: &str,
    source_path: &Path,
) -> Result<(String, u64), AppError> {
    let manifest = ArrowDatasetManifest::from_json(payload).map_err(|error| {
        invalid_component(format!(
            "Arrow DatasetBundle 组件 manifest 非法 {}: {error}",
            source_path.display()
        ))
    })?;
    if manifest.kind != kind {
        return Err(invalid_component(format!(
            "Arrow DatasetBundle 组件 kind 不匹配: expected={kind} actual={}",
            manifest.kind
        )));
    }
    Ok((manifest.dataset.fingerprint, manifest.row_count))
}

/// Recompute the canonical fingerprint and row count for a JSON component document.
pub fn dataset_component_fingerprint_from_json(
    payload: &str,
    kind: &str,
    source_path: &Path,
) -> Result<(String, u64), AppError> {
    let value: serde_json::Value = serde_json::from_str(payload).map_err(|error| {
        invalid_component(format!(
            "DatasetBundle 组件 JSON 无效 {}: {error}",
            source_path.display()
        ))
    })?;
    match kind {
        "corporate_actions" => {
            let actions = value
                .as_array()
                .or_else(|| value.get("actions").and_then(serde_json::Value::as_array))
                .ok_or_else(|| {
                    invalid_component("corporate_actions 组件必须是数组或包含 actions 数组")
                })?;
            let canonical = serde_json::to_vec(actions).map_err(|error| {
                invalid_component(format!("规范化 corporate_actions 组件失败: {error}"))
            })?;
            Ok((qx_strategy::sha256_hex(&canonical), actions.len() as u64))
        }
        "calendar" => qx_data::calendar_component_fingerprint_from_json(payload)
            .map_err(|error| invalid_component(format!("{}: {error}", source_path.display()))),
        _ => {
            let rows = value
                .as_array()
                .or_else(|| value.get("rows").and_then(serde_json::Value::as_array))
                .or_else(|| value.get("data").and_then(serde_json::Value::as_array))
                .ok_or_else(|| {
                    invalid_component(format!(
                        "DatasetBundle 组件 {kind} 必须是数组，或包含 rows/data 数组"
                    ))
                })?;
            let canonical = canonicalize_json(&value);
            let bytes = serde_json::to_vec(&canonical).map_err(|error| {
                invalid_component(format!("规范化 DatasetBundle 组件 {kind} 失败: {error}"))
            })?;
            Ok((qx_strategy::sha256_hex(&bytes), rows.len() as u64))
        }
    }
}

fn canonicalize_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => {
            let mut sorted = serde_json::Map::new();
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            for (key, value) in entries {
                sorted.insert(key.clone(), canonicalize_json(value));
            }
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonicalize_json).collect())
        }
        _ => value.clone(),
    }
}

fn invalid_component(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

#[cfg(test)]
mod assembly_tests {
    use super::assemble_dataset_bundle;
    use qx_data::{DatasetComponentFormat, DatasetComponentManifest, DatasetManifest};

    fn component(kind: &str) -> DatasetComponentManifest {
        DatasetComponentManifest {
            kind: kind.into(),
            format: DatasetComponentFormat::Json,
            dataset: DatasetManifest {
                dataset_id: format!("bundle.{kind}"),
                version: "v1".into(),
                source: "test".into(),
                fingerprint: format!("{kind}-fingerprint"),
                schema_version: 1,
                start_timestamp: 1,
                end_timestamp: 2,
            },
            row_count: 1,
        }
    }

    #[test]
    fn bundle_assembly_validates_required_bars_and_duplicate_components() {
        assert!(
            assemble_dataset_bundle("bundle", "v1", "test", [component("actions")])
                .unwrap_err()
                .contains("must contain a bars component")
        );
        assert!(assemble_dataset_bundle(
            "bundle",
            "v1",
            "test",
            [component("bars"), component("bars")]
        )
        .unwrap_err()
        .contains("already exists: bars"));

        let bundle = assemble_dataset_bundle(
            "bundle",
            "v1",
            "test",
            [component("bars"), component("actions")],
        )
        .unwrap();
        assert_eq!(bundle.components.len(), 2);
        assert_eq!(bundle.component("bars").unwrap().row_count, 1);
        assert!(bundle.fingerprint().is_ok());
    }
}
