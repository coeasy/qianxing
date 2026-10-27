use qx_core::{Fnv1a, RunManifest};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

mod portable_bundle;
pub(crate) use portable_bundle::{
    pack_bar_experiment, pack_depth_experiment, pack_multi_leg_experiment,
    pack_multi_market_experiment, verify_bar_experiment_bundle, verify_depth_experiment_bundle,
    verify_multi_leg_experiment_bundle, verify_multi_market_experiment_bundle,
};

/// Re-run every builtin candidate in a single-market Bar experiment from its persisted input
/// specification. External strategy material must be supplied by a later, explicit build
/// attestation flow; never substitute a builtin strategy for it.
pub(crate) fn recompute_bar_experiment(artifact: &Path) -> Result<(), String> {
    let (report, input) = read_experiment(artifact, ".experiment.json")?;
    let spec_json = input.exact_spec_json()?;
    let spec = qx_app::RunExperimentSpec::from_json(&spec_json)
        .map_err(|error| format!("Bar 实验规格无效: {error}"))?;
    ensure_matching_build(&input)?;
    let fingerprint = spec_fingerprint(
        &spec.to_json().map_err(|error| error.to_json())?,
        input.strategy_identity.as_ref(),
    );
    if input.strategy_identity.is_some() {
        return Err("实验绑定了外部策略；缺少第三方策略构建材料，拒绝 builtin 代替复算".into());
    }
    check_report_identity(&report, &spec.experiment_id, &fingerprint)?;
    let original: qx_app::RunExperimentResult = serde_json::from_value(report.clone())
        .map_err(|error| format!("Bar 实验报告无效: {error}"))?;
    verify_bar_experiment_outcomes(&original, "原始")?;
    with_isolated_output(|output| {
        let mut isolated = spec.clone();
        isolated.base.output_dir = output.to_string_lossy().into_owned();
        let context = qx_app::RunContext::new(
            qx_app::CallerCapability::Research,
            isolated.experiment_id.clone(),
        )
        .with_code_commit(crate::build_identity::BUILD_REVISION);
        let rerun = qx_app::run_experiment(&isolated, &context).map_err(|error| error.to_json())?;
        verify_bar_experiment_outcomes(&rerun, "复算")?;
        let rerun = serde_json::to_value(rerun).map_err(|error| error.to_string())?;
        compare_single_market_experiment(&report, &rerun, "Bar")
    })?;
    println!(
        "[Experiment · Recompute] Bar 实验全部 {} 个候选已复算一致",
        original.total_candidates
    );
    Ok(())
}

/// Re-run every builtin candidate in a single-market Tick/OrderBook experiment.
pub(crate) fn recompute_depth_experiment(artifact: &Path) -> Result<(), String> {
    let (report, input) = read_experiment(artifact, ".depth-experiment.json")?;
    let spec_json = input.exact_spec_json()?;
    let spec = qx_app::RunDepthExperimentSpec::from_json(&spec_json)
        .map_err(|error| format!("深度实验规格无效: {error}"))?;
    ensure_matching_build(&input)?;
    let fingerprint = spec_fingerprint(
        &spec.to_json().map_err(|error| error.to_json())?,
        input.strategy_identity.as_ref(),
    );
    if input.strategy_identity.is_some() {
        return Err("实验绑定了外部策略；缺少第三方策略构建材料，拒绝 builtin 代替复算".into());
    }
    check_report_identity(&report, &spec.experiment_id, &fingerprint)?;
    let original: qx_app::RunDepthExperimentResult = serde_json::from_value(report.clone())
        .map_err(|error| format!("深度实验报告无效: {error}"))?;
    verify_depth_experiment_outcomes(&original, "原始")?;
    with_isolated_output(|output| {
        let mut isolated = spec.clone();
        isolated.base.output_dir = output.to_string_lossy().into_owned();
        let context = qx_app::RunContext::new(
            qx_app::CallerCapability::Research,
            isolated.experiment_id.clone(),
        )
        .with_code_commit(crate::build_identity::BUILD_REVISION);
        let rerun =
            qx_app::run_depth_experiment(&isolated, &context).map_err(|error| error.to_json())?;
        verify_depth_experiment_outcomes(&rerun, "复算")?;
        let rerun = serde_json::to_value(rerun).map_err(|error| error.to_string())?;
        compare_single_market_experiment(&report, &rerun, "深度")
    })?;
    println!(
        "[Experiment · Recompute] 深度实验全部 {} 个候选已复算一致",
        original.total_candidates
    );
    Ok(())
}

