use super::super::run_multi_market_backtest::canonical_spec::build_multi_market_canonical_run_spec;
use super::super::run_multi_market_manifest::{verify_dataset_quality, DataIdentity};
use super::{
    RunContext, RunMultiMarketExperimentResult, RunMultiMarketExperimentSpec,
    StrategyArtifactIdentity,
};
use crate::cases::experiment_spec_artifact::write_spec_artifact;
use crate::cases::run_experiment::{
    experiment_result_hash_from_json, normalize_experiment_identity_values,
    quote_unsafe_integer_tokens,
};
use crate::{AppError, AppErrorCategory};
use qx_core::RunManifest;
use qx_datastruct::BarFrame;
use qx_spec::{ArtifactRefV2, CanonicalRunSpec, ReplayVerdict, RunRecordV2, RunStatus};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

pub(super) fn build_identity(
    spec: &RunMultiMarketExperimentSpec,
    identity: Option<&StrategyArtifactIdentity>,
    context: &RunContext,
) -> Result<(CanonicalRunSpec, String), AppError> {
    let mut data_identities = Vec::with_capacity(spec.base.markets.len());
    let mut market_inputs = Vec::with_capacity(spec.base.markets.len());
    let mut clock_start = u64::MAX;
    let mut clock_end = 0_u64;

    for market in &spec.base.markets {
        let payload = super::super::artifacts::read_text(&market.bars_path)?;
        let frame = BarFrame::from_json(&payload).map_err(|error| {
            invalid(format!(
                "市场 {} BarFrame 无效，不能建立多市场实验身份: {error:?}",
                market.instrument
            ))
        })?;
        if frame.instrument.to_string() != market.instrument {
            return Err(invalid(format!(
                "市场 {} 的 BarFrame 标的是 {}，不能建立多市场实验身份",
                market.instrument, frame.instrument
            )));
        }
        let (Some(first_ts), Some(last_ts)) = (frame.ts.first().copied(), frame.ts.last().copied())
        else {
            return Err(AppError::new(
                AppErrorCategory::FidelityInsufficient,
                format!("市场 {} 至少需要一根 Bar", market.instrument),
            ));
        };
        let content_hash = qx_data::fingerprint_bar_frame(&frame)
            .map_err(|error| invalid(format!("BarFrame 指纹计算失败: {error}")))?;
        let quality = market
            .dataset_manifest_path
            .as_deref()
            .map(|manifest_path| {
                verify_dataset_quality(manifest_path, &market.bars_path, &frame, &content_hash)
            })
            .transpose()?;
        let quality_fingerprint = quality
            .as_ref()
            .and_then(|value| value.get("manifest_fingerprint"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let input_path = Path::new(&market.bars_path)
            .canonicalize()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| market.bars_path.clone());
        data_identities.push(DataIdentity {
            instrument: market.instrument.clone(),
            path: input_path,
            content_hash: content_hash.clone(),
            row_count: frame.ts.len(),
            start_ts_ms: first_ts,
            end_ts_ms: last_ts,
            quality_fingerprint: quality_fingerprint.clone(),
        });
        market_inputs.push(crate::MultiMarketInputIdentityV1 {
            instrument: market.instrument.clone(),
            bar_fingerprint: content_hash,
            quality_fingerprint,
        });
        clock_start = clock_start.min(first_ts);
        clock_end = clock_end.max(last_ts);
    }

    let input_set =
        crate::multi_market_report::multi_market_input_set_identity_v1(&spec.base, &market_inputs)?;
    let runtime_version = format!("qx-app-{}", env!("CARGO_PKG_VERSION"));
    let mut canonical = build_multi_market_canonical_run_spec(
        &spec.base,
        &identity.into_iter().cloned().collect::<Vec<_>>(),
        &data_identities,
        context.code_commit(),
        &runtime_version,
        clock_start,
        clock_end,
        None,
        None,
    )?;

    let portable_json = quote_unsafe_integer_tokens(&spec.to_json()?);
    let mut behavior: Value = serde_json::from_str(&portable_json)
        .map_err(|error| invalid(format!("多市场实验行为 JSON 无效: {error}")))?;
    let fields = behavior
        .as_object_mut()
        .ok_or_else(|| invariant("多市场实验行为不是 JSON 对象"))?;
    fields.remove("experiment_id");
    let base = fields
        .get_mut("base")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| invariant("多市场实验行为缺少 base"))?;
    base.remove("run_id");
    base.remove("output_dir");
    let markets = base
        .get_mut("markets")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invariant("多市场实验行为缺少 markets"))?;
    for market in markets {
        let market = market
            .as_object_mut()
            .ok_or_else(|| invariant("多市场实验行为项不是对象"))?;
        market.remove("bars_path");
        market.remove("dataset_manifest_path");
    }
    normalize_experiment_identity_values(&mut behavior, "behavior")?;
    canonical.run_type = "multi_market_experiment".into();
    canonical.behavior = behavior;
    canonical.extensions.insert(
        "input_data_fingerprint".into(),
        input_set.data_fingerprint.clone().into(),
    );
    for (name, fingerprint) in input_set.input_components {
        canonical
            .extensions
            .insert(format!("input_component:{name}"), fingerprint.into());
    }
    canonical
        .canonical_json()
        .map_err(|error| invariant(format!("多市场实验 CanonicalRunSpec 无效: {error}")))?;
    Ok((canonical, input_set.data_fingerprint))
}

