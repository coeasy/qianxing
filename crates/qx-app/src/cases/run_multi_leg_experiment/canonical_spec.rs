//! Path-independent identity for a bounded multi-leg parameter experiment.

use super::RunMultiLegExperimentSpec;
use crate::cases::run_experiment::quote_unsafe_integer_tokens;
use crate::cases::run_multi_leg_backtest::canonical_spec::{
    build_multi_leg_canonical_run_spec, effective_market_specs,
};
use crate::cases::run_multi_leg_backtest::read_bar_frame;
use crate::cases::run_multi_market_manifest::verify_dataset_quality;
use crate::StrategyArtifactIdentity;
use crate::{AppError, AppErrorCategory, RunContext};
use qx_spec::{CanonicalDatasetIdentity, CanonicalRunSpec};
use serde_json::Value;
use std::path::Path;

pub(super) fn build_multi_leg_experiment_identity(
    spec: &RunMultiLegExperimentSpec,
    identity: Option<&StrategyArtifactIdentity>,
    context: &RunContext,
) -> Result<(CanonicalRunSpec, String), AppError> {
    let primary = read_bar_frame(&spec.base.primary_bars_path, "主腿")?;
    let reference = read_bar_frame(&spec.base.reference_bars_path, "对冲腿")?;
    let primary_fingerprint = qx_data::fingerprint_bar_frame(&primary)
        .map_err(|error| invalid(format!("多腿实验主腿 BarFrame 指纹失败: {error}")))?;
    let reference_fingerprint = qx_data::fingerprint_bar_frame(&reference)
        .map_err(|error| invalid(format!("多腿实验对冲腿 BarFrame 指纹失败: {error}")))?;
    let quality = match (
        spec.base.primary_dataset_manifest_path.as_deref(),
        spec.base.reference_dataset_manifest_path.as_deref(),
    ) {
        (Some(primary_manifest), Some(reference_manifest)) => Some(serde_json::json!({
            "primary": verify_dataset_quality(
                primary_manifest,
                &spec.base.primary_bars_path,
                &primary,
                &primary_fingerprint,
            )?,
            "reference": verify_dataset_quality(
                reference_manifest,
                &spec.base.reference_bars_path,
                &reference,
                &reference_fingerprint,
            )?,
        })),
        (None, None) => None,
        _ => return Err(invalid("多腿实验的数据质量清单必须成对提供")),
    };
    let input_data_fingerprint = if let Some(quality) = &quality {
        format!(
            "multi-bar:{primary_fingerprint}:{reference_fingerprint}:quality:{}:{}",
            quality["primary"]["manifest_fingerprint"]
                .as_str()
                .unwrap_or_default(),
            quality["reference"]["manifest_fingerprint"]
                .as_str()
                .unwrap_or_default(),
        )
    } else {
        format!("multi-bar:{primary_fingerprint}:{reference_fingerprint}")
    };

    let primary_start = *primary
        .ts
        .first()
        .ok_or_else(|| invalid("多腿实验主腿 BarFrame 没有时间范围"))?;
    let primary_end = *primary
        .ts
        .last()
        .ok_or_else(|| invalid("多腿实验主腿 BarFrame 没有结束时间"))?;
    let reference_start = *reference
        .ts
        .first()
        .ok_or_else(|| invalid("多腿实验对冲腿 BarFrame 没有时间范围"))?;
    let reference_end = *reference
        .ts
        .last()
        .ok_or_else(|| invalid("多腿实验对冲腿 BarFrame 没有结束时间"))?;
    let datasets = vec![
        CanonicalDatasetIdentity {
            role: "bars:primary".into(),
            instrument: primary.instrument.to_string(),
            content_fingerprint: primary_fingerprint,
            start_ts_ms: primary_start,
            end_ts_ms: primary_end,
        },
        CanonicalDatasetIdentity {
            role: "bars:reference".into(),
            instrument: reference.instrument.to_string(),
            content_fingerprint: reference_fingerprint,
            start_ts_ms: reference_start,
            end_ts_ms: reference_end,
        },
    ];
    let (primary_market, reference_market) =
        effective_market_specs(&spec.base, &primary.instrument, &reference.instrument);
    let strategy_id = identity
        .map(|value| value.strategy_id.as_str())
        .unwrap_or(spec.base.strategy.strategy_id.as_str());
    let strategy_version = identity
        .map(|value| value.strategy_version.clone())
        .unwrap_or_else(|| format!("{strategy_id}-v1"));
    let strategy_source_sha256 = identity.map(|value| value.source_sha256.as_str());
    let account_id = spec
        .base
        .account_id
        .clone()
        .unwrap_or_else(|| "multi_leg_experiment_candidate_scope_v1".into());
    let clock_start = primary_start.min(reference_start);
    let clock_end = primary_end.max(reference_end);
    let runtime_version = format!("qx-app-{}", env!("CARGO_PKG_VERSION"));
    let mut canonical = build_multi_leg_canonical_run_spec(
        &spec.base,
        datasets,
        quality.as_ref(),
        &primary_market,
        &reference_market,
        strategy_id,
        &strategy_version,
        strategy_source_sha256,
        &account_id,
        None,
        context.code_commit(),
        &runtime_version,
        clock_start,
        clock_end,
    )?;
    canonical.run_type = "multi_leg_experiment".into();

    let behavior_json = quote_unsafe_integer_tokens(&spec.to_json()?);
    let mut behavior: Value = serde_json::from_str(&behavior_json)
        .map_err(|error| invalid(format!("多腿实验行为 JSON 无效: {error}")))?;
    let fields = behavior
        .as_object_mut()
        .ok_or_else(|| invariant("多腿实验 behavior 根节点不是 object"))?;
    fields.remove("experiment_id");
    let base = fields
        .get_mut("base")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| invariant("多腿实验 behavior 缺少 base"))?;
    for locator in [
        "run_id",
        "primary_bars_path",
        "reference_bars_path",
        "output_dir",
        "primary_dataset_manifest_path",
        "reference_dataset_manifest_path",
    ] {
        base.remove(locator);
    }
    normalize_identity_values(&mut behavior, "behavior")?;
    canonical.behavior = behavior;
    canonical.extensions.insert(
        "input_data_fingerprint".into(),
        Value::String(input_data_fingerprint.clone()),
    );
    canonical.extensions.insert(
        "experiment_algorithm".into(),
        Value::String("bounded_grid_v1".into()),
    );
    canonical.extensions.insert(
        "candidate_objective".into(),
        Value::String("combined_return_bps_desc_ordinal_asc".into()),
    );
    canonical.extensions.insert(
        "candidate_account_id_policy".into(),
        Value::String(if spec.base.account_id.is_some() {
            "shared_explicit_account".into()
        } else {
            "derived_from_candidate_run_id".into()
        }),
    );
    canonical.extensions.insert(
        "candidate_count".into(),
        Value::Number(
            spec.validate()
                .map_err(|error| invalid(error.to_string()))?
                .into(),
        ),
    );
    canonical
        .canonical_json()
        .map_err(|error| invalid(format!("多腿实验 CanonicalRunSpec 无效: {error}")))?;
    Ok((canonical, input_data_fingerprint))
}

