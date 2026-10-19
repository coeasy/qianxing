use super::artifacts::write_text;
use crate::{AppError, AppErrorCategory, StrategyArtifactIdentity};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
struct PersistedExperimentInput<'a> {
    schema_version: u32,
    code_commit: &'a str,
    strategy_identity: Option<&'a StrategyArtifactIdentity>,
    spec: Value,
}

pub(super) fn serialize_experiment_input(
    spec_json: &str,
    code_commit: &str,
    identity: Option<&StrategyArtifactIdentity>,
) -> Result<String, AppError> {
    let spec = serde_json::from_str(spec_json).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("序列化多市场实验规格 sidecar 失败: {error}"),
        )
    })?;
    serde_json::to_string(&PersistedExperimentInput {
        schema_version: 1,
        code_commit,
        strategy_identity: identity,
        spec,
    })
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("序列化多市场实验输入 sidecar 失败: {error}"),
        )
    })
}

pub(super) fn verify_spec_artifact(
    path: &str,
    expected: &str,
    label: &str,
) -> Result<(), AppError> {
    match std::fs::read_to_string(path) {
        Ok(existing) if existing == expected => Ok(()),
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
        Ok(existing) if existing == expected => Ok(()),
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
