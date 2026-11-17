use super::artifacts::write_text;
use crate::{AppError, AppErrorCategory, StrategyArtifactIdentity};
use serde_json::Value;

pub(super) fn serialize_experiment_input(
    spec_json: &str,
    code_commit: &str,
    identity: Option<&StrategyArtifactIdentity>,
) -> Result<String, AppError> {
    let _: Value = serde_json::from_str(spec_json).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("序列化多市场实验规格 sidecar 失败: {error}"),
        )
    })?;
    let code_commit = serde_json::to_string(code_commit).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("序列化多市场实验 code_commit 失败: {error}"),
        )
    })?;
    let strategy_identity = serde_json::to_string(&identity).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("序列化多市场实验策略身份失败: {error}"),
        )
    })?;
    let exact_spec_json = serde_json::to_string(spec_json.trim()).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("序列化多市场实验精确规格文本失败: {error}"),
        )
    })?;
    // Preserve exact integer tokens (notably i128 parameter bounds) instead of round-tripping the
    // specification through serde_json::Value, which cannot represent the full signed i128 range.
    Ok(format!(
        "{{\"schema_version\":1,\"code_commit\":{code_commit},\"strategy_identity\":{strategy_identity},\"spec\":{},\"spec_json\":{exact_spec_json}}}",
        spec_json.trim(),
    ))
}

pub(super) fn verify_spec_artifact(
    path: &str,
    expected: &str,
    label: &str,
) -> Result<(), AppError> {
    match std::fs::read_to_string(path) {
        Ok(existing) if same_experiment_input(&existing, expected) => Ok(()),
        Ok(_) => Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("{label}规格 sidecar 与当前规格不一致: {path}"),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(AppError::new(
            AppErrorCategory::StorageFailure,
            format!("既有{label}报告缺少规格 sidecar: {path}"),
        )),
        Err(error) => Err(AppError::from_io(
            &format!("读取{label}规格 sidecar"),
            &error,
        )),
    }
}

pub(super) fn write_spec_artifact(path: &str, expected: &str, label: &str) -> Result<(), AppError> {
    match std::fs::read_to_string(path) {
        Ok(existing) if same_experiment_input(&existing, expected) => Ok(()),
        Ok(_) => Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("{label}规格 sidecar 已存在且内容不同: {path}"),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => write_text(path, expected),
        Err(error) => Err(AppError::from_io(
            &format!("读取既有{label}规格 sidecar"),
            &error,
        )),
    }
}

fn same_experiment_input(existing: &str, expected: &str) -> bool {
    if existing == expected {
        return true;
    }
    let (Ok(mut existing), Ok(mut expected)) = (
        serde_json::from_str::<Value>(existing),
        serde_json::from_str::<Value>(expected),
    ) else {
        return false;
    };
    for value in [&mut existing, &mut expected] {
        normalize_experiment_input_paths(value);
    }
    existing == expected
}

fn normalize_experiment_input_paths(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object.remove("spec_json");
    if let Some(spec) = object.get_mut("spec") {
        strip_location_fields(spec);
    }
}

fn strip_location_fields(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                strip_location_fields(item);
            }
        }
        Value::Object(fields) => {
            fields.retain(|name, _| {
                !matches!(name.as_str(), "run_id" | "manifest_run_id" | "output_dir")
                    && !name.ends_with("_path")
                    && !name.ends_with("_source")
            });
            for child in fields.values_mut() {
                strip_location_fields(child);
            }
        }
        _ => {}
    }
}
