//! Provider-backed Bar research use case. Provider selection/normalization remains in
//! `qx-provider`; immutable dataset freezing and the shared backtest live at this layer.

use crate::artifact_store::sink;
use crate::cases::{attach, guard::guard_panics};
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::{validate_dataset, BacktestOutcome, BacktestSpec, DatasetSpec, DatasetVerdict};
use qx_provider::{
    DataRequest, DatasetManifestV2, ProviderBarFrame, ProviderErrorClass, ProviderRegistry,
    RegistryError,
};
use serde_json::json;
use std::collections::BTreeMap;
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
    pub data_request: &'a DataRequest,
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
                data_request: &spec.data_request,
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
    let request_json = request.data_request.to_json().map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!(
                "validated Provider request serialization failed: {}",
                error.message
            ),
        )
    })?;
    let manifest_sha256 = qx_data::sha256_hex_bytes(manifest_json.as_bytes());
    let lineage = json!({
        "schema_version": 1,
        "provider_id": request.fetched.result.provider_id.clone(),
        "provider_version": request.fetched.result.provider_version.clone(),
        "request_id": request.fetched.result.request_id.clone(),
        "retry_chain": request.fetched.result.retry_chain.clone(),
        "source_hash": format!("{:016x}", request.fetched.result.source_hash),
        "query_digest": format!("{:016x}", request.data_request.query.digest()),
        "request_json": request_json,
        "dataset_manifest_json": manifest_json,
        "dataset_manifest_sha256": manifest_sha256,
    });
    let outcome = crate::cases::run_backtest_with_identity_extensions(
        request.spec,
        context,
        BTreeMap::from([("provider_lineage".to_string(), lineage)]),
    )?;
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
    use crate::{BacktestDatasetInput, BuiltinStrategySpec, CallerCapability, RunContext};
    use qx_core::{RunManifest, SCALE};
    use qx_provider::{
        DataKind, DataProvider, DataQuery, DataRequirements, MarketDataTier, ProviderCapability,
        ProviderError, ProviderPayload, ProviderResult,
    };
    use qx_strategy::BuiltinStrategyKind;
    use std::collections::BTreeSet;

    struct FixedBarProvider {
        capability: ProviderCapability,
        result: ProviderResult,
    }

    impl DataProvider for FixedBarProvider {
        fn capability(&self) -> &ProviderCapability {
            &self.capability
        }

        fn fetch(&self, _query: &qx_provider::DataQuery) -> Result<ProviderResult, ProviderError> {
            Ok(self.result.clone())
        }
    }

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

    #[test]
    fn provider_lineage_is_bound_to_run_identity_and_report_readback() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("qx-app-provider-lineage-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let bars_path = root.join("bars.json");
        let output_dir = root.join("runs");
        let instrument = "BTCUSDT.BINANCE";
        let payloads = (0..48)
            .map(|index| {
                let ts = 1_000 + index * 1_000;
                let close = (100 + (index % 9) as i128) * SCALE;
                let bar = qx_guanxing::Bar::new(
                    ts,
                    close,
                    close + SCALE,
                    close - SCALE,
                    close,
                    100 * SCALE,
                );
                (
                    ts,
                    ts,
                    ProviderPayload::new_json(
                        instrument,
                        "application/vnd.qianxing.bar+json;v=1",
                        serde_json::to_string(&bar).unwrap(),
                    ),
                )
            })
            .collect();
        let result = ProviderResult::from_payloads(
            "test-provider",
            "provider-v1",
            "request-1",
            50_000,
            payloads,
        )
        .unwrap();
        let capability = ProviderCapability {
            provider_id: "test-provider".into(),
            version: "provider-v1".into(),
            data_kinds: [DataKind::Bar].into_iter().collect(),
            data_tiers: [MarketDataTier::Bar].into_iter().collect(),
            asset_classes: ["crypto".into()].into_iter().collect(),
            frequencies: ["1s".into()].into_iter().collect(),
            auth_scope: "public".into(),
            rate_limit_per_second: 100,
            freshness_seconds: 60,
            historical_start: 0,
            historical_end: 100_000,
            realtime: false,
            priority: 1,
            quality_score: 100,
            cost_score: 1,
        };
        let mut registry = ProviderRegistry::new();
        registry
            .register(Box::new(FixedBarProvider { capability, result }))
            .unwrap();
        let data_request = DataRequest::new(
            DataQuery {
                kind: DataKind::Bar,
                asset_class: "crypto".into(),
                instrument_set: [instrument.into()].into_iter().collect::<BTreeSet<_>>(),
                field_set: BTreeSet::new(),
                frequency: "1s".into(),
                adjustment: "none".into(),
                quality_policy: "strict".into(),
                start: 1_000,
                end: 48_000,
                as_of: None,
            },
            DataRequirements::default(),
        );
        let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "provider-sma");
        strategy.fast_window = 2;
        strategy.slow_window = 3;
        let spec = ProviderBacktestSpec {
            data_request,
            backtest: BacktestSpec::new(
                BacktestDatasetInput {
                    run_id: "provider-lineage-run".into(),
                    instrument: instrument.into(),
                    bars_path: bars_path.to_string_lossy().into_owned(),
                },
                "USDT",
                10_000 * SCALE,
                20261025,
                output_dir.to_string_lossy().into_owned(),
                strategy,
            ),
            dataset_id: "provider-bars".into(),
            dataset_version: "2026-10-25".into(),
            interval_ms: 1_000,
            timezone: "UTC".into(),
            manifest_path: None,
        };

        let outcome = run_provider_backtest_with_manifest(
            &registry,
            &spec,
            &RunContext::new(CallerCapability::Research, "provider-lineage-run"),
        )
        .expect("Provider-backed Bar run should complete");
        let summary_path = output_dir.join("provider-lineage-run.summary.json");
        let summary: serde_json::Value =
            serde_json::from_slice(&fs::read(&summary_path).unwrap()).unwrap();
        let run_manifest_path = output_dir.join("provider-lineage-run.run.json");
        let run_manifest =
            RunManifest::from_json(&fs::read_to_string(run_manifest_path).unwrap()).unwrap();
        let canonical = qx_spec::CanonicalRunSpec::from_json(
            run_manifest.canonical_run_spec_json.as_deref().unwrap(),
        )
        .unwrap();
        let lineage = canonical.extensions.get("provider_lineage").unwrap();
        assert_eq!(lineage["provider_id"], "test-provider");
        assert_eq!(lineage["request_id"], "request-1");
        assert_eq!(
            summary["input"]["provider_lineage"], *lineage,
            "report input must carry the same Provider lineage as the run identity"
        );
        assert!(Path::new(&outcome.manifest_path).is_file());
        crate::artifact_store::verify_declared_run_manifest(&summary)
            .expect("Provider lineage should survive standard artifact readback");
        let report = crate::read_provider_backtest_report_v1(
            &outcome,
            &RunContext::new(CallerCapability::Research, "provider-lineage-run"),
        )
        .expect("Provider report should bind its envelope to the shared report");
        assert!(report.report.verification.verified);
        assert_eq!(report.report.run_id, "provider-lineage-run");
        assert_eq!(report.manifest, outcome.manifest);

        let mut tampered_outcome = outcome.clone();
        tampered_outcome.manifest.provider_version = "provider-v2".into();
        assert!(crate::read_provider_backtest_report_v1(
            &tampered_outcome,
            &RunContext::new(CallerCapability::Research, "provider-lineage-run"),
        )
        .is_err());

        let mut tampered_summary = summary.clone();
        tampered_summary["input"]["provider_lineage"]["provider_id"] =
            serde_json::json!("other-provider");
        assert!(
            crate::artifact_store::verify_declared_run_manifest(&tampered_summary)
                .unwrap_err()
                .contains("Provider lineage")
        );
    }
}
