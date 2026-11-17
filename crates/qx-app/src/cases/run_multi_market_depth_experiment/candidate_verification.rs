use super::super::artifacts::write_text;
use super::{
    invariant, persist_multi_market_depth_experiment_result,
    verify_persisted_multi_market_depth_experiment_result, RunMultiMarketDepthExperimentResult,
};
use crate::{AppError, AppErrorCategory, MultiMarketDepthBacktestSpec};
use qx_spec::CanonicalRunSpec;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub(super) fn verify_multi_market_depth_experiment_candidates(
    result: &RunMultiMarketDepthExperimentResult,
    expected_data_fingerprint: &str,
    canonical: &CanonicalRunSpec,
    artifact_root: &Path,
) -> Result<(), AppError> {
    let reader = crate::artifact_store::FileArtifactReader;
    let root = artifact_root
        .canonicalize()
        .map_err(|error| AppError::from_io("解析多市场盘口实验产物根目录", &error))?;
    let mut verified_run_ids = BTreeSet::new();
    for candidate in &result.candidates {
        let Some(outcome) = candidate.outcome.as_ref() else {
            continue;
        };
        if !verified_run_ids.insert(outcome.run_id.as_str()) {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场盘口实验重复引用候选运行 {}", outcome.run_id),
            ));
        }
        if outcome.data_fingerprint != expected_data_fingerprint
            || outcome.markets_total != canonical.datasets.len()
        {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!(
                    "多市场盘口实验候选 {} 输入身份与父实验不一致",
                    outcome.run_id
                ),
            ));
        }
        let artifact_path = rooted_candidate_path(&root, &outcome.artifact_path)?;
        let payload = std::fs::read_to_string(&artifact_path)
            .map_err(|error| AppError::from_io("读取多市场盘口实验候选产物", &error))?;
        let artifact: serde_json::Value = serde_json::from_str(&payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("多市场盘口实验候选 {} JSON 无效: {error}", outcome.run_id),
            )
        })?;
        let stored_outcome: crate::MultiMarketDepthBacktestOutcome = serde_json::from_value(
            artifact
                .get("outcome")
                .cloned()
                .ok_or_else(|| invariant("多市场深度候选产物缺少 outcome"))?,
        )
        .map_err(|error| invariant(format!("多市场深度候选 outcome 无效: {error}")))?;
        if &stored_outcome != outcome {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!(
                    "多市场盘口实验候选 {} 摘要与持久化产物不一致",
                    outcome.run_id
                ),
            ));
        }
        let manifest_path = artifact
            .get("run_manifest_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invariant("多市场盘口候选产物缺少 RunManifest 路径"))?;
        let manifest_path = rooted_candidate_path(&root, manifest_path)?;
        let manifest_payload = std::fs::read_to_string(&manifest_path)
            .map_err(|error| AppError::from_io("读取多市场盘口候选 RunManifest", &error))?;
        let manifest = qx_core::RunManifest::from_json(&manifest_payload)
            .map_err(|error| invariant(format!("多市场盘口候选 RunManifest 无效: {error}")))?;
        crate::artifact_store::verify_canonical_run_spec_for_manifest(&manifest, &artifact)
            .map_err(|error| {
                AppError::new(
                    AppErrorCategory::Conflict,
                    format!(
                        "多市场盘口实验候选 {} 身份核验失败: {error}",
                        outcome.run_id
                    ),
                )
            })?;
        let candidate_canonical = manifest
            .canonical_run_spec_json
            .as_deref()
            .ok_or_else(|| invariant("多市场盘口候选 RunManifest 缺少 CanonicalRunSpec"))
            .and_then(|json| {
                CanonicalRunSpec::from_json(json).map_err(|error| {
                    invariant(format!("多市场盘口候选 CanonicalRunSpec 无效: {error}"))
                })
            })?;
        if candidate_canonical.run_type != "multi_market_depth_backtest"
            || candidate_canonical.datasets != canonical.datasets
            || candidate_canonical.market_rules != canonical.market_rules
            || candidate_canonical.clock != canonical.clock
            || candidate_canonical.execution_model != canonical.execution_model
        {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!(
                    "多市场盘口实验候选 {} 未绑定父实验的数据/规则/执行环境",
                    outcome.run_id
                ),
            ));
        }
        if artifact
            .get("replay_verified")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场盘口实验候选 {} 未声明已验证重放", outcome.run_id),
            ));
        }
        let event_trace_path = artifact
            .get("event_trace_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invariant("多市场盘口候选产物缺少 EventLog 轨迹"))?;
        let event_trace_path = rooted_candidate_path(&root, event_trace_path)?;
        let event_count = artifact
            .get("event_count")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| invariant("多市场盘口候选产物缺少 event_count"))?;
        let ledger_entries = artifact
            .get("ledger_entries")
            .ok_or_else(|| invariant("多市场盘口候选产物缺少重放账簿"))?;
        let candidate_spec: MultiMarketDepthBacktestSpec = serde_json::from_value(
            artifact
                .get("spec")
                .cloned()
                .ok_or_else(|| invariant("多市场盘口候选产物缺少 spec"))?,
        )
        .map_err(|error| invariant(format!("多市场盘口候选 spec 无效: {error}")))?;
        let strategy_markets = candidate_spec
            .markets
            .iter()
            .map(|market| {
                (
                    market.strategy.strategy_id.clone(),
                    market.instrument.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let replay = crate::read_multi_market_event_report_v1_with_reader(
            event_trace_path.to_string_lossy().as_ref(),
            event_count,
            &manifest.output_event_hash,
            ledger_entries,
            &strategy_markets,
            None,
            &reader,
        )
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::Conflict,
                format!(
                    "多市场盘口实验候选 {} EventLog 重放失败: {error}",
                    outcome.run_id
                ),
            )
        })?;
        if replay.total != event_count {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场盘口实验候选 {} EventLog 摘要不一致", outcome.run_id),
            ));
        }
    }
    Ok(())
}

