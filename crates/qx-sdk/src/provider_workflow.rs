//! Provider data freezing is SDK orchestration; all manifest and backtest semantics
//! remain owned by `qx-data` and `qx-app` respectively.

use crate::{AppError, AppErrorCategory, BacktestOutcome, BacktestSpec, Client, DatasetSpec};
use qx_provider::{DatasetManifestV2, ProviderBarFrame};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Result of freezing validated Provider data, checking it, and running the shared Bar use case.
pub struct ProviderBacktestOutcome {
    pub outcome: BacktestOutcome,
    pub dataset_verdict: crate::DatasetVerdict,
    pub manifest: DatasetManifestV2,
    pub manifest_path: String,
}

/// Inputs for the one-step Provider freeze, validate, backtest and replay workflow.
pub struct ProviderBacktestRequest<'a> {
    pub fetched: &'a ProviderBarFrame,
    pub spec: &'a BacktestSpec,
    pub dataset_id: &'a str,
    pub dataset_version: &'a str,
    pub interval_ms: u64,
    pub timezone: &'a str,
    /// Optional caller-selected sidecar path. The manifest remains immutable once written.
    pub manifest_path: Option<&'a str>,
}

pub(crate) fn run_provider_backtest_with_manifest(
    client: &Client,
    request: &ProviderBacktestRequest<'_>,
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
    let dataset_verdict = client.validate_dataset(&DatasetSpec::new(
        request.dataset_id,
        request.spec.bars_path.clone(),
    ))?;
    let outcome = client.run_backtest(request.spec)?;
    Ok(ProviderBacktestOutcome {
        outcome,
        dataset_verdict,
        manifest,
        manifest_path: manifest_path.to_string_lossy().into_owned(),
    })
}

fn freeze(path: &Path, payload: &[u8]) -> Result<(), AppError> {
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
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            if let Err(error) = file.write_all(payload).and_then(|()| file.sync_all()) {
                drop(file);
                let _ = fs::remove_file(path);
                return Err(AppError::new(
                    AppErrorCategory::StorageFailure,
                    format!("cannot freeze dataset file {}: {error}", path.display()),
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = fs::read(path).map_err(|read_error| {
                AppError::new(
                    AppErrorCategory::StorageFailure,
                    format!(
                        "cannot read existing frozen dataset {}: {read_error}",
                        path.display()
                    ),
                )
            })?;
            if existing == payload {
                Ok(())
            } else {
                Err(AppError::new(
                    AppErrorCategory::Conflict,
                    format!(
                        "refusing to replace different frozen dataset file {}",
                        path.display()
                    ),
                ))
            }
        }
        Err(error) => Err(AppError::new(
            AppErrorCategory::StorageFailure,
            format!(
                "cannot create frozen dataset file {}: {error}",
                path.display()
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freezing_is_idempotent_for_same_bytes_and_refuses_replacement() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("qx-sdk-freeze-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("bars.json");

        freeze(&path, b"canonical-frame").unwrap();
        freeze(&path, b"canonical-frame").unwrap();
        let error = freeze(&path, b"different-frame").unwrap_err();
        assert_eq!(error.category(), AppErrorCategory::Conflict);
        assert_eq!(fs::read(&path).unwrap(), b"canonical-frame");

        fs::remove_dir_all(root).unwrap();
    }
}