pub(super) fn verify_candidate_run_manifests(
    candidates: &[super::MultiLegExperimentCandidate],
    input_data_fingerprint: &str,
    experiment: &CanonicalRunSpec,
) -> Result<(), AppError> {
    verify_candidate_run_manifests_with_reader(
        candidates,
        input_data_fingerprint,
        experiment,
        &crate::artifact_store::FileArtifactReader,
    )
}

pub(crate) fn verify_candidate_run_manifests_with_reader(
    candidates: &[super::MultiLegExperimentCandidate],
    input_data_fingerprint: &str,
    experiment: &CanonicalRunSpec,
    reader: &dyn crate::artifact_store::ArtifactReader,
) -> Result<(), AppError> {
    for candidate in candidates {
        let Some(outcome) = &candidate.outcome else {
            continue;
        };
        let artifact_path = Path::new(&outcome.attribution_artifact);
        let manifest_path =
            artifact_path.with_file_name(format!("{}.multi-leg.run.json", outcome.run_id));
        let manifest_reference = manifest_path.to_string_lossy();
        let manifest_payload = crate::artifact_store::read_text_bounded(
            reader,
            &manifest_reference,
            16 * 1024 * 1024,
            "多腿实验候选 RunManifest",
        )?;
        let manifest = qx_core::RunManifest::from_json(&manifest_payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("多腿实验候选 RunManifest 无效: {error}"),
            )
        })?;
        if manifest.run_id != outcome.run_id
            || manifest.data_fingerprint != input_data_fingerprint
            || manifest.code_commit != experiment.code_revision
            || manifest.clock_start != experiment.clock.start_ts_ms
            || manifest.clock_end != experiment.clock.end_ts_ms
            || manifest.global_seed.to_string() != experiment.random_seed
            || manifest.runtime_version
                != experiment
                    .execution_model
                    .get("runtime_version")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
        {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                format!(
                    "多腿实验候选 {} 的 RunManifest 与实验输入身份不一致",
                    outcome.run_id
                ),
            ));
        }
        let candidate_spec_json = manifest.canonical_run_spec_json.as_deref().ok_or_else(|| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("多腿实验候选 {} 缺少 CanonicalRunSpec", outcome.run_id),
            )
        })?;
        let candidate_spec = CanonicalRunSpec::from_json(candidate_spec_json).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("多腿实验候选 CanonicalRunSpec 无效: {error}"),
            )
        })?;
        let candidate_fingerprint = candidate_spec
            .fingerprint()
            .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))?;
        if manifest.canonical_run_spec_fingerprint.as_deref()
            != Some(candidate_fingerprint.as_str())
            || manifest.config_hash != candidate_fingerprint
            || candidate_spec.code_revision != experiment.code_revision
            || candidate_spec.datasets != experiment.datasets
            || candidate_spec.market_rules != experiment.market_rules
            || candidate_spec.clock != experiment.clock
            || candidate_spec.random_seed != experiment.random_seed
            || !same_execution_model(&candidate_spec, experiment, &outcome.run_id)
            || !same_strategy_source(&candidate_spec, experiment)
            || !same_dataset_quality(&candidate_spec.extensions, &experiment.extensions)
        {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                format!(
                    "多腿实验候选 {} 的 CanonicalRunSpec 与实验身份不一致",
                    outcome.run_id
                ),
            ));
        }
    }
    Ok(())
}