pub(super) fn persist_result(
    mut result: RunMultiMarketExperimentResult,
    canonical: &CanonicalRunSpec,
    root: &Path,
    report_index_path: &str,
    spec_artifact_path: &str,
) -> Result<RunMultiMarketExperimentResult, AppError> {
    let facts_payload = result.to_json()?;
    let result_hash = experiment_result_hash_from_json(&facts_payload)
        .map_err(|error| invariant(format!("多市场实验结果指纹失败: {error}")))?;
    result.result_hash = Some(result_hash.clone());
    let manifest = build_manifest(&result.experiment_id, canonical, &result_hash)?;
    let manifest_path = crate::artifact_store::persist_run_manifest(root, &manifest)?;
    let report_path = root.join(format!(
        "{}-{result_hash}-input-v1.multi-market-experiment.json",
        result.experiment_id
    ));
    let record_path = root.join(format!(
        "{}-{result_hash}-input-v1.record.json",
        result.experiment_id
    ));
    let spec_bytes = std::fs::read(spec_artifact_path)
        .map_err(|error| AppError::from_io("读取多市场实验输入规格 sidecar", &error))?;
    result.artifact_path = report_index_path.to_owned();
    result.canonical_run_spec_json =
        Some(canonical.canonical_json().map_err(|error| {
            invariant(format!("多市场实验 CanonicalRunSpec 编码失败: {error}"))
        })?);
    result.canonical_run_spec_fingerprint =
        Some(canonical.fingerprint().map_err(|error| {
            invariant(format!("多市场实验 CanonicalRunSpec 指纹失败: {error}"))
        })?);
    result.input_data_fingerprint = canonical
        .extensions
        .get("input_data_fingerprint")
        .and_then(Value::as_str)
        .map(str::to_owned);
    result.run_manifest = Some(manifest_path.to_string_lossy().into_owned());
    result.run_record = Some(record_path.to_string_lossy().into_owned());
    result.artifact_locations = Some(BTreeMap::from([
        (
            "experiment_report".into(),
            report_path.to_string_lossy().into_owned(),
        ),
        (
            "run_manifest".into(),
            manifest_path.to_string_lossy().into_owned(),
        ),
        ("experiment_input".into(), spec_artifact_path.to_owned()),
    ]));

    let report_payload = result.to_json()?;
    let manifest_payload = manifest
        .to_json()
        .map_err(|error| invariant(format!("多市场实验 RunManifest 编码失败: {error}")))?;
    let report_bytes = report_payload.as_bytes();
    let manifest_bytes = manifest_payload.as_bytes();
    let record = RunRecordV2 {
        schema_version: qx_spec::RUN_RECORD_V2_SCHEMA_VERSION,
        run_id: result.experiment_id.clone(),
        status: RunStatus::Completed,
        input_digest: result
            .input_data_fingerprint
            .clone()
            .ok_or_else(|| invariant("多市场实验输入指纹缺失"))?,
        code_identity: format!("{}:{}", manifest.code_commit, manifest.runtime_version),
        config_fingerprint: result
            .canonical_run_spec_fingerprint
            .clone()
            .ok_or_else(|| invariant("多市场实验 CanonicalRunSpec 指纹缺失"))?,
        started_at_ms: manifest.clock_start,
        finished_at_ms: Some(manifest.clock_end),
        artifact_refs: vec![
            ArtifactRefV2 {
                name: "experiment_report".into(),
                artifact_id: qx_strategy::sha256_hex(report_bytes),
                kind: "run-multi-market-experiment-result-json".into(),
                schema_version: super::RUN_MULTI_MARKET_EXPERIMENT_SCHEMA_VERSION,
                byte_len: report_bytes.len() as u64,
            },
            ArtifactRefV2 {
                name: "run_manifest".into(),
                artifact_id: qx_strategy::sha256_hex(manifest_bytes),
                kind: "run-manifest-json".into(),
                schema_version: 1,
                byte_len: manifest_bytes.len() as u64,
            },
            ArtifactRefV2 {
                name: "experiment_input".into(),
                artifact_id: qx_strategy::sha256_hex(&spec_bytes),
                kind: "run-experiment-input-json".into(),
                schema_version: 1,
                byte_len: spec_bytes.len() as u64,
            },
        ],
        replay_verdict: ReplayVerdict::Verified,
        capability_level: qx_spec::CapabilityLevel::L1,
    };
    let record_payload = record
        .to_json()
        .map_err(|error| invariant(format!("多市场实验 RunRecord V2 编码失败: {error}")))?;
    publish_artifact(&record_path, &record_payload, "RunRecord V2")?;
    publish_artifact(&report_path, &report_payload, "实验报告")?;
    super::write_text(report_index_path, &report_payload)?;
    Ok(result)
}

