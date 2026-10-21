//! Provider-backed Bar research use case. Provider selection/normalization remains in
//! `qx-provider`; immutable dataset freezing and the shared backtest live at this layer.

use crate::artifact_store::sink;
use crate::cases::{attach, guard::guard_panics};
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::{
    run_backtest, validate_dataset, BacktestOutcome, BacktestSpec, DatasetSpec, DatasetVerdict,
};
use qx_provider::{
    DataRequest, DatasetManifestV2, ProviderBarFrame, ProviderErrorClass, ProviderRegistry,
    RegistryError,
};
use std::fs;
use std::path::{Path, PathBuf};

/// Result of freezing validated Provider data, checking it, and running the shared Bar use case.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderBacktestOutcome {
    pub outcome: BacktestOutcome,
    pub dataset_verdict: DatasetVerdict,
    pub manifest: DatasetManifestV2,
    pub manifest_path: String,
}

/// Inputs after Provider selection for the shared freeze/validate/backtest core.
struct FetchedProviderBacktest<'a> {
    pub fetched: &'a ProviderBarFrame,
    pub spec: &'a BacktestSpec,
    pub dataset_id: &'a str,
    pub dataset_version: &'a str,
    pub interval_ms: u64,
    pub timezone: &'a str,
    /// Optional caller-selected sidecar path. The manifest remains immutable once written.
    pub manifest_path: Option<&'a str>,
}

/// Complete Provider-to-backtest input. The application layer owns source selection and failover.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderBacktestSpec {
    pub data_request: DataRequest,
    pub backtest: BacktestSpec,
    pub dataset_id: String,
    pub dataset_version: String,
    pub interval_ms: u64,
    pub timezone: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<String>,
}

impl ProviderBacktestSpec {
    pub fn validate(&self) -> Result<(), AppError> {
        self.data_request
            .query
            .validate()
            .and_then(|()| {
                self.data_request
                    .requirements
                    .validate_for(&self.data_request.query)
            })
            .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error.message))?;
        self.backtest.validate()?;
        if self.data_request.query.kind != qx_provider::DataKind::Bar
            || self.data_request.query.instrument_set.len() != 1
            || self.data_request.query.instrument_set.iter().next()
                != Some(&self.backtest.instrument)
            || self.dataset_id.trim().is_empty()
            || self.dataset_version.trim().is_empty()
            || self.interval_ms == 0
            || self.timezone.trim().is_empty()
            || self
                .manifest_path
                .as_deref()
                .is_some_and(|path| path.trim().is_empty())
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "Provider backtest requires matching single-instrument Bar data, dataset identity, positive interval, timezone, and a non-empty optional manifest path",
            ));
        }
        Ok(())
    }
}

/// Select/fail over a Provider, freeze its validated BarFrame and manifest, validate the dataset,
/// then execute the shared deterministic Rust backtest.
pub fn run_provider_backtest_with_manifest(
    registry: &ProviderRegistry,
    spec: &ProviderBacktestSpec,
    context: &RunContext,
) -> Result<ProviderBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "Provider Bar 回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        spec.validate()?;
        let fetched = registry
            .fetch_bar_frame(&spec.data_request)
            .map_err(|error| {
                let (category, message) = match error {
                    RegistryError::NoProvider => (
                        AppErrorCategory::DataUnavailable,
                        "no Provider satisfies the backtest data request".to_string(),
                    ),
                    RegistryError::Duplicate(id) => (
                        AppErrorCategory::InvalidInput,
                        format!("duplicate Provider registration: {id}"),
                    ),
                    RegistryError::Invariant(message) => {
                        (AppErrorCategory::InternalInvariant, message)
                    }
                    RegistryError::Provider(error) => {
                        let category = match error.class {
                            ProviderErrorClass::SwitchProvider => AppErrorCategory::DataUnavailable,
                            ProviderErrorClass::ManualIntervention => {
                                AppErrorCategory::PermissionDenied
                            }
                            ProviderErrorClass::Permanent => AppErrorCategory::InvalidInput,
                        };
                        (category, error.message)
                    }
                };
                AppError::new(category, format!("Provider fetch failed: {message}"))
            })?;
        run_provider_backtest_inner(
            &FetchedProviderBacktest {
                fetched: &fetched,
                spec: &spec.backtest,
                dataset_id: &spec.dataset_id,
                dataset_version: &spec.dataset_version,
                interval_ms: spec.interval_ms,
                timezone: &spec.timezone,
                manifest_path: spec.manifest_path.as_deref(),
            },
            context,
        )
    })
    .map_err(|error| attach(error, &correlation_id))
}

fn run_provider_backtest_inner(
    request: &FetchedProviderBacktest<'_>,
    context: &RunContext,
) -> Result<ProviderBacktestOutcome, AppError> {
    let manifest = request
        .fetched
        .dataset_manifest(
            request.dataset_id,
            request.dataset_version,
            request.interval_ms,
            request.timezone,
        )
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("Provider dataset manifest is invalid: {error}"),
            )
        })?;
    let frame_json = request.fetched.frame.to_json();
    let manifest_json = manifest.to_json().map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("validated Provider manifest serialization failed: {error}"),
        )
    })?;
    let bars_path = PathBuf::from(&request.spec.bars_path);
    let manifest_path = request
        .manifest_path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{}.manifest.json", request.spec.bars_path)));
    freeze(&bars_path, frame_json.as_bytes())?;
    freeze(&manifest_path, manifest_json.as_bytes())?;
    let dataset_verdict = validate_dataset(
        &DatasetSpec::new(request.dataset_id, request.spec.bars_path.clone()),
        context,
    )?;
    let outcome = run_backtest(request.spec, context)?;
    Ok(ProviderBacktestOutcome {
        outcome,
        dataset_verdict,
        manifest,
        manifest_path: manifest_path.to_string_lossy().into_owned(),
    })
}

pub(super) fn freeze(path: &Path, payload: &[u8]) -> Result<(), AppError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!(
                    "cannot create frozen dataset directory {}: {error}",
                    parent.display()
                ),
            )
        })?;
    }
    // 冻结数据集也是产物：走发布出口的 Idempotent 档（内容相同幂等、不同即冲突），
    // 语义与原先的「create_new → 失败后比对」完全一致，但临时文件与清理只有一处。
    sink::publish_bytes(path, payload, sink::PublishMode::Idempotent).map_err(|error| match error {
        sink::PublishError::Conflict(_) => AppError::new(
            AppErrorCategory::Conflict,
            format!(
                "refusing to replace different frozen dataset file {}",
                path.display()
            ),
        ),
        sink::PublishError::Io(message) => AppError::new(
            AppErrorCategory::StorageFailure,
            format!("cannot freeze dataset file {}: {message}", path.display()),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freezing_is_idempotent_for_same_bytes_and_refuses_replacement() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("qx-app-provider-freeze-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("bars.json");

        freeze(&path, b"canonical-frame").unwrap();
        freeze(&path, b"canonical-frame").unwrap();
        let error = freeze(&path, b"different-frame").unwrap_err();
        assert_eq!(error.category(), AppErrorCategory::Conflict);
        assert_eq!(fs::read(&path).unwrap(), b"canonical-frame");
    }
}