fn same_execution_model(
    candidate: &CanonicalRunSpec,
    experiment: &CanonicalRunSpec,
    run_id: &str,
) -> bool {
    let per_candidate_account = experiment
        .extensions
        .get("candidate_account_id_policy")
        .and_then(Value::as_str)
        == Some("derived_from_candidate_run_id");
    if !per_candidate_account {
        return candidate.execution_model == experiment.execution_model;
    }
    let expected_account_id = format!("multi-leg-{run_id}");
    if candidate
        .execution_model
        .get("account_id")
        .and_then(Value::as_str)
        != Some(expected_account_id.as_str())
    {
        return false;
    }
    let mut candidate_execution = candidate.execution_model.clone();
    let mut experiment_execution = experiment.execution_model.clone();
    let Some(candidate_fields) = candidate_execution.as_object_mut() else {
        return false;
    };
    candidate_fields.remove("account_id");
    let Some(experiment_fields) = experiment_execution.as_object_mut() else {
        return false;
    };
    experiment_fields.remove("account_id");
    candidate_execution == experiment_execution
}

fn same_strategy_source(candidate: &CanonicalRunSpec, experiment: &CanonicalRunSpec) -> bool {
    match (
        candidate.strategies.as_slice(),
        experiment.strategies.as_slice(),
    ) {
        ([candidate], [experiment]) => {
            candidate.strategy_id == experiment.strategy_id
                && candidate.strategy_version == experiment.strategy_version
                && candidate.source_sha256 == experiment.source_sha256
        }
        _ => false,
    }
}

fn same_dataset_quality(
    candidate: &std::collections::BTreeMap<String, Value>,
    experiment: &std::collections::BTreeMap<String, Value>,
) -> bool {
    let candidate_quality = candidate
        .iter()
        .filter(|(name, _)| name.starts_with("dataset_quality:"))
        .collect::<std::collections::BTreeMap<_, _>>();
    let experiment_quality = experiment
        .iter()
        .filter(|(name, _)| name.starts_with("dataset_quality:"))
        .collect::<std::collections::BTreeMap<_, _>>();
    candidate_quality == experiment_quality
}

fn normalize_identity_values(value: &mut Value, path: &str) -> Result<(), AppError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(number) if number.is_f64() => Err(invalid(format!(
            "{path} 含浮点值，CanonicalRunSpec 需要精确数值"
        ))),
        Value::Number(number) => {
            if number
                .as_i64()
                .is_some_and(|number| number.unsigned_abs() > 9_007_199_254_740_991)
                || number
                    .as_u64()
                    .is_some_and(|number| number > 9_007_199_254_740_991)
            {
                return Err(invalid(format!(
                    "{path} 含跨语言不安全整数，需用十进制文本表示"
                )));
            }
            Ok(())
        }
        Value::Array(values) => {
            for (index, value) in values.iter_mut().enumerate() {
                normalize_identity_values(value, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            for (name, value) in fields.iter_mut() {
                if (name.ends_with("_raw") || name == "seed") && value.is_number() {
                    *value = Value::String(value.to_string());
                } else {
                    normalize_identity_values(value, &format!("{path}.{name}"))?;
                }
            }
            Ok(())
        }
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn invariant(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}
