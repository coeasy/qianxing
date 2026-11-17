//! Experiment artifact publication and verification.

use super::*;

pub(super) fn persist_experiment_result(
    mut result: RunExperimentResult,
    canonical_spec: &CanonicalRunSpec,
    root: &Path,
    report_index_path: &str,
    spec_artifact_path: &str,
) -> Result<RunExperimentResult, AppError> {
    let result_hash = experiment_result_hash(&result)?;
    result.result_hash = Some(result_hash.clone());
    let manifest =
        build_experiment_run_manifest(&result.experiment_id, canonical_spec, &result_hash)?;
    let manifest_path = persist_run_manifest(root, &manifest)?;
    let report_path = root.join(format!(
        "{}-{result_hash}-input-v1.experiment.json",
        result.experiment_id
    ));
    let record_path = root.join(format!(
        "{}-{result_hash}-input-v1.record.json",
        result.experiment_id
    ));
    // Keep artifact_path as the stable read/write index. The immutable report is exposed by the
    // RunRecord-backed artifact_locations map.
    result.artifact_path = report_index_path.to_owned();
    let manifest_path = manifest_path.to_string_lossy().into_owned();
    result.run_manifest = Some(manifest_path.clone());
    result.run_record = Some(record_path.to_string_lossy().into_owned());
    result.artifact_locations = Some(BTreeMap::from([
        (
            "experiment_report".into(),
            report_path.to_string_lossy().into_owned(),
        ),
        ("run_manifest".into(), manifest_path),
        ("experiment_input".into(), spec_artifact_path.to_owned()),
    ]));

    let report_payload = result.to_json()?;
    let manifest_payload = manifest.to_json().map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("参数实验 RunManifest 编码失败: {error}"),
        )
    })?;
    let report_bytes = report_payload.as_bytes();
    let manifest_bytes = manifest_payload.as_bytes();
    let spec_bytes = std::fs::read(spec_artifact_path)
        .map_err(|error| AppError::from_io("读取参数实验输入规格 sidecar", &error))?;
    let record = RunRecordV2 {
        schema_version: qx_spec::RUN_RECORD_V2_SCHEMA_VERSION,
        run_id: result.experiment_id.clone(),
        status: RunStatus::Completed,
        input_digest: result
            .input_data_fingerprint
            .clone()
            .ok_or_else(|| experiment_storage_error("实验输入数据指纹缺失"))?,
        code_identity: format!("{}:{}", manifest.code_commit, manifest.runtime_version),
        config_fingerprint: result
            .canonical_run_spec_fingerprint
            .clone()
            .ok_or_else(|| experiment_storage_error("实验 CanonicalRunSpec 指纹缺失"))?,
        started_at_ms: manifest.clock_start,
        finished_at_ms: Some(manifest.clock_end),
        artifact_refs: vec![
            ArtifactRefV2 {
                name: "experiment_report".into(),
                artifact_id: qx_strategy::sha256_hex(report_bytes),
                kind: "run-experiment-result-json".into(),
                schema_version: RUN_EXPERIMENT_SCHEMA_VERSION,
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
    let record_payload = record.to_json().map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("参数实验 RunRecord V2 编码失败: {error}"),
        )
    })?;
    let readback = RunRecordV2::from_json(&record_payload).map_err(|error| {
        experiment_storage_error(format!("参数实验 RunRecord V2 无效: {error}"))
    })?;
    if readback != record {
        return Err(experiment_storage_error(
            "参数实验 RunRecord V2 规范化读回与写入对象不一致",
        ));
    }

    std::fs::create_dir_all(root)
        .map_err(|error| AppError::from_io("创建参数实验产物目录", &error))?;
    let record_publish = crate::artifact_store::sink::publish(
        &record_path,
        &record_payload,
        crate::artifact_store::sink::PublishMode::Idempotent,
    );
    map_experiment_publish_result(record_publish, "参数实验 RunRecord V2")?;
    let report_publish = crate::artifact_store::sink::publish(
        &report_path,
        &report_payload,
        crate::artifact_store::sink::PublishMode::Idempotent,
    );
    map_experiment_publish_result(report_publish, "参数实验报告")?;
    write_text(report_index_path, &report_payload)?;
    Ok(result)
}