fn rooted_candidate_path(root: &Path, reference: &str) -> Result<PathBuf, AppError> {
    let path = Path::new(reference)
        .canonicalize()
        .map_err(|error| AppError::from_io("解析多市场盘口候选产物路径", &error))?;
    if !path.starts_with(root) || !path.is_file() {
        return Err(AppError::new(
            AppErrorCategory::StorageFailure,
            format!("多市场盘口候选产物超出实验根目录或不是文件: {reference}"),
        ));
    }
    Ok(path)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn existing_result(
    payload: &str,
    fingerprint: &str,
    canonical_json: &str,
    canonical_fingerprint: &str,
    input_data_fingerprint: &str,
    experiment_id: &str,
    canonical: &CanonicalRunSpec,
    root: &Path,
    spec_artifact_path: &str,
    canonical_spec_artifact: &str,
) -> Result<RunMultiMarketDepthExperimentResult, AppError> {
    let mut existing = RunMultiMarketDepthExperimentResult::from_json(payload)?;
    let canonical_pair = match (
        existing.canonical_run_spec_json.as_deref(),
        existing.canonical_run_spec_fingerprint.as_deref(),
    ) {
        (Some(json), Some(hash)) => Some((json, hash)),
        (None, None) => None,
        _ => {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                "既有多市场盘口实验 CanonicalRunSpec JSON/fingerprint 不完整",
            ));
        }
    };
    let same_identity = canonical_pair
        .map_or(existing.spec_fingerprint == fingerprint, |(_, hash)| {
            hash == canonical_fingerprint
        });
    if !same_identity || existing.experiment_id != experiment_id {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场盘口实验身份已存在且规格不同",
        ));
    }
    if canonical_pair.is_some_and(|(json, _)| json != canonical_json)
        || existing
            .input_data_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| fingerprint != input_data_fingerprint)
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "既有多市场盘口实验已绑定不同数据或 CanonicalRunSpec",
        ));
    }
    if existing.to_json()? != payload {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "既有多市场盘口实验报告与规范序列化不一致",
        ));
    }
    verify_multi_market_depth_experiment_candidates(
        &existing,
        input_data_fingerprint,
        canonical,
        root,
    )?;
    let envelope_fields = [
        existing.result_hash.is_some(),
        existing.run_manifest.is_some(),
        existing.run_record.is_some(),
        existing.artifact_locations.is_some(),
    ];
    if envelope_fields.iter().all(|present| *present) {
        if existing.canonical_run_spec_json.as_deref() != Some(canonical_json)
            || existing.canonical_run_spec_fingerprint.as_deref() != Some(canonical_fingerprint)
            || existing.input_data_fingerprint.as_deref() != Some(input_data_fingerprint)
        {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                "已发布的多市场盘口实验缺少或错配标准身份",
            ));
        }
        verify_persisted_multi_market_depth_experiment_result(&existing)?;
        let has_input_spec = existing
            .artifact_locations
            .as_ref()
            .is_some_and(|locations| locations.contains_key("experiment_input"));
        if has_input_spec {
            super::super::experiment_spec_artifact::verify_spec_artifact(
                spec_artifact_path,
                canonical_spec_artifact,
                "多市场盘口实验",
            )?;
            return Ok(existing);
        }
    }
    if envelope_fields.iter().any(|present| *present)
        && !envelope_fields.iter().all(|present| *present)
    {
        return Err(AppError::new(
            AppErrorCategory::StorageFailure,
            "既有多市场盘口实验的 result_hash/manifest/record/artifact_locations 必须完整或全部缺省",
        ));
    }
    existing.canonical_run_spec_json = Some(canonical_json.to_owned());
    existing.canonical_run_spec_fingerprint = Some(canonical_fingerprint.to_owned());
    existing.input_data_fingerprint = Some(input_data_fingerprint.to_owned());
    super::super::experiment_spec_artifact::write_spec_artifact(
        spec_artifact_path,
        canonical_spec_artifact,
        "多市场盘口实验",
    )?;
    let report_index_path = existing.artifact_path.clone();
    let upgraded = persist_multi_market_depth_experiment_result(
        existing,
        canonical,
        root,
        &report_index_path,
        spec_artifact_path,
    )?;
    write_text(&upgraded.artifact_path, &upgraded.to_json()?)?;
    verify_persisted_multi_market_depth_experiment_result(&upgraded)?;
    Ok(upgraded)
}