fn verify_bar_experiment_outcomes(
    result: &qx_app::RunExperimentResult,
    phase: &str,
) -> Result<(), String> {
    for candidate in &result.candidates {
        let Some(outcome) = &candidate.outcome else {
            continue;
        };
        let context = qx_app::RunContext::new(
            qx_app::CallerCapability::Research,
            format!("{}:{}:{}", result.experiment_id, phase, candidate.ordinal),
        );
        let verification = qx_app::verify_run(outcome, &context).map_err(|error| {
            format!(
                "{phase} Bar 候选 {} 产物读取失败: {}",
                candidate.ordinal,
                error.to_json()
            )
        })?;
        if !verification.verified {
            return Err(format!(
                "{phase} Bar 候选 {} 产物未通过共享 reader 验证: {}",
                candidate.ordinal,
                verification.mismatches.join("; ")
            ));
        }
    }
    Ok(())
}

fn verify_depth_experiment_outcomes(
    result: &qx_app::RunDepthExperimentResult,
    phase: &str,
) -> Result<(), String> {
    for candidate in &result.candidates {
        let Some(outcome) = &candidate.outcome else {
            continue;
        };
        let context = qx_app::RunContext::new(
            qx_app::CallerCapability::Research,
            format!("{}:{}:{}", result.experiment_id, phase, candidate.ordinal),
        );
        let verification = qx_app::verify_depth_run(outcome, &context).map_err(|error| {
            format!(
                "{phase} 深度候选 {} 产物读取失败: {}",
                candidate.ordinal,
                error.to_json()
            )
        })?;
        if !verification.verified {
            return Err(format!(
                "{phase} 深度候选 {} 产物未通过共享 reader 验证: {}",
                candidate.ordinal,
                verification.mismatches.join("; ")
            ));
        }
    }
    Ok(())
}

fn compare_single_market_experiment(
    original: &Value,
    recomputed: &Value,
    label: &str,
) -> Result<(), String> {
    let mut original = original.clone();
    let mut recomputed = recomputed.clone();
    strip_relocated_experiment_paths(&mut original);
    strip_relocated_experiment_paths(&mut recomputed);
    if original != recomputed {
        let field = first_json_difference(&original, &recomputed, "$".into())
            .unwrap_or_else(|| "unknown".into());
        return Err(format!(
            "{label}实验复算报告与原报告不一致，首个差异: {field}"
        ));
    }
    Ok(())
}

fn first_json_difference(before: &Value, after: &Value, path: String) -> Option<String> {
    match (before, after) {
        (Value::Object(left), Value::Object(right)) => {
            let names = left
                .keys()
                .chain(right.keys())
                .collect::<std::collections::BTreeSet<_>>();
            for name in names {
                let child = format!("{path}.{name}");
                match (left.get(name), right.get(name)) {
                    (Some(left), Some(right)) => {
                        if let Some(difference) = first_json_difference(left, right, child) {
                            return Some(difference);
                        }
                    }
                    (Some(_), None) | (None, Some(_)) => return Some(child),
                    (None, None) => {}
                }
            }
            None
        }
        (Value::Array(left), Value::Array(right)) => {
            if left.len() != right.len() {
                return Some(format!("{path}.length"));
            }
            left.iter()
                .zip(right)
                .enumerate()
                .find_map(|(index, (left, right))| {
                    first_json_difference(left, right, format!("{path}[{index}]"))
                })
        }
        _ if before == after => None,
        _ => Some(path),
    }
}