pub(crate) fn build_experiment_run_manifest(
    experiment_id: &str,
    canonical_spec: &CanonicalRunSpec,
    result_hash: &str,
) -> Result<RunManifest, AppError> {
    let dataset = canonical_spec
        .datasets
        .first()
        .ok_or_else(|| experiment_storage_error("实验 CanonicalRunSpec 缺少数据身份"))?;
    let runtime_version = canonical_spec
        .execution_model
        .get("runtime_version")
        .and_then(Value::as_str)
        .ok_or_else(|| experiment_storage_error("实验执行模型缺少 runtime_version"))?;
    let global_seed = canonical_spec
        .random_seed
        .parse::<u64>()
        .map_err(|error| experiment_storage_error(format!("实验随机种子无效: {error}")))?;
    let model_bytes = serde_json::to_vec(&canonical_spec.execution_model)
        .map_err(|error| experiment_storage_error(format!("实验执行模型规范化失败: {error}")))?;
    let instrument_spec_version = canonical_spec
        .market_rules
        .get(&dataset.instrument)
        .cloned()
        .unwrap_or_else(|| "legacy-default-v1".into());
    let strategy_version = if canonical_spec.run_type == "depth_experiment" {
        "depth-experiment-strategy-set-v1".into()
    } else if canonical_spec.strategies.len() == 1 {
        canonical_spec.strategies[0].strategy_version.clone()
    } else {
        "experiment-strategy-set-v1".into()
    };
    let canonical_json = canonical_spec.canonical_json().map_err(|error| {
        experiment_storage_error(format!("实验 CanonicalRunSpec 规范化失败: {error}"))
    })?;
    let canonical_fingerprint = canonical_spec.fingerprint().map_err(|error| {
        experiment_storage_error(format!("实验 CanonicalRunSpec 指纹失败: {error}"))
    })?;
    let input_components = canonical_spec
        .datasets
        .iter()
        .map(|dataset| (dataset.role.clone(), dataset.content_fingerprint.clone()))
        .collect::<BTreeMap<_, _>>();
    Ok(RunManifest {
        run_id: experiment_id.to_owned(),
        code_commit: canonical_spec.code_revision.clone(),
        config_hash: canonical_fingerprint.clone(),
        data_fingerprint: dataset.content_fingerprint.clone(),
        input_components,
        clock_start: canonical_spec.clock.start_ts_ms,
        clock_end: canonical_spec.clock.end_ts_ms,
        global_seed,
        determinism_mode: true,
        result_hash: result_hash.to_owned(),
        strategy_version,
        instrument_spec_version,
        model_fingerprint: qx_strategy::sha256_hex(&model_bytes),
        input_event_hash: dataset.content_fingerprint.clone(),
        output_event_hash: result_hash.to_owned(),
        runtime_version: runtime_version.to_owned(),
        canonical_run_spec_json: Some(canonical_json),
        canonical_run_spec_fingerprint: Some(canonical_fingerprint),
    })
}

pub(super) fn verify_experiment_candidate_outcomes(
    result: &RunExperimentResult,
    context: &RunContext,
) -> Result<(), AppError> {
    let mut run_ids = BTreeSet::new();
    for candidate in &result.candidates {
        if let Some(outcome) = &candidate.outcome {
            verify_one_experiment_outcome(outcome, context, &mut run_ids)?;
        }
    }
    for scenario in &result.cost_sensitivity {
        verify_cost_sensitivity_outcomes(scenario, context, &mut run_ids)?;
    }
    if let Some(search) = &result.search {
        for fold in &search.fold_searches {
            for candidate in &fold.candidates {
                if let Some(outcome) = &candidate.outcome {
                    verify_one_experiment_outcome(outcome, context, &mut run_ids)?;
                }
            }
            for scenario in &fold.cost_sensitivity {
                verify_cost_sensitivity_outcomes(scenario, context, &mut run_ids)?;
            }
        }
    }
    Ok(())
}

fn verify_cost_sensitivity_outcomes(
    scenario: &ExecutionCostSensitivityResult,
    context: &RunContext,
    verified_run_ids: &mut BTreeSet<String>,
) -> Result<(), AppError> {
    if let Some(outcome) = &scenario.outcome {
        verify_one_experiment_outcome(outcome, context, verified_run_ids)?;
    }
    if let Some(neighborhood) = &scenario.neighborhood_cost_robustness {
        for candidate in &neighborhood.candidates {
            if let Some(outcome) = &candidate.outcome {
                verify_one_experiment_outcome(outcome, context, verified_run_ids)?;
            }
        }
    }
    Ok(())
}

fn verify_one_experiment_outcome(
    outcome: &BacktestOutcome,
    context: &RunContext,
    verified_run_ids: &mut BTreeSet<String>,
) -> Result<(), AppError> {
    if !verified_run_ids.insert(outcome.run_id.clone()) {
        return Ok(());
    }
    let verification = verify_run(outcome, context)?;
    if !verification.verified {
        return Err(experiment_storage_error(format!(
            "实验候选 {} 产物复核失败: {}",
            outcome.run_id,
            verification.mismatches.join("; ")
        )));
    }
    Ok(())
}

pub(super) fn verify_persisted_experiment_result(
    result: &RunExperimentResult,
) -> Result<(), AppError> {
    let value = serde_json::json!({
        "schema_version": result.schema_version,
        "experiment_id": result.experiment_id,
        "spec_fingerprint": result.spec_fingerprint,
        "canonical_run_spec_json": result.canonical_run_spec_json,
        "canonical_run_spec_fingerprint": result.canonical_run_spec_fingerprint,
        "input_data_fingerprint": result.input_data_fingerprint,
        "result_hash": result.result_hash,
        "run_manifest": result.run_manifest,
        "run_record": result.run_record,
        "artifact_locations": result.artifact_locations,
        "artifact_path": result.artifact_path,
    });
    let reader = crate::artifact_store::FileArtifactReader;
    crate::artifact_store::verify_declared_run_record_with_reader(&value, &reader).map_err(
        |error| experiment_storage_error(format!("参数实验 RunRecord V2 复核失败: {error}")),
    )?;
    crate::artifact_store::verify_declared_run_manifest_with_reader(&value, &reader).map_err(
        |error| experiment_storage_error(format!("参数实验 RunManifest 复核失败: {error}")),
    )
}

fn map_experiment_publish_result(
    result: Result<(), crate::artifact_store::sink::PublishError>,
    label: &str,
) -> Result<(), AppError> {
    result.map_err(|error| match error {
        crate::artifact_store::sink::PublishError::Conflict(message) => AppError::new(
            AppErrorCategory::Conflict,
            format!("{label}冲突: {message}"),
        ),
        crate::artifact_store::sink::PublishError::Io(error) => {
            AppError::from_io(&format!("发布{label}"), &error)
        }
    })
}