pub(super) fn verify_persisted_result(
    result: &RunMultiMarketExperimentResult,
) -> Result<(), AppError> {
    let summary: Value = serde_json::from_str(&result.to_json()?)
        .map_err(|error| invariant(format!("多市场实验结果读回 JSON 无效: {error}")))?;
    crate::artifact_store::verify_declared_run_manifest(&summary).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("多市场实验 RunManifest/RunRecord 读回失败: {error}"),
        )
    })
}

fn build_manifest(
    experiment_id: &str,
    canonical: &CanonicalRunSpec,
    result_hash: &str,
) -> Result<RunManifest, AppError> {
    let runtime_version = canonical
        .execution_model
        .get("runtime_version")
        .and_then(Value::as_str)
        .ok_or_else(|| invariant("多市场实验执行模型缺少 runtime_version"))?;
    let global_seed = canonical
        .random_seed
        .parse::<u64>()
        .map_err(|error| invalid(format!("多市场实验随机种子无效: {error}")))?;
    let data_fingerprint = canonical
        .extensions
        .get("input_data_fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| invariant("多市场实验 CanonicalRunSpec 缺少输入指纹"))?;
    let input_components = canonical
        .extensions
        .iter()
        .filter_map(|(name, value)| {
            name.strip_prefix("input_component:").and_then(|key| {
                value
                    .as_str()
                    .map(|fingerprint| (key.to_owned(), fingerprint.to_owned()))
            })
        })
        .collect::<BTreeMap<_, _>>();
    if input_components.len() < canonical.datasets.len()
        || canonical.datasets.iter().any(|dataset| {
            !input_components.contains_key(&format!("barframe:{}", dataset.instrument))
        })
    {
        return Err(invariant("多市场实验输入组件与数据集数量不一致"));
    }
    let model_bytes = serde_json::to_vec(&canonical.execution_model)
        .map_err(|error| invariant(format!("多市场实验执行模型编码失败: {error}")))?;
    let canonical_json = canonical
        .canonical_json()
        .map_err(|error| invariant(format!("多市场实验 CanonicalRunSpec 编码失败: {error}")))?;
    let canonical_fingerprint = canonical
        .fingerprint()
        .map_err(|error| invariant(format!("多市场实验 CanonicalRunSpec 指纹失败: {error}")))?;
    Ok(RunManifest {
        run_id: experiment_id.to_owned(),
        code_commit: canonical.code_revision.clone(),
        config_hash: canonical_fingerprint.clone(),
        data_fingerprint: data_fingerprint.to_owned(),
        input_components,
        clock_start: canonical.clock.start_ts_ms,
        clock_end: canonical.clock.end_ts_ms,
        global_seed,
        determinism_mode: true,
        result_hash: result_hash.to_owned(),
        strategy_version: "multi-market-experiment-strategy-set-v1".into(),
        instrument_spec_version: "multi-market-experiment-v1".into(),
        model_fingerprint: qx_strategy::sha256_hex(&model_bytes),
        input_event_hash: data_fingerprint.to_owned(),
        output_event_hash: result_hash.to_owned(),
        runtime_version: runtime_version.to_owned(),
        canonical_run_spec_json: Some(canonical_json),
        canonical_run_spec_fingerprint: Some(canonical_fingerprint),
    })
}

fn publish_artifact(path: &Path, payload: &str, label: &str) -> Result<(), AppError> {
    crate::artifact_store::sink::publish(
        path,
        payload,
        crate::artifact_store::sink::PublishMode::Idempotent,
    )
    .map_err(|error| match error {
        crate::artifact_store::sink::PublishError::Conflict(message) => {
            AppError::new(AppErrorCategory::Conflict, message)
        }
        crate::artifact_store::sink::PublishError::Io(error) => {
            AppError::from_io(&format!("发布多市场实验 {label}"), &error)
        }
    })
}

pub(super) fn write_input_spec(path: &str, payload: &str) -> Result<(), AppError> {
    write_spec_artifact(path, payload, "多市场实验")
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn invariant(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}