fn strip_relocated_experiment_paths(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                strip_relocated_experiment_paths(value);
            }
        }
        Value::Object(fields) => {
            fields.retain(|name, _| {
                !matches!(
                    name.as_str(),
                    "artifact_path"
                        | "artifact_locations"
                        | "artifacts"
                        | "run_manifest"
                        | "run_record"
                        // This legacy fingerprint includes output_dir. The path-independent
                        // canonical_run_spec_fingerprint and input_data_fingerprint remain compared.
                        | "spec_fingerprint"
                ) && !name.ends_with("_path")
                    && !name.ends_with("_dir")
            });
            for value in fields.values_mut() {
                strip_relocated_experiment_paths(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

pub(crate) fn recompute_multi_market_experiment(artifact: &Path) -> Result<(), String> {
    let (report, input) = read_experiment(artifact, ".multi-market-experiment.json")?;
    let spec_json = input.exact_spec_json()?;
    let spec = qx_app::RunMultiMarketExperimentSpec::from_json(&spec_json)
        .map_err(|error| format!("多市场实验规格无效: {error}"))?;
    ensure_matching_build(&input)?;
    let fingerprint = spec_fingerprint(
        &spec.to_json().map_err(|error| error.to_string())?,
        input.strategy_identity.as_ref(),
    );
    if input.strategy_identity.is_some() {
        return Err("实验绑定了外部策略；缺少第三方策略构建材料，拒绝 builtin 代替复算".into());
    }
    check_report_identity(&report, &spec.experiment_id, &fingerprint)?;
    with_isolated_output(|output| {
        let mut isolated = spec.clone();
        isolated.base.output_dir = output.to_string_lossy().into_owned();
        let context = qx_app::RunContext::new(
            qx_app::CallerCapability::Research,
            isolated.experiment_id.clone(),
        )
        .with_code_commit(crate::build_identity::BUILD_REVISION);
        let rerun = qx_app::run_multi_market_experiment(&isolated, &context)
            .map_err(|error| error.to_json())
            .and_then(|result| serde_json::to_value(result).map_err(|error| error.to_string()))?;
        compare_experiment(&report, &rerun, "多市场 Bar", None)
    })
}

pub(crate) fn recompute_multi_market_depth_experiment(artifact: &Path) -> Result<(), String> {
    let (report, input) = read_experiment(artifact, ".multi-market-depth-experiment.json")?;
    let spec_json = input.exact_spec_json()?;
    let spec = qx_app::RunMultiMarketDepthExperimentSpec::from_json(&spec_json)
        .map_err(|error| format!("多市场深度实验规格无效: {error}"))?;
    ensure_matching_build(&input)?;
    let fingerprint = spec_fingerprint(
        &spec.to_json().map_err(|error| error.to_string())?,
        input.strategy_identity.as_ref(),
    );
    if input.strategy_identity.is_some() {
        return Err("实验绑定了外部策略；缺少第三方策略构建材料，拒绝 builtin 代替复算".into());
    }
    check_report_identity(&report, &spec.experiment_id, &fingerprint)?;
    with_isolated_output(|output| {
        let mut isolated = spec.clone();
        isolated.base.output_dir = output.to_string_lossy().into_owned();
        let context = qx_app::RunContext::new(
            qx_app::CallerCapability::Research,
            isolated.experiment_id.clone(),
        )
        .with_code_commit(crate::build_identity::BUILD_REVISION);
        let rerun = qx_app::run_multi_market_depth_experiment(&isolated, &context)
            .map_err(|error| error.to_json())
            .and_then(|result| serde_json::to_value(result).map_err(|error| error.to_string()))?;
        compare_experiment(&report, &rerun, "多市场深度", None)
    })
}

pub(crate) fn recompute_multi_leg_experiment(artifact: &Path) -> Result<(), String> {
    let (report, input) = read_experiment(artifact, ".multi-leg-experiment.json")?;
    let spec_json = input.exact_spec_json()?;
    let spec = qx_app::RunMultiLegExperimentSpec::from_json(&spec_json)
        .map_err(|error| format!("多腿实验规格无效: {error}"))?;
    ensure_matching_build(&input)?;
    let fingerprint = spec_fingerprint(
        &spec.to_json().map_err(|error| error.to_string())?,
        input.strategy_identity.as_ref(),
    );
    if input.strategy_identity.is_some() {
        return Err("实验绑定了外部策略；缺少第三方策略构建材料，拒绝 builtin 代替复算".into());
    }
    check_report_identity(&report, &spec.experiment_id, &fingerprint)?;
    with_isolated_output(|output| {
        let mut isolated = spec.clone();
        isolated.base.output_dir = output.to_string_lossy().into_owned();
        let context = qx_app::RunContext::new(
            qx_app::CallerCapability::Research,
            isolated.experiment_id.clone(),
        )
        .with_code_commit(crate::build_identity::BUILD_REVISION);
        let rerun = qx_app::run_multi_leg_experiment(&isolated, &context)
            .map_err(|error| error.to_json())
            .and_then(|result| {
                let payload = result.to_json().map_err(|error| error.to_json())?;
                serde_json::from_str(&payload).map_err(|error| error.to_string())
            })?;
        compare_multi_leg_experiment(&report, &rerun)
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedExperimentInput {
    schema_version: u32,
    code_commit: String,
    #[serde(default)]
    strategy_identity: Option<qx_app::StrategyArtifactIdentity>,
    spec: Value,
    #[serde(default)]
    spec_json: Option<String>,
}

impl PersistedExperimentInput {
    fn exact_spec_json(&self) -> Result<String, String> {
        match &self.spec_json {
            Some(payload) => Ok(payload.clone()),
            None => serde_json::to_string(&self.spec).map_err(|error| error.to_string()),
        }
    }
}

fn read_experiment(
    artifact: &Path,
    suffix: &str,
) -> Result<(Value, PersistedExperimentInput), String> {
    let payload = std::fs::read_to_string(artifact)
        .map_err(|error| format!("读取实验报告失败 {}: {error}", artifact.display()))?;
    let report: Value = serde_json::from_str(&payload).map_err(|error| error.to_string())?;
    let name = artifact
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| name.ends_with(suffix))
        .ok_or_else(|| format!("实验报告文件名必须以 {suffix} 结尾"))?;
    let spec_name = format!("{}.spec.json", name.trim_end_matches(".json"));
    let spec_path = artifact.with_file_name(spec_name);
    let input_text = std::fs::read_to_string(&spec_path)
        .map_err(|error| format!("读取实验规格 sidecar 失败 {}: {error}", spec_path.display()))?;
    let input: PersistedExperimentInput = serde_json::from_str(&input_text)
        .map_err(|error| format!("实验规格 sidecar 无效: {error}"))?;
    if input.schema_version != 1 {
        return Err(format!(
            "实验输入 sidecar schema_version={}，只支持 1",
            input.schema_version
        ));
    }
    Ok((report, input))
}

fn spec_fingerprint(
    canonical: &str,
    identity: Option<&qx_app::StrategyArtifactIdentity>,
) -> String {
    let mut hash = Fnv1a::new();
    hash.write_text(canonical);
    if let Some(identity) = identity {
        hash.write_text(&identity.strategy_id);
        hash.write_text(&identity.strategy_version);
        hash.write_text(&identity.source_sha256);
    }
    format!("{:016x}", hash.finish())
}

fn ensure_matching_build(input: &PersistedExperimentInput) -> Result<(), String> {
    if input.code_commit != crate::build_identity::BUILD_REVISION {
        return Err(format!(
            "实验规格记录的 code_commit {} 与当前构建 {} 不一致，拒绝复算",
            input.code_commit,
            crate::build_identity::BUILD_REVISION
        ));
    }
    Ok(())
}

fn check_report_identity(
    report: &Value,
    experiment_id: &str,
    fingerprint: &str,
) -> Result<(), String> {
    if report.get("experiment_id").and_then(Value::as_str) != Some(experiment_id)
        || report.get("spec_fingerprint").and_then(Value::as_str) != Some(fingerprint)
    {
        return Err(
            "报告身份与持久化规格不匹配；自定义策略身份未随 sidecar 保存时不能 builtin 代替复算"
                .into(),
        );
    }
    let total = report.get("total_candidates").and_then(Value::as_u64);
    let completed = report.get("completed_candidates").and_then(Value::as_u64);
    if total.is_none() || completed != total {
        return Err("实验报告候选矩阵不完整，拒绝将部分结果声明为完整复算".into());
    }
    Ok(())
}

fn with_isolated_output<T>(run: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
    let target = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .parent()
        .and_then(Path::parent)
        .ok_or("无法确定隔离复算目录")?
        .to_path_buf();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let work = target.join(format!(
        "qx-experiment-recompute-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&work).map_err(|error| error.to_string())?;
    let output = work.join("output");
    let result = run(&output);
    let cleanup = std::fs::remove_dir_all(&work)
        .map_err(|error| format!("清理实验复算目录失败 {}: {error}", work.display()));
    let result = result?;
    cleanup?;
    Ok(result)
}

fn compare_experiment(
    original: &Value,
    recomputed: &Value,
    label: &str,
    original_root: Option<&Path>,
) -> Result<(), String> {
    for field in [
        "experiment_id",
        "target_strategy_id",
        "total_candidates",
        "completed_candidates",
        "succeeded_candidates",
        "failed_candidates",
    ] {
        if original.get(field) != recomputed.get(field) {
            return Err(format!("{label}实验复算字段 {field} 不一致"));
        }
    }
    let original_candidates = original
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or("原始实验报告缺少 candidates")?;
    let rerun_candidates = recomputed
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or("复算实验报告缺少 candidates")?;
    if original_candidates.len() != rerun_candidates.len() {
        return Err(format!("{label}实验候选数不一致"));
    }
    for (index, (before, after)) in original_candidates.iter().zip(rerun_candidates).enumerate() {
        compare_candidate(before, after, index + 1, label)?;
        if before.get("outcome").is_some_and(|value| !value.is_null()) {
            let expected = read_candidate_manifest(before, original_root)?;
            let actual = read_candidate_manifest(after, None)?;
            if expected.code_commit != crate::build_identity::BUILD_REVISION {
                return Err(format!(
                    "候选 {} code_commit 与当前构建不匹配，拒绝复算",
                    index + 1
                ));
            }
            if actual != expected {
                return Err(format!("候选 {} RunManifest 复算不一致", index + 1));
            }
        }
    }
    if original.get("ranking") != recomputed.get("ranking") {
        return Err(format!("{label}实验排名复算不一致"));
    }
    println!(
        "{label}多市场实验全部 {} 个候选已复算一致",
        original_candidates.len()
    );
    Ok(())
}

fn compare_candidate(
    before: &Value,
    after: &Value,
    ordinal: usize,
    label: &str,
) -> Result<(), String> {
    let mut left = before.clone();
    let mut right = after.clone();
    for candidate in [&mut left, &mut right] {
        if let Some(outcome) = candidate.get_mut("outcome").and_then(Value::as_object_mut) {
            outcome.remove("artifact_path");
            outcome.remove("spec_fingerprint");
        }
    }
    if left != right {
        return Err(format!("{label}实验候选 {ordinal} 复算摘要不一致"));
    }
    Ok(())
}

fn read_candidate_manifest(candidate: &Value, root: Option<&Path>) -> Result<RunManifest, String> {
    let artifact = candidate
        .pointer("/outcome/artifact_path")
        .and_then(Value::as_str)
        .ok_or("成功候选缺少 artifact_path")?;
    let artifact_path = Path::new(artifact);
    let artifact_path = if artifact_path.is_relative() {
        root.unwrap_or_else(|| Path::new(".")).join(artifact_path)
    } else {
        artifact_path.to_path_buf()
    };
    let payload = std::fs::read_to_string(&artifact_path)
        .map_err(|error| format!("读取候选产物失败 {}: {error}", artifact_path.display()))?;
    let summary: Value = serde_json::from_str(&payload).map_err(|error| error.to_string())?;
    if summary
        .get("strategy_identities")
        .and_then(Value::as_array)
        .is_some_and(|identities| !identities.is_empty())
    {
        return Err("实验候选绑定外部策略；缺少第三方构建材料，拒绝 builtin 代替复算".into());
    }
    let manifest_path = summary
        .get("run_manifest_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!(
                "候选产物缺少 run_manifest_path: {}",
                artifact_path.display()
            )
        })?;
    let manifest_path = Path::new(manifest_path);
    let manifest_path = if manifest_path.is_relative() {
        artifact_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(manifest_path)
    } else {
        manifest_path.to_path_buf()
    };
    let manifest =
        RunManifest::from_json(&std::fs::read_to_string(&manifest_path).map_err(|error| {
            format!(
                "读取候选 RunManifest 失败 {}: {error}",
                manifest_path.display()
            )
        })?)
        .map_err(|error| error.to_string())?;
    verify_candidate_replay(&summary, &manifest, &artifact_path)?;
    Ok(manifest)
}

fn verify_candidate_replay(
    summary: &Value,
    manifest: &RunManifest,
    artifact_path: &Path,
) -> Result<(), String> {
    if summary.get("replay_verified").and_then(Value::as_bool) != Some(true) {
        return Err("候选产物没有 replay_verified=true".into());
    }
    if summary.get("schema_version").and_then(Value::as_u64) == Some(2) {
        if summary.get("event_log").is_some() {
            return Err("多市场 v2 候选产物不得内嵌完整 event_log".into());
        }
        let event_count = summary
            .get("event_count")
            .and_then(Value::as_u64)
            .ok_or("多市场 v2 候选产物缺少 event_count")?;
        let event_trace = summary
            .get("event_trace_path")
            .and_then(Value::as_str)
            .ok_or("多市场 v2 候选产物缺少 event_trace_path")?;
        let event_trace = Path::new(event_trace);
        let event_trace = if event_trace.is_relative() {
            artifact_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(event_trace)
        } else {
            event_trace.to_path_buf()
        };
        let spec: qx_app::MultiMarketBacktestSpec = serde_json::from_value(
            summary
                .get("spec")
                .cloned()
                .ok_or("多市场 v2 候选产物缺少 spec")?,
        )
        .map_err(|error| format!("多市场 v2 候选 spec 无效: {error}"))?;
        let strategy_markets = spec
            .markets
            .iter()
            .flat_map(|market| {
                market.strategies.iter().map(move |member| {
                    (
                        member.strategy.strategy_id.clone(),
                        market.instrument.clone(),
                    )
                })
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let report = qx_app::read_multi_market_event_report_v1_with_reader(
            &event_trace.to_string_lossy(),
            event_count,
            &manifest.output_event_hash,
            summary
                .get("ledger_entries")
                .ok_or("多市场 v2 候选产物缺少 ledger_entries")?,
            &strategy_markets,
            Some((0, 1)),
            &qx_app::artifact_store::FileArtifactReader,
        )
        .map_err(|error| format!("多市场 v2 候选 JSONL EventLog 复核失败: {error}"))?;
        if manifest.output_event_hash != manifest.result_hash
            || summary
                .pointer("/outcome/shared_result_hash")
                .and_then(Value::as_str)
                != Some(manifest.result_hash.as_str())
            || report.total != event_count
        {
            return Err("候选 JSONL EventLog、账簿、Outcome 与 RunManifest 不一致".into());
        }
        return Ok(());
    }
    let events: Vec<qx_core::Event> = serde_json::from_value(
        summary
            .get("event_log")
            .cloned()
            .ok_or("候选产物缺少 event_log")?,
    )
    .map_err(|error| format!("候选 event_log 无效: {error}"))?;
    let (replayed, ledger) =
        qx_core::ReplayVerifier::replay_owned(events).map_err(|error| error.to_string())?;
    replayed
        .validate()
        .map_err(|error| format!("候选重放 EventLog 无效: {error}"))?;
    let replayed_ledger = serde_json::to_value(ledger.entries())
        .map_err(|error| format!("序列化候选重放账簿失败: {error}"))?;
    if summary.get("ledger_entries") != Some(&replayed_ledger)
        || format!("{:016x}", replayed.digest()) != manifest.result_hash
        || summary
            .pointer("/outcome/shared_result_hash")
            .and_then(Value::as_str)
            != Some(manifest.result_hash.as_str())
    {
        return Err("候选 EventLog、账簿、Outcome 与 RunManifest 不一致".into());
    }
    Ok(())
}

fn compare_multi_leg_experiment(original: &Value, recomputed: &Value) -> Result<(), String> {
    for field in [
        "experiment_id",
        "total_candidates",
        "completed_candidates",
        "succeeded_candidates",
        "failed_candidates",
        "canonical_run_spec_json",
        "canonical_run_spec_fingerprint",
        "input_data_fingerprint",
    ] {
        if original.get(field) != recomputed.get(field) {
            return Err(format!("多腿实验复算字段 {field} 不一致"));
        }
    }
    let before = original
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or("原多腿实验报告缺少 candidates")?;
    let after = recomputed
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or("复算多腿实验报告缺少 candidates")?;
    if before.len() != after.len() {
        return Err("多腿实验候选数不一致".into());
    }
    for (index, (old, new)) in before.iter().zip(after).enumerate() {
        compare_multi_leg_candidate(old, new, index + 1)?;
        if old.get("outcome").is_some_and(|outcome| !outcome.is_null()) {
            let old_artifact = read_multi_leg_artifact(old)?;
            let new_artifact = read_multi_leg_artifact(new)?;
            verify_multi_leg_replay(old, &old_artifact)?;
            verify_multi_leg_replay(new, &new_artifact)?;
            let mut old_facts = old_artifact;
            let mut new_facts = new_artifact;
            strip_multi_leg_artifact_locations(&mut old_facts);
            strip_multi_leg_artifact_locations(&mut new_facts);
            if old_facts != new_facts {
                let fields = old_facts
                    .as_object()
                    .into_iter()
                    .flat_map(|values| values.keys().cloned())
                    .chain(
                        new_facts
                            .as_object()
                            .into_iter()
                            .flat_map(|values| values.keys().cloned()),
                    )
                    .collect::<std::collections::BTreeSet<_>>();
                let differences = fields
                    .iter()
                    .filter(|field| old_facts.get(*field) != new_facts.get(*field))
                    .cloned()
                    .collect::<Vec<_>>();
                return Err(format!(
                    "多腿实验候选 {} 归因产物复算不一致，差异字段: {}",
                    index + 1,
                    differences.join(", ")
                ));
            }
        }
    }
    if original.get("ranking") != recomputed.get("ranking") {
        return Err("多腿实验排名复算不一致".into());
    }
    println!(
        "[Experiment · Recompute] 多腿实验全部 {} 个候选已复算一致",
        before.len()
    );
    Ok(())
}

fn strip_multi_leg_artifact_locations(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                strip_multi_leg_artifact_locations(item);
            }
        }
        Value::Object(fields) => {
            fields.retain(|name, _| {
                name != "path"
                    && !name.ends_with("_path")
                    && !name.ends_with("_file")
                    && !name.ends_with("_dir")
            });
            for item in fields.values_mut() {
                strip_multi_leg_artifact_locations(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn compare_multi_leg_candidate(
    before: &Value,
    after: &Value,
    ordinal: usize,
) -> Result<(), String> {
    let mut left = before.clone();
    let mut right = after.clone();
    for candidate in [&mut left, &mut right] {
        if let Some(outcome) = candidate.get_mut("outcome").and_then(Value::as_object_mut) {
            outcome.remove("attribution_artifact");
        }
    }
    if left != right {
        return Err(format!("多腿实验候选 {ordinal} 复算摘要不一致"));
    }
    Ok(())
}

fn read_multi_leg_artifact(candidate: &Value) -> Result<Value, String> {
    let path = candidate
        .pointer("/outcome/attribution_artifact")
        .and_then(Value::as_str)
        .ok_or("多腿成功候选缺少 attribution_artifact")?;
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取多腿候选归因产物失败 {path}: {error}"))?;
    serde_json::from_str(&payload).map_err(|error| format!("多腿候选归因产物 JSON 无效: {error}"))
}

fn verify_multi_leg_replay(candidate: &Value, artifact: &Value) -> Result<(), String> {
    if artifact
        .get("strategy_source_sha256")
        .and_then(Value::as_str)
        .is_some_and(|hash| !hash.is_empty())
    {
        return Err("多腿候选含外部策略身份，缺少第三方构建材料，拒绝 builtin 代替复算".into());
    }
    let events: Vec<qx_core::Event> = serde_json::from_value(
        artifact
            .get("shared_event_log")
            .cloned()
            .ok_or("多腿候选归因产物缺少 shared_event_log")?,
    )
    .map_err(|error| format!("多腿候选 shared_event_log 无效: {error}"))?;
    let (log, _) = qx_core::ReplayVerifier::replay_owned(events)
        .map_err(|error| format!("多腿候选事件重放失败: {error}"))?;
    log.validate()
        .map_err(|error| format!("多腿候选重放 EventLog 无效: {error}"))?;
    let hash = format!("{:016x}", log.digest());
    if artifact.get("run_id") != candidate.pointer("/outcome/run_id")
        || artifact.get("shared_result_hash").and_then(Value::as_str) != Some(hash.as_str())
        || artifact.get("primary_result_hash") != candidate.pointer("/outcome/primary_result_hash")
        || artifact.get("reference_result_hash")
            != candidate.pointer("/outcome/reference_result_hash")
    {
        return Err("多腿候选 EventLog、双腿 Outcome 哈希与归因产物不一致".into());
    }
    Ok(())
}
