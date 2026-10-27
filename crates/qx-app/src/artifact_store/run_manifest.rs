//! Read-side verification for the RunManifest reference stored in a backtest summary.

use super::reader::{
    read_text_bounded, read_text_bounded_by_id, ArtifactReader, FileArtifactReader,
};
use super::run_record::declared_run_record_artifact_locators;

const MAX_RUN_MANIFEST_BYTES: u64 = 128 * 1024 * 1024;

/// Verify that a summary's RunManifest pointer resolves and agrees with its declared hashes.
pub fn verify_declared_run_manifest(summary: &serde_json::Value) -> Result<(), String> {
    verify_declared_run_manifest_with_reader(summary, &FileArtifactReader)
}

/// Verify a RunManifest through the caller's authorized artifact reader.
pub fn verify_declared_run_manifest_with_reader(
    summary: &serde_json::Value,
    reader: &dyn ArtifactReader,
) -> Result<(), String> {
    let pointer = summary
        .get("run_manifest")
        .and_then(serde_json::Value::as_str)
        .ok_or("回测摘要没写 run_manifest 指针，产物身份没有第二份证据可对")?;
    if summary
        .get("experiment_id")
        .and_then(serde_json::Value::as_str)
        .is_some()
    {
        super::run_record::verify_declared_run_record_with_reader(summary, reader)
            .map_err(|error| format!("参数实验 RunRecord V2 核验失败: {error}"))?;
        let payload = read_text_bounded(reader, pointer, MAX_RUN_MANIFEST_BYTES, "RunManifest")
            .map_err(|error| format!("读取参数实验 RunManifest 失败 {pointer}: {error}"))?;
        let manifest = qx_core::RunManifest::from_json(&payload)
            .map_err(|error| format!("参数实验 RunManifest 无效 {pointer}: {error}"))?;
        verify_canonical_run_spec(&manifest, summary, pointer)?;
        return Ok(());
    }
    let artifact_locators = declared_run_record_artifact_locators(summary, reader)
        .map_err(|error| format!("读取 RunRecord 工件身份失败: {error}"))?;
    let payload = match artifact_locators {
        Some(locators) => read_text_bounded_by_id(
            reader,
            &locators.run_manifest.id,
            Some(&locators.run_manifest.legacy_reference),
            MAX_RUN_MANIFEST_BYTES,
            "RunManifest",
        ),
        None => read_text_bounded(reader, pointer, MAX_RUN_MANIFEST_BYTES, "RunManifest"),
    }
    .map_err(|error| format!("读取回测 RunManifest 失败 {pointer}: {error}"))?;
    let manifest = qx_core::RunManifest::from_json(&payload)
        .map_err(|error| format!("回测 RunManifest 无效 {pointer}: {error}"))?;
    verify_canonical_run_spec(&manifest, summary, pointer)?;
    let declared = |name: &str| -> Result<String, String> {
        summary
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(|value| value.to_string())
            .ok_or_else(|| format!("回测摘要的 {name} 缺失或不是字符串，无法与 RunManifest 对账"))
    };
    for (field, manifest_value) in [
        ("result_hash", manifest.result_hash.as_str()),
        ("input_data_hash", manifest.input_event_hash.as_str()),
    ] {
        let summary_value = declared(field)?;
        if summary_value != manifest_value {
            return Err(format!(
                "回测摘要与自己的 RunManifest 不一致: {field} 摘要={summary_value} 清单={manifest_value}（{pointer}）"
            ));
        }
    }
    Ok(())
}

/// Verify the CanonicalRunSpec attached to a RunManifest when the caller has already resolved and
/// authorized that manifest (for example, a multi-market artifact uses `run_manifest_path`).
pub fn verify_canonical_run_spec_for_manifest(
    manifest: &qx_core::RunManifest,
    summary: &serde_json::Value,
) -> Result<(), String> {
    verify_canonical_run_spec(manifest, summary, "已解析的 RunManifest")
}

/// Canonical identity is additive: old manifests remain readable, while migrated run types must
/// prove that the stored identity agrees with the report and the legacy manifest fields.
fn verify_canonical_run_spec(
    manifest: &qx_core::RunManifest,
    summary: &serde_json::Value,
    pointer: &str,
) -> Result<(), String> {
    let (payload, declared_fingerprint) = match (
        manifest.canonical_run_spec_json.as_deref(),
        manifest.canonical_run_spec_fingerprint.as_deref(),
    ) {
        (None, None) => return Ok(()),
        (Some(payload), Some(fingerprint)) => (payload, fingerprint),
        _ => {
            return Err(format!(
                "RunManifest CanonicalRunSpec JSON 与 fingerprint 必须成对出现 {pointer}"
            ));
        }
    };
    let spec = qx_spec::CanonicalRunSpec::from_json(payload)
        .map_err(|error| format!("RunManifest CanonicalRunSpec 无效 {pointer}: {error}"))?;
    let canonical_json = spec
        .canonical_json()
        .map_err(|error| format!("RunManifest CanonicalRunSpec 规范化失败 {pointer}: {error}"))?;
    if canonical_json != payload {
        return Err(format!(
            "RunManifest CanonicalRunSpec 不是规范 JSON {pointer}"
        ));
    }
    let actual_fingerprint = spec
        .fingerprint()
        .map_err(|error| format!("RunManifest CanonicalRunSpec 指纹失败 {pointer}: {error}"))?;
    if actual_fingerprint != declared_fingerprint {
        return Err(format!(
            "RunManifest CanonicalRunSpec 指纹不匹配 {pointer}: 声明={declared_fingerprint} 实际={actual_fingerprint}"
        ));
    }
    if spec.run_type == "experiment" {
        return verify_experiment_canonical_run_spec(manifest, summary, &spec, pointer);
    }
    if spec.run_type == "depth_experiment" {
        return verify_depth_experiment_canonical_run_spec(manifest, summary, &spec, pointer);
    }
    if spec.run_type == "multi_market_depth_experiment" {
        return verify_multi_market_depth_experiment_canonical_run_spec(
            manifest, summary, &spec, pointer,
        );
    }
    if spec.run_type == "multi_market_experiment" {
        return verify_multi_market_experiment_canonical_run_spec(
            manifest, summary, &spec, pointer,
        );
    }
    if spec.run_type == "multi_leg_experiment" {
        return verify_multi_leg_experiment_canonical_run_spec(manifest, summary, &spec, pointer);
    }
    if spec.run_type == "multi_leg_backtest" {
        if spec.code_revision != manifest.code_commit {
            return Err(format!(
                "CanonicalRunSpec 代码版本与 RunManifest 不一致 {pointer}"
            ));
        }
        return verify_multi_leg_canonical_run_spec(manifest, summary, &spec, pointer);
    }
    let dataset_role = match spec.run_type.as_str() {
        "bar_backtest" => "bars",
        "depth_backtest" => "depth_snapshots",
        "multi_market_backtest" => "bars",
        "multi_market_depth_backtest" => "depth_snapshots",
        "multi_account_joint_backtest" => "bars:",
        other => {
            return Err(format!(
                "当前回测产物读取器不支持 CanonicalRunSpec run_type={other} {pointer}"
            ));
        }
    };
    let multi_market = matches!(
        spec.run_type.as_str(),
        "multi_market_backtest" | "multi_market_depth_backtest"
    );
    let multi_account_joint = spec.run_type == "multi_account_joint_backtest";
    if spec.datasets.is_empty()
        || spec.datasets.iter().any(|dataset| {
            if multi_account_joint {
                !dataset.role.starts_with(dataset_role)
            } else {
                dataset.role != dataset_role
            }
        })
        || (!multi_market && !multi_account_joint && spec.datasets.len() != 1)
        || (multi_market && spec.datasets.len() > 32)
        || (multi_account_joint && spec.datasets.len() > 256)
    {
        let run_type = match spec.run_type.as_str() {
            "bar_backtest" => "Bar",
            "depth_backtest" => "Depth",
            "multi_market_backtest" => "多市场 Bar",
            _ => "多市场 Depth",
        };
        return Err(format!(
            "{run_type} CanonicalRunSpec 的 {dataset_role} 数据身份数量无效 {pointer}"
        ));
    }
    if spec.code_revision != manifest.code_commit {
        return Err(format!(
            "CanonicalRunSpec 代码版本与 RunManifest 不一致 {pointer}"
        ));
    }
    if spec.run_type == "multi_market_backtest" {
        return verify_multi_market_canonical_run_spec(manifest, summary, &spec, pointer);
    }
    if spec.run_type == "multi_market_depth_backtest" {
        return verify_multi_market_depth_canonical_run_spec(manifest, summary, &spec, pointer);
    }
    if multi_account_joint {
        return verify_multi_account_joint_canonical_run_spec(manifest, summary, &spec, pointer);
    }
    let dataset = &spec.datasets[0];
    if spec.run_type == "bar_backtest" && dataset.content_fingerprint != manifest.data_fingerprint {
        return Err(format!(
            "CanonicalRunSpec 数据指纹与 RunManifest 不一致 {pointer}"
        ));
    }
    if spec.run_type == "bar_backtest" {
        verify_provider_lineage(&spec, summary, dataset, pointer)?;
    }
    if spec.clock.start_ts_ms != manifest.clock_start
        || spec.clock.end_ts_ms != manifest.clock_end
        || spec.random_seed != manifest.global_seed.to_string()
    {
        return Err(format!(
            "CanonicalRunSpec 时钟或随机种子与 RunManifest 不一致 {pointer}"
        ));
    }
    if spec.market_rules.get(&dataset.instrument) != Some(&manifest.instrument_spec_version) {
        return Err(format!(
            "CanonicalRunSpec 市场规则与 RunManifest 不一致 {pointer}"
        ));
    }
    let behavior = spec.behavior.as_object().expect("validated as object");
    if spec.run_type == "bar_backtest"
        && (behavior
            .get("instrument")
            .and_then(serde_json::Value::as_str)
            != Some(dataset.instrument.as_str())
            || behavior.get("seed").and_then(serde_json::Value::as_str)
                != Some(spec.random_seed.as_str()))
    {
        return Err(format!(
            "Bar CanonicalRunSpec behavior 标的或种子与身份字段不一致 {pointer}"
        ));
    }
    let primary_strategy = behavior
        .get("strategy")
        .ok_or_else(|| format!("CanonicalRunSpec behavior 缺少主策略 {pointer}"))?;
    let mut behavior_strategies = vec![primary_strategy];
    if let Some(additional) = behavior
        .get("portfolio")
        .and_then(serde_json::Value::as_object)
        .and_then(|portfolio| portfolio.get("additional_strategies"))
        .and_then(serde_json::Value::as_array)
    {
        for member in additional {
            let strategy = member
                .get("strategy")
                .ok_or_else(|| format!("CanonicalRunSpec 组合成员缺少策略 {pointer}"))?;
            behavior_strategies.push(strategy);
        }
    }
    if behavior_strategies.len() != spec.strategies.len()
        || behavior_strategies
            .iter()
            .zip(&spec.strategies)
            .any(|(behavior_strategy, identity)| {
                behavior_strategy
                    .get("strategy_id")
                    .and_then(serde_json::Value::as_str)
                    != Some(identity.strategy_id.as_str())
                    || *behavior_strategy != &identity.parameters
            })
    {
        return Err(format!(
            "CanonicalRunSpec 策略身份与 behavior 配置不一致 {pointer}"
        ));
    }
    if behavior.get("portfolio").is_none()
        && spec
            .strategies
            .first()
            .is_some_and(|strategy| strategy.strategy_version != manifest.strategy_version)
    {
        return Err(format!(
            "CanonicalRunSpec 策略版本与 RunManifest 不一致 {pointer}"
        ));
    }
    let execution_model = spec
        .execution_model
        .as_object()
        .expect("validated as object");
    if execution_model
        .get("runtime_version")
        .and_then(serde_json::Value::as_str)
        != Some(manifest.runtime_version.as_str())
    {
        return Err(format!(
            "CanonicalRunSpec 执行器版本与 RunManifest 不一致 {pointer}"
        ));
    }
    let execution_account = execution_model
        .get("account_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("CanonicalRunSpec 执行模型缺少 account_id {pointer}"))?;
    if spec.run_type == "bar_backtest" {
        let summary_account = summary
            .get("account_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("Bar 回测摘要缺少 account_id {pointer}"))?;
        if execution_account != summary_account {
            return Err(format!("CanonicalRunSpec 账户与回测摘要不一致 {pointer}"));
        }
    } else {
        let expected_account = behavior
            .get("account_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("app");
        if execution_account != expected_account
            || summary
                .get("account_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|summary_account| summary_account != execution_account)
        {
            return Err(format!(
                "Depth CanonicalRunSpec 账户与回测摘要不一致 {pointer}"
            ));
        }
    }
    let summary_instrument = summary
        .get("instrument")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("回测摘要缺少 instrument {pointer}"))?;
    if summary_instrument != dataset.instrument {
        return Err(format!("CanonicalRunSpec 标的与回测摘要不一致 {pointer}"));
    }
    if spec.run_type == "bar_backtest" {
        let summary_seed = summary
            .get("seed")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| format!("Bar 回测摘要缺少合法 seed {pointer}"))?;
        if summary_seed.to_string() != spec.random_seed {
            return Err(format!(
                "CanonicalRunSpec 随机种子与回测摘要不一致 {pointer}"
            ));
        }
    } else if let Some(summary_seed) = summary.get("seed") {
        let summary_seed = summary_seed
            .as_u64()
            .map(|seed| seed.to_string())
            .or_else(|| summary_seed.as_str().map(str::to_string))
            .ok_or_else(|| format!("Depth 回测摘要 seed 格式无效 {pointer}"))?;
        if summary_seed != spec.random_seed {
            return Err(format!(
                "Depth CanonicalRunSpec 随机种子与回测摘要不一致 {pointer}"
            ));
        }
    }
    let summary_fingerprint = summary
        .get("data_fingerprint")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("回测摘要缺少 data_fingerprint {pointer}"))?;
    if summary_fingerprint != manifest.data_fingerprint {
        return Err(format!(
            "CanonicalRunSpec 数据指纹与回测摘要不一致 {pointer}"
        ));
    }
    if spec.run_type == "depth_backtest" {
        let frame_hash = summary
            .get("input")
            .and_then(|input| input.get("fingerprint"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("Depth 回测摘要缺少输入帧 fingerprint {pointer}"))?;
        if dataset.content_fingerprint != format!("depth-frame-fnv1a64:{frame_hash}") {
            return Err(format!(
                "Depth CanonicalRunSpec 帧内容指纹与回测摘要输入不一致 {pointer}"
            ));
        }
    } else if summary_fingerprint != dataset.content_fingerprint {
        return Err(format!(
            "CanonicalRunSpec 数据指纹与回测摘要不一致 {pointer}"
        ));
    }
    for (field, expected) in [
        ("clock_start", spec.clock.start_ts_ms),
        ("clock_end", spec.clock.end_ts_ms),
    ] {
        let actual = summary
            .get(field)
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| format!("回测摘要缺少合法 {field} {pointer}"))?;
        if actual != expected {
            return Err(format!(
                "CanonicalRunSpec 时钟与回测摘要的 {field} 不一致 {pointer}"
            ));
        }
    }
    if spec.run_type == "depth_backtest" {
        if behavior.get("tier").and_then(serde_json::Value::as_str)
            != summary.get("tier").and_then(serde_json::Value::as_str)
        {
            return Err(format!(
                "Depth CanonicalRunSpec 档位与回测摘要不一致 {pointer}"
            ));
        }
        if spec.execution_model.get("execution_model_descriptor") != summary.get("execution_model")
        {
            return Err(format!(
                "Depth CanonicalRunSpec 撮合描述与回测摘要不一致 {pointer}"
            ));
        }
        if let Some(quality_fingerprint) = spec
            .extensions
            .get("dataset_quality_fingerprint")
            .and_then(serde_json::Value::as_str)
        {
            if summary
                .get("dataset_quality")
                .and_then(|quality| quality.get("manifest_fingerprint"))
                .and_then(serde_json::Value::as_str)
                != Some(quality_fingerprint)
            {
                return Err(format!(
                    "Depth CanonicalRunSpec 数据质量指纹与回测摘要不一致 {pointer}"
                ));
            }
        }
    }
    Ok(())
}

fn verify_experiment_canonical_run_spec(
    manifest: &qx_core::RunManifest,
    summary: &serde_json::Value,
    spec: &qx_spec::CanonicalRunSpec,
    pointer: &str,
) -> Result<(), String> {
    let dataset = spec
        .datasets
        .first()
        .ok_or_else(|| format!("实验 CanonicalRunSpec 缺少 Bar 数据身份 {pointer}"))?;
    let summary_spec = summary
        .get("canonical_run_spec_json")
        .and_then(serde_json::Value::as_str);
    let summary_fingerprint = summary
        .get("canonical_run_spec_fingerprint")
        .and_then(serde_json::Value::as_str);
    let result_hash = summary
        .get("result_hash")
        .and_then(serde_json::Value::as_str);
    let input_fingerprint = summary
        .get("input_data_fingerprint")
        .and_then(serde_json::Value::as_str);
    let result_fingerprint = if summary.get("candidates").is_some() {
        Some(crate::cases::experiment_result_hash_from_value(summary)?)
    } else {
        None
    };
    let runtime_version = spec
        .execution_model
        .get("runtime_version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("实验 CanonicalRunSpec 缺少 runtime_version {pointer}"))?;
    let model_json = serde_json::to_vec(&spec.execution_model)
        .map_err(|error| format!("实验执行模型编码失败 {pointer}: {error}"))?;
    let expected_strategy_version = if spec.strategies.len() == 1 {
        spec.strategies[0].strategy_version.as_str()
    } else {
        "experiment-strategy-set-v1"
    };
    let expected_market_version = spec
        .market_rules
        .get(&dataset.instrument)
        .map(String::as_str)
        .unwrap_or("legacy-default-v1");
    let expected_components = std::collections::BTreeMap::from([(
        "bars".to_string(),
        dataset.content_fingerprint.clone(),
    )]);
    let canonical_json = spec
        .canonical_json()
        .map_err(|error| format!("实验 CanonicalRunSpec 规范化失败 {pointer}: {error}"))?;
    if dataset.role != "bars"
        || spec.datasets.len() != 1
        || spec.code_revision != manifest.code_commit
        || spec.clock.start_ts_ms != manifest.clock_start
        || spec.clock.end_ts_ms != manifest.clock_end
        || spec.random_seed != manifest.global_seed.to_string()
        || manifest.run_id
            != summary
                .get("experiment_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        || manifest.config_hash != spec.fingerprint().map_err(|error| error.to_string())?
        || manifest.data_fingerprint != dataset.content_fingerprint
        || input_fingerprint != Some(dataset.content_fingerprint.as_str())
        || manifest.input_event_hash != dataset.content_fingerprint
        || manifest.output_event_hash != manifest.result_hash
        || result_hash != Some(manifest.result_hash.as_str())
        || result_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| fingerprint != manifest.result_hash)
        || summary_spec != Some(canonical_json.as_str())
        || summary_fingerprint != Some(manifest.config_hash.as_str())
        || manifest.determinism_mode != true
        || manifest.strategy_version != expected_strategy_version
        || manifest.instrument_spec_version != expected_market_version
        || manifest.model_fingerprint != qx_strategy::sha256_hex(&model_json)
        || manifest.input_components != expected_components
        || runtime_version != manifest.runtime_version
    {
        return Err(format!(
            "实验 CanonicalRunSpec 与报告、RunManifest 或结果指纹不一致 {pointer}"
        ));
    }
    Ok(())
}

fn verify_depth_experiment_canonical_run_spec(
    manifest: &qx_core::RunManifest,
    summary: &serde_json::Value,
    spec: &qx_spec::CanonicalRunSpec,
    pointer: &str,
) -> Result<(), String> {
    let dataset = spec
        .datasets
        .first()
        .ok_or_else(|| format!("深度实验 CanonicalRunSpec 缺少深度数据身份 {pointer}"))?;
    let canonical_json = spec
        .canonical_json()
        .map_err(|error| format!("深度实验 CanonicalRunSpec 规范化失败 {pointer}: {error}"))?;
    let canonical_fingerprint = spec
        .fingerprint()
        .map_err(|error| format!("深度实验 CanonicalRunSpec 指纹失败 {pointer}: {error}"))?;
    let runtime_version = spec
        .execution_model
        .get("runtime_version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("深度实验 CanonicalRunSpec 缺少 runtime_version {pointer}"))?;
    let model_json = serde_json::to_vec(&spec.execution_model)
        .map_err(|error| format!("深度实验执行模型编码失败 {pointer}: {error}"))?;
    let expected_market_version = spec
        .market_rules
        .get(&dataset.instrument)
        .map(String::as_str)
        .unwrap_or("instrument-spec:none");
    let expected_components = spec
        .datasets
        .iter()
        .map(|dataset| (dataset.role.clone(), dataset.content_fingerprint.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let expected_seed = spec
        .random_seed
        .parse::<u64>()
        .map_err(|error| format!("深度实验随机种子无效 {pointer}: {error}"))?;
    let summary_spec = summary
        .get("canonical_run_spec_json")
        .and_then(serde_json::Value::as_str);
    let summary_fingerprint = summary
        .get("canonical_run_spec_fingerprint")
        .and_then(serde_json::Value::as_str);
    let result_hash = summary
        .get("result_hash")
        .and_then(serde_json::Value::as_str);
    let input_fingerprint = summary
        .get("input_data_fingerprint")
        .and_then(serde_json::Value::as_str);
    let result_fingerprint = if summary.get("candidates").is_some() {
        Some(crate::cases::experiment_result_hash_from_value(summary)?)
    } else {
        None
    };
    let has_foreign_instrument = summary
        .get("candidates")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|candidates| {
            candidates.iter().any(|candidate| {
                candidate
                    .get("outcome")
                    .filter(|outcome| !outcome.is_null())
                    .and_then(|outcome| outcome.get("instrument"))
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|instrument| instrument != dataset.instrument)
            })
        });
    if dataset.role != "depth_snapshots"
        || spec.datasets.len() != 1
        || spec.code_revision != manifest.code_commit
        || spec.clock.start_ts_ms != manifest.clock_start
        || spec.clock.end_ts_ms != manifest.clock_end
        || spec.random_seed != manifest.global_seed.to_string()
        || expected_seed != manifest.global_seed
        || manifest.run_id
            != summary
                .get("experiment_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        || manifest.config_hash != canonical_fingerprint
        || manifest.data_fingerprint != dataset.content_fingerprint
        || input_fingerprint != Some(dataset.content_fingerprint.as_str())
        || manifest.input_event_hash != dataset.content_fingerprint
        || manifest.output_event_hash != manifest.result_hash
        || result_hash != Some(manifest.result_hash.as_str())
        || result_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| fingerprint != manifest.result_hash)
        || summary_spec != Some(canonical_json.as_str())
        || summary_fingerprint != Some(manifest.config_hash.as_str())
        || manifest.determinism_mode != true
        || manifest.strategy_version != "depth-experiment-strategy-set-v1"
        || manifest.instrument_spec_version != expected_market_version
        || manifest.model_fingerprint != qx_strategy::sha256_hex(&model_json)
        || manifest.input_components != expected_components
        || runtime_version != manifest.runtime_version
        || has_foreign_instrument
    {
        return Err(format!(
            "深度实验 CanonicalRunSpec 与报告、RunManifest 或结果指纹不一致 {pointer}"
        ));
    }
    Ok(())
}

fn verify_multi_market_depth_experiment_canonical_run_spec(
    manifest: &qx_core::RunManifest,
    summary: &serde_json::Value,
    spec: &qx_spec::CanonicalRunSpec,
    pointer: &str,
) -> Result<(), String> {
    use qx_core::Fnv1a;
    use serde_json::Value;
    use std::collections::{BTreeMap, BTreeSet};

    let behavior = spec
        .behavior
        .as_object()
        .ok_or_else(|| format!("多市场盘口实验 behavior 无效 {pointer}"))?;
    let base = behavior
        .get("base")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("多市场盘口实验 CanonicalRunSpec 缺少 base {pointer}"))?;
    let markets = base
        .get("markets")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("多市场盘口实验 CanonicalRunSpec 缺少 markets {pointer}"))?;
    if markets.is_empty()
        || markets.len() != spec.datasets.len()
        || spec.run_type != "multi_market_depth_experiment"
    {
        return Err(format!(
            "多市场盘口实验 CanonicalRunSpec 市场/数据集数量无效 {pointer}"
        ));
    }
    let runtime_version = spec
        .execution_model
        .get("runtime_version")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("多市场盘口实验执行模型缺少 runtime_version {pointer}"))?;
    let target_strategy_id = behavior
        .get("target_strategy_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!("多市场盘口实验 CanonicalRunSpec 缺少 target_strategy_id {pointer}")
        })?;
    let report_target_strategy_id = summary
        .get("target_strategy_id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("多市场盘口实验报告缺少 target_strategy_id {pointer}"))?;
    let experiment_id = summary
        .get("experiment_id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("多市场盘口实验报告缺少 experiment_id {pointer}"))?;
    let summary_json = spec.canonical_json().map_err(|error| {
        format!("多市场盘口实验 CanonicalRunSpec 规范化失败 {pointer}: {error}")
    })?;
    let canonical_fingerprint = spec
        .fingerprint()
        .map_err(|error| format!("多市场盘口实验 CanonicalRunSpec 指纹失败 {pointer}: {error}"))?;
    let summary_fingerprint = summary
        .get("canonical_run_spec_fingerprint")
        .and_then(Value::as_str);
    let summary_spec = summary
        .get("canonical_run_spec_json")
        .and_then(Value::as_str);
    let input_fingerprint = summary
        .get("input_data_fingerprint")
        .and_then(Value::as_str);
    let result_hash = summary.get("result_hash").and_then(Value::as_str);
    let calculated_result_hash = if summary.get("candidates").is_some() {
        Some(crate::cases::experiment_result_hash_from_value(summary)?)
    } else {
        None
    };
    let datasets = spec
        .datasets
        .iter()
        .map(|dataset| (dataset.instrument.as_str(), dataset))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let mut hasher = Fnv1a::new();
    let mut input_components = BTreeMap::new();
    let mut market_rules = BTreeMap::new();
    for market in markets {
        let fields = market
            .as_object()
            .ok_or_else(|| format!("多市场盘口实验市场项无效 {pointer}"))?;
        let instrument = fields
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("多市场盘口实验市场缺少 instrument {pointer}"))?;
        if !seen.insert(instrument) {
            return Err(format!(
                "多市场盘口实验 CanonicalRunSpec 标的重复 {pointer}"
            ));
        }
        let dataset = datasets.get(instrument).ok_or_else(|| {
            format!("多市场盘口实验 CanonicalRunSpec 缺少 {instrument} 数据集 {pointer}")
        })?;
        let data_tier = fields
            .get("data_tier")
            .and_then(Value::as_str)
            .unwrap_or("l2");
        let source = spec
            .extensions
            .get(&format!("depth_source:{instrument}"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!("多市场盘口实验 CanonicalRunSpec 缺少 {instrument} 来源 {pointer}")
            })?;
        let prefix = "depth-frame-fnv1a64:";
        let input_hash_text = dataset
            .content_fingerprint
            .strip_prefix(prefix)
            .ok_or_else(|| format!("多市场盘口实验 {instrument} 数据指纹格式无效 {pointer}"))?;
        let input_hash = u64::from_str_radix(input_hash_text, 16).map_err(|error| {
            format!("多市场盘口实验 {instrument} 数据指纹无效 {pointer}: {error}")
        })?;
        if format!("{input_hash:016x}") != input_hash_text
            || dataset.role != "depth_snapshots"
            || dataset.start_ts_ms > dataset.end_ts_ms
        {
            return Err(format!(
                "多市场盘口实验 {instrument} 数据身份非规范 {pointer}"
            ));
        }
        let quality_fingerprint = spec
            .extensions
            .get(&format!("dataset_quality:{instrument}"))
            .and_then(Value::as_str);
        hasher.write_text(instrument);
        hasher.write_text(data_tier);
        hasher.write_text(source);
        hasher.write_u64(input_hash);
        input_components.insert(
            format!("depthframe:{instrument}"),
            format!("depth:{data_tier}:{source}:{input_hash:016x}"),
        );
        if let Some(quality_fingerprint) = quality_fingerprint {
            hasher.write_text(quality_fingerprint);
            input_components.insert(
                format!("dataset-quality:{instrument}"),
                quality_fingerprint.to_owned(),
            );
        }
        let mut market_spec = fields
            .get("market_spec")
            .cloned()
            .ok_or_else(|| format!("多市场盘口实验 {instrument} 缺少 market_spec {pointer}"))?;
        normalize_multi_market_depth_market_rule(&mut market_spec, "market_spec", pointer)?;
        let market_spec_bytes = serde_json::to_vec(&market_spec).map_err(|error| {
            format!("多市场盘口实验 {instrument} market_spec 编码失败: {error}")
        })?;
        market_rules.insert(
            instrument.to_owned(),
            qx_strategy::sha256_hex(&market_spec_bytes),
        );
        if !spec.strategies.iter().any(|strategy| {
            strategy.strategy_id
                == fields
                    .get("strategy")
                    .and_then(Value::as_object)
                    .and_then(|strategy| strategy.get("strategy_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
        }) {
            return Err(format!(
                "多市场盘口实验 {instrument} 策略身份缺失 {pointer}"
            ));
        }
    }
    let data_fingerprint = format!("{:016x}", hasher.finish());
    let declared_components = spec
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
    let market_rule_count_matches = market_rules == spec.market_rules;
    let candidate_outcomes = summary
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("多市场盘口实验候选列表无效 {pointer}"))?;
    let total_candidates = summary
        .get("total_candidates")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("多市场盘口实验缺少 total_candidates {pointer}"))?;
    let completed_candidates = summary
        .get("completed_candidates")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("多市场盘口实验缺少 completed_candidates {pointer}"))?;
    let mut candidate_run_ids = BTreeSet::new();
    let candidate_inputs_match = candidate_outcomes.iter().all(|candidate| {
        let Some(outcome) = candidate.get("outcome").filter(|value| !value.is_null()) else {
            return true;
        };
        let run_id = outcome
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        candidate_run_ids.insert(run_id.to_owned())
            && outcome.get("data_fingerprint").and_then(Value::as_str)
                == Some(data_fingerprint.as_str())
            && outcome.get("markets_total").and_then(Value::as_u64) == Some(markets.len() as u64)
    });
    let mut mismatches = Vec::new();
    if spec.code_revision != manifest.code_commit {
        mismatches.push("code_revision");
    }
    if spec.clock.start_ts_ms != manifest.clock_start || spec.clock.end_ts_ms != manifest.clock_end
    {
        mismatches.push("clock");
    }
    if spec.random_seed != manifest.global_seed.to_string() {
        mismatches.push("random_seed");
    }
    if manifest.run_id != experiment_id {
        mismatches.push("run_id");
    }
    if report_target_strategy_id != target_strategy_id {
        mismatches.push("target_strategy_id");
    }
    if manifest.config_hash != canonical_fingerprint {
        mismatches.push("config_fingerprint");
    }
    if summary_spec != Some(summary_json.as_str()) {
        mismatches.push("summary_canonical_json");
    }
    if summary_fingerprint != Some(canonical_fingerprint.as_str()) {
        mismatches.push("summary_canonical_fingerprint");
    }
    if input_fingerprint != Some(data_fingerprint.as_str()) {
        mismatches.push("summary_input_fingerprint");
    }
    if manifest.data_fingerprint != data_fingerprint {
        mismatches.push("manifest_data_fingerprint");
    }
    if manifest.input_event_hash != data_fingerprint {
        mismatches.push("input_event_hash");
    }
    if manifest.input_components != input_components {
        mismatches.push("manifest_input_components");
    }
    if declared_components != input_components {
        mismatches.push("canonical_input_components");
    }
    if manifest.output_event_hash != manifest.result_hash {
        mismatches.push("output_event_hash");
    }
    if result_hash != Some(manifest.result_hash.as_str()) {
        mismatches.push("summary_result_hash");
    }
    if calculated_result_hash.as_deref() != Some(manifest.result_hash.as_str()) {
        mismatches.push("computed_result_hash");
    }
    if manifest.strategy_version != "multi-market-depth-experiment-strategy-set-v1" {
        mismatches.push("strategy_version");
    }
    if manifest.instrument_spec_version != "multi-market-depth-experiment-v1" {
        mismatches.push("instrument_spec_version");
    }
    if manifest.model_fingerprint
        != qx_strategy::sha256_hex(
            &serde_json::to_vec(&spec.execution_model).map_err(|error| error.to_string())?,
        )
    {
        mismatches.push("model_fingerprint");
    }
    if manifest.runtime_version != runtime_version {
        mismatches.push("runtime_version");
    }
    if !manifest.determinism_mode {
        mismatches.push("determinism_mode");
    }
    if !market_rule_count_matches {
        mismatches.push("market_rules");
    }
    if completed_candidates != total_candidates {
        mismatches.push("completed_candidates");
    }
    if candidate_outcomes.len() as u64 != total_candidates {
        mismatches.push("candidate_count");
    }
    if !candidate_inputs_match {
        mismatches.push("candidate_inputs");
    }
    if !mismatches.is_empty() {
        return Err(format!(
            "多市场盘口实验 CanonicalRunSpec 与报告、RunManifest 或候选身份不一致（{}）{pointer}",
            mismatches.join(", ")
        ));
    }
    Ok(())
}

fn verify_multi_market_experiment_canonical_run_spec(
    manifest: &qx_core::RunManifest,
    summary: &serde_json::Value,
    spec: &qx_spec::CanonicalRunSpec,
    pointer: &str,
) -> Result<(), String> {
    use qx_core::Fnv1a;
    use serde_json::Value;
    use std::collections::{BTreeMap, BTreeSet};

    let behavior = spec
        .behavior
        .as_object()
        .ok_or_else(|| format!("多市场实验 behavior 无效 {pointer}"))?;
    let target_strategy_id = behavior
        .get("target_strategy_id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("多市场实验 CanonicalRunSpec 缺少目标策略 {pointer}"))?;
    let experiment_id = summary
        .get("experiment_id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("多市场实验报告缺少 experiment_id {pointer}"))?;
    let report_target_strategy_id = summary
        .get("target_strategy_id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("多市场实验报告缺少 target_strategy_id {pointer}"))?;
    let markets = behavior
        .get("base")
        .and_then(Value::as_object)
        .and_then(|base| base.get("markets"))
        .and_then(Value::as_array)
        .ok_or_else(|| format!("多市场实验 CanonicalRunSpec 缺少 markets {pointer}"))?;
    if spec.run_type != "multi_market_experiment"
        || markets.is_empty()
        || markets.len() != spec.datasets.len()
        || target_strategy_id != report_target_strategy_id
    {
        return Err(format!(
            "多市场实验 CanonicalRunSpec 市场或目标无效 {pointer}"
        ));
    }
    let canonical_json = spec
        .canonical_json()
        .map_err(|error| format!("多市场实验 CanonicalRunSpec 规范化失败 {pointer}: {error}"))?;
    let canonical_fingerprint = spec
        .fingerprint()
        .map_err(|error| format!("多市场实验 CanonicalRunSpec 指纹失败 {pointer}: {error}"))?;
    if summary
        .get("canonical_run_spec_json")
        .and_then(Value::as_str)
        != Some(canonical_json.as_str())
        || summary
            .get("canonical_run_spec_fingerprint")
            .and_then(Value::as_str)
            != Some(canonical_fingerprint.as_str())
    {
        return Err(format!(
            "多市场实验报告 CanonicalRunSpec 身份不一致 {pointer}"
        ));
    }
    let data_fingerprint = summary
        .get("input_data_fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("多市场实验报告缺少输入数据指纹 {pointer}"))?;
    let result_hash = summary
        .get("result_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("多市场实验报告缺少 result_hash {pointer}"))?;
    let calculated_result_hash = crate::cases::experiment_result_hash_from_value(summary)?;
    let runtime_version = spec
        .execution_model
        .get("runtime_version")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("多市场实验执行模型缺少 runtime_version {pointer}"))?;
    let global_seed = spec
        .random_seed
        .parse::<u64>()
        .map_err(|error| format!("多市场实验随机种子无效 {pointer}: {error}"))?;

    let datasets = spec
        .datasets
        .iter()
        .map(|dataset| (dataset.instrument.as_str(), dataset))
        .collect::<BTreeMap<_, _>>();
    if datasets.len() != spec.datasets.len() {
        return Err(format!(
            "多市场实验 CanonicalRunSpec 含重复数据集 {pointer}"
        ));
    }
    let mut seen = BTreeSet::new();
    let mut hasher = Fnv1a::new();
    let mut expected_components = BTreeMap::new();
    for market in markets {
        let fields = market
            .as_object()
            .ok_or_else(|| format!("多市场实验市场项无效 {pointer}"))?;
        let instrument = fields
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("多市场实验市场项缺少 instrument {pointer}"))?;
        if !seen.insert(instrument) {
            return Err(format!("多市场实验 CanonicalRunSpec 标的重复 {pointer}"));
        }
        let dataset = datasets.get(instrument).ok_or_else(|| {
            format!("多市场实验 CanonicalRunSpec 缺少 {instrument} 数据集 {pointer}")
        })?;
        if dataset.role != "bars"
            || dataset.content_fingerprint.trim().is_empty()
            || dataset.start_ts_ms > dataset.end_ts_ms
        {
            return Err(format!(
                "多市场实验 {instrument} Bar 数据身份无效 {pointer}"
            ));
        }
        let input_component_key = format!("input_component:barframe:{instrument}");
        if spec
            .extensions
            .get(&input_component_key)
            .and_then(Value::as_str)
            != Some(dataset.content_fingerprint.as_str())
        {
            return Err(format!(
                "多市场实验 {instrument} Bar 输入组件不一致 {pointer}"
            ));
        }
        expected_components.insert(
            format!("barframe:{instrument}"),
            dataset.content_fingerprint.clone(),
        );
        hasher.write_text(instrument);
        hasher.write_text(&dataset.content_fingerprint);

        if let Some(quality) = spec
            .extensions
            .get(&format!("dataset_quality:{instrument}"))
            .and_then(Value::as_str)
        {
            if quality.trim().is_empty()
                || spec
                    .extensions
                    .get(&format!("input_component:dataset-quality:{instrument}"))
                    .and_then(Value::as_str)
                    != Some(quality)
            {
                return Err(format!(
                    "多市场实验 {instrument} 质量输入组件无效 {pointer}"
                ));
            }
            hasher.write_text(quality);
            expected_components.insert(format!("dataset-quality:{instrument}"), quality.into());
        } else if spec
            .extensions
            .contains_key(&format!("input_component:dataset-quality:{instrument}"))
        {
            return Err(format!(
                "多市场实验 {instrument} 质量输入组件没有质量指纹 {pointer}"
            ));
        }
    }
    if seen.len() != datasets.len() {
        return Err(format!(
            "多市场实验 CanonicalRunSpec 数据集未全部绑定 {pointer}"
        ));
    }
    let mut declared_components = BTreeMap::new();
    for (name, value) in &spec.extensions {
        if let Some(key) = name.strip_prefix("input_component:") {
            let fingerprint = value
                .as_str()
                .ok_or_else(|| format!("多市场实验输入组件不是字符串 {pointer}"))?;
            if declared_components
                .insert(key.to_owned(), fingerprint.to_owned())
                .is_some()
            {
                return Err(format!("多市场实验输入组件重复 {pointer}"));
            }
        }
    }
    let calculated_data_fingerprint = format!("{:016x}", hasher.finish());
    let mut mismatches = Vec::new();
    if declared_components != expected_components {
        mismatches.push("canonical input components");
    }
    if calculated_data_fingerprint != data_fingerprint
        || spec
            .extensions
            .get("input_data_fingerprint")
            .and_then(Value::as_str)
            != Some(data_fingerprint)
    {
        mismatches.push("input data fingerprint");
    }
    if result_hash != calculated_result_hash {
        mismatches.push("result hash");
    }
    if manifest.run_id != experiment_id {
        mismatches.push("run_id");
    }
    if manifest.code_commit != spec.code_revision {
        mismatches.push("code revision");
    }
    if manifest.config_hash != canonical_fingerprint {
        mismatches.push("config fingerprint");
    }
    if manifest.data_fingerprint != data_fingerprint {
        mismatches.push("manifest data fingerprint");
    }
    if manifest.input_components != expected_components {
        mismatches.push("manifest input components");
    }
    if manifest.clock_start != spec.clock.start_ts_ms || manifest.clock_end != spec.clock.end_ts_ms
    {
        mismatches.push("clock range");
    }
    if manifest.global_seed != global_seed {
        mismatches.push("random seed");
    }
    if !manifest.determinism_mode {
        mismatches.push("determinism mode");
    }
    if manifest.result_hash != result_hash {
        mismatches.push("manifest result hash");
    }
    if manifest.input_event_hash != data_fingerprint {
        mismatches.push("manifest input event hash");
    }
    if manifest.output_event_hash != result_hash {
        mismatches.push("manifest output event hash");
    }
    if manifest.runtime_version != runtime_version {
        mismatches.push("runtime version");
    }
    if !mismatches.is_empty() {
        return Err(format!(
            "多市场实验 CanonicalRunSpec 与报告、RunManifest 或结果指纹不一致（{}）{pointer}",
            mismatches.join(", ")
        ));
    }
    Ok(())
}

fn verify_multi_leg_experiment_canonical_run_spec(
    manifest: &qx_core::RunManifest,
    summary: &serde_json::Value,
    spec: &qx_spec::CanonicalRunSpec,
    pointer: &str,
) -> Result<(), String> {
    use serde_json::{json, Value};
    use std::collections::{BTreeMap, BTreeSet};

    let primary = spec
        .datasets
        .iter()
        .find(|dataset| dataset.role == "bars:primary")
        .ok_or_else(|| format!("多腿实验 CanonicalRunSpec 缺少主腿数据 {pointer}"))?;
    let reference = spec
        .datasets
        .iter()
        .find(|dataset| dataset.role == "bars:reference")
        .ok_or_else(|| format!("多腿实验 CanonicalRunSpec 缺少对冲腿数据 {pointer}"))?;
    if spec.run_type != "multi_leg_experiment"
        || spec.datasets.len() != 2
        || primary.instrument == reference.instrument
    {
        return Err(format!(
            "多腿实验 CanonicalRunSpec 数据集结构无效 {pointer}"
        ));
    }
    let canonical_json = spec
        .canonical_json()
        .map_err(|error| format!("多腿实验 CanonicalRunSpec 规范化失败 {pointer}: {error}"))?;
    let canonical_fingerprint = spec
        .fingerprint()
        .map_err(|error| format!("多腿实验 CanonicalRunSpec 指纹失败 {pointer}: {error}"))?;
    let runtime_version = spec
        .execution_model
        .get("runtime_version")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("多腿实验执行模型缺少 runtime_version {pointer}"))?;
    let seed = spec
        .random_seed
        .parse::<u64>()
        .map_err(|error| format!("多腿实验随机种子无效 {pointer}: {error}"))?;
    let primary_quality = spec
        .extensions
        .get("dataset_quality:primary")
        .and_then(Value::as_str);
    let reference_quality = spec
        .extensions
        .get("dataset_quality:reference")
        .and_then(Value::as_str);
    if primary_quality.is_some() != reference_quality.is_some() {
        return Err(format!("多腿实验质量清单身份必须成对出现 {pointer}"));
    }
    let expected_data_fingerprint = match (primary_quality, reference_quality) {
        (Some(primary_quality), Some(reference_quality)) => format!(
            "multi-bar:{}:{}:quality:{}:{}",
            primary.content_fingerprint,
            reference.content_fingerprint,
            primary_quality,
            reference_quality,
        ),
        (None, None) => format!(
            "multi-bar:{}:{}",
            primary.content_fingerprint, reference.content_fingerprint
        ),
        _ => return Err(format!("多腿实验质量清单身份必须成对出现 {pointer}")),
    };
    let mut expected_components = BTreeMap::from([
        (
            "primary_bars".to_string(),
            primary.content_fingerprint.clone(),
        ),
        (
            "reference_bars".to_string(),
            reference.content_fingerprint.clone(),
        ),
    ]);
    if let (Some(primary_quality), Some(reference_quality)) = (primary_quality, reference_quality) {
        expected_components.insert("primary_quality".into(), primary_quality.to_owned());
        expected_components.insert("reference_quality".into(), reference_quality.to_owned());
    }
    let model_json = serde_json::to_vec(&spec.execution_model)
        .map_err(|error| format!("多腿实验执行模型编码失败 {pointer}: {error}"))?;
    let experiment_id = summary
        .get("experiment_id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("多腿实验报告缺少 experiment_id {pointer}"))?;
    let input_fingerprint = summary
        .get("input_data_fingerprint")
        .and_then(Value::as_str);
    let summary_spec = summary
        .get("canonical_run_spec_json")
        .and_then(Value::as_str);
    let summary_fingerprint = summary
        .get("canonical_run_spec_fingerprint")
        .and_then(Value::as_str);
    let result_hash = summary.get("result_hash").and_then(Value::as_str);
    let result_hash_is_valid = result_hash.is_some_and(|value| {
        value.len() == 16
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    let candidates = summary
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("多腿实验候选列表无效 {pointer}"))?;
    let total_candidates = summary
        .get("total_candidates")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("多腿实验缺少 total_candidates {pointer}"))?;
    let completed_candidates = summary
        .get("completed_candidates")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("多腿实验缺少 completed_candidates {pointer}"))?;
    let succeeded_candidates = summary
        .get("succeeded_candidates")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("多腿实验缺少 succeeded_candidates {pointer}"))?;
    let failed_candidates = summary
        .get("failed_candidates")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("多腿实验缺少 failed_candidates {pointer}"))?;
    let mut seen_ordinals = BTreeSet::new();
    let mut seen_runs = BTreeSet::new();
    let mut ranked = Vec::new();
    for candidate in candidates {
        let ordinal = candidate
            .get("ordinal")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("多腿实验候选缺少 ordinal {pointer}"))?;
        if ordinal == 0 || ordinal > total_candidates || !seen_ordinals.insert(ordinal) {
            return Err(format!("多腿实验候选 ordinal 重复或越界 {pointer}"));
        }
        match candidate
            .get("outcome")
            .filter(|outcome| !outcome.is_null())
        {
            Some(outcome) => {
                let run_id = outcome
                    .get("run_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("多腿实验候选结果缺少 run_id {pointer}"))?;
                if !seen_runs.insert(run_id.to_owned()) {
                    return Err(format!("多腿实验候选运行身份重复 {pointer}"));
                }
                let combined_return_bps = outcome
                    .get("combined_return_bps")
                    .and_then(Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok())
                    .ok_or_else(|| format!("多腿实验候选收益字段无效 {pointer}"))?;
                let primary_result_hash = outcome
                    .get("primary_result_hash")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("多腿实验候选主腿结果摘要缺失 {pointer}"))?;
                let reference_result_hash = outcome
                    .get("reference_result_hash")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("多腿实验候选对冲腿结果摘要缺失 {pointer}"))?;
                ranked.push((
                    ordinal,
                    run_id.to_owned(),
                    combined_return_bps,
                    primary_result_hash.to_owned(),
                    reference_result_hash.to_owned(),
                ));
                if !candidate.get("error").is_some_and(Value::is_null) {
                    return Err(format!("成功的多腿实验候选不能同时声明 error {pointer}"));
                }
            }
            None => {
                if candidate
                    .get("error")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    return Err(format!("失败的多腿实验候选缺少错误说明 {pointer}"));
                }
            }
        }
    }
    ranked.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(&right.0)));
    let expected_ranking = ranked
        .iter()
        .enumerate()
        .map(
            |(index, (ordinal, run_id, return_bps, primary_hash, reference_hash))| {
                json!({
                    "rank": index + 1,
                    "ordinal": ordinal,
                    "run_id": run_id,
                    "combined_return_bps": return_bps,
                    "primary_result_hash": primary_hash,
                    "reference_result_hash": reference_hash,
                })
            },
        )
        .collect::<Vec<_>>();
    let input_components_match = manifest.input_components == expected_components;
    if spec.code_revision != manifest.code_commit
        || manifest.run_id != experiment_id
        || manifest.config_hash != canonical_fingerprint
        || manifest.data_fingerprint != expected_data_fingerprint
        || input_fingerprint != Some(expected_data_fingerprint.as_str())
        || manifest.clock_start != spec.clock.start_ts_ms
        || manifest.clock_end != spec.clock.end_ts_ms
        || manifest.global_seed != seed
        || !manifest.determinism_mode
        || !result_hash_is_valid
        || result_hash != Some(manifest.result_hash.as_str())
        || manifest.input_event_hash != expected_data_fingerprint
        || manifest.output_event_hash != manifest.result_hash
        || manifest.strategy_version != "multi-leg-experiment-strategy-set-v1"
        || manifest.instrument_spec_version != "multi-leg-experiment-v1"
        || manifest.model_fingerprint != qx_strategy::sha256_hex(&model_json)
        || manifest.runtime_version != runtime_version
        || manifest.canonical_run_spec_json.as_deref() != Some(canonical_json.as_str())
        || manifest.canonical_run_spec_fingerprint.as_deref()
            != Some(canonical_fingerprint.as_str())
        || summary_spec != Some(canonical_json.as_str())
        || summary_fingerprint != Some(canonical_fingerprint.as_str())
        || !input_components_match
        || candidates.len() as u64 != completed_candidates
        || completed_candidates != total_candidates
        || ranked.len() as u64 != succeeded_candidates
        || failed_candidates != total_candidates.saturating_sub(succeeded_candidates)
        || summary.get("ranking") != Some(&Value::Array(expected_ranking))
    {
        return Err(format!(
            "多腿实验 CanonicalRunSpec 与报告、RunManifest、候选排名或结果摘要不一致 {pointer}"
        ));
    }
    Ok(())
}

fn normalize_multi_market_depth_market_rule(
    value: &mut serde_json::Value,
    path: &str,
    pointer: &str,
) -> Result<(), String> {
    use serde_json::Value;
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(number) if number.is_f64() => Err(format!("{path} 含浮点数 {pointer}")),
        Value::Number(number) => {
            if number
                .as_i64()
                .is_some_and(|integer| integer.unsigned_abs() > 9_007_199_254_740_991)
                || number
                    .as_u64()
                    .is_some_and(|integer| integer > 9_007_199_254_740_991)
            {
                return Err(format!("{path} 含跨语言不安全整数 {pointer}"));
            }
            Ok(())
        }
        Value::Array(values) => {
            for (index, child) in values.iter_mut().enumerate() {
                normalize_multi_market_depth_market_rule(
                    child,
                    &format!("{path}[{index}]"),
                    pointer,
                )?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            for (name, child) in fields.iter_mut() {
                if (name.ends_with("_raw")
                    || matches!(
                        name.as_str(),
                        "contract_size" | "price_tick" | "qty_step" | "min_qty"
                    ))
                    && child.is_number()
                {
                    *child = Value::String(child.to_string());
                } else {
                    normalize_multi_market_depth_market_rule(
                        child,
                        &format!("{path}.{name}"),
                        pointer,
                    )?;
                }
            }
            Ok(())
        }
    }
}

fn verify_provider_lineage(
    spec: &qx_spec::CanonicalRunSpec,
    summary: &serde_json::Value,
    dataset: &qx_spec::CanonicalDatasetIdentity,
    pointer: &str,
) -> Result<(), String> {
    let canonical_lineage = spec.extensions.get("provider_lineage");
    let summary_lineage = summary
        .get("input")
        .and_then(serde_json::Value::as_object)
        .and_then(|input| input.get("provider_lineage"));
    match (canonical_lineage, summary_lineage) {
        (None, None) => return Ok(()),
        (Some(canonical), Some(declared)) if canonical == declared => {}
        _ => {
            return Err(format!(
                "Provider lineage 必须同时存在于 CanonicalRunSpec 和回测输入摘要且内容一致 {pointer}"
            ));
        }
    }
    let lineage = canonical_lineage.expect("matched Some");
    let field = |name: &str| -> Result<&str, String> {
        lineage
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("Provider lineage 缺少有效字段 {name} {pointer}"))
    };
    if lineage
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(format!(
            "Provider lineage schema_version 不受支持 {pointer}"
        ));
    }
    let request_json = field("request_json")?;
    let request = qx_provider::DataRequest::from_json(request_json)
        .map_err(|error| format!("Provider lineage request 无效 {pointer}: {}", error.message))?;
    if request.to_json().map_err(|error| error.message)? != request_json
        || request.query.kind != qx_provider::DataKind::Bar
        || request.query.instrument_set.len() != 1
        || request.query.instrument_set.iter().next() != Some(&dataset.instrument)
        || format!("{:016x}", request.query.digest()) != field("query_digest")?
    {
        return Err(format!(
            "Provider lineage request 与 Bar 数据身份不一致 {pointer}"
        ));
    }
    let manifest_json = field("dataset_manifest_json")?;
    if qx_data::sha256_hex_bytes(manifest_json.as_bytes()) != field("dataset_manifest_sha256")? {
        return Err(format!("Provider DatasetManifest SHA-256 不匹配 {pointer}"));
    }
    let provider_manifest = qx_data::DatasetManifestV2::from_json(manifest_json)
        .map_err(|error| format!("Provider DatasetManifest 无效 {pointer}: {error}"))?;
    for (label, actual, expected) in [
        (
            "instrument",
            provider_manifest.instrument.as_str(),
            dataset.instrument.as_str(),
        ),
        (
            "dataset fingerprint",
            provider_manifest.dataset.fingerprint.as_str(),
            dataset
                .content_fingerprint
                .strip_prefix("barframe:")
                .unwrap_or(dataset.content_fingerprint.as_str()),
        ),
        (
            "provider id",
            provider_manifest.source_lineage.primary_source.as_str(),
            field("provider_id")?,
        ),
        (
            "provider version",
            provider_manifest.provider_version.as_str(),
            field("provider_version")?,
        ),
    ] {
        if actual != expected {
            return Err(format!(
                "Provider DatasetManifest {label} 与 CanonicalRunSpec 不一致: {actual} != {expected} {pointer}"
            ));
        }
    }
    let source_hash = field("source_hash")?;
    if source_hash.len() != 16
        || !source_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "Provider DatasetManifest source metadata 无效 {pointer}"
        ));
    }
    let retry_chain = lineage
        .get("retry_chain")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("Provider lineage retry_chain 格式无效 {pointer}"))?;
    if retry_chain.iter().any(|value| value.as_str().is_none()) {
        return Err(format!(
            "Provider lineage retry_chain 包含非字符串值 {pointer}"
        ));
    }
    Ok(())
}

fn verify_multi_leg_canonical_run_spec(
    manifest: &qx_core::RunManifest,
    artifact: &serde_json::Value,
    canonical: &qx_spec::CanonicalRunSpec,
    pointer: &str,
) -> Result<(), String> {
    use crate::cases::{
        build_multi_leg_canonical_run_spec, effective_market_specs, multi_leg_input_components,
    };
    use crate::MultiLegBacktestSpec;
    use qx_spec::CanonicalDatasetIdentity;

    let spec: MultiLegBacktestSpec = serde_json::from_value(
        artifact
            .get("spec")
            .cloned()
            .ok_or_else(|| format!("多腿产物缺少 spec {pointer}"))?,
    )
    .map_err(|error| format!("多腿 CanonicalRunSpec 对账 spec 无效 {pointer}: {error}"))?;
    spec.validate()
        .map_err(|error| format!("多腿 CanonicalRunSpec 对账 spec 校验失败 {pointer}: {error}"))?;
    let market_data = artifact
        .get("market_data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("多腿产物缺少 market_data {pointer}"))?;
    if market_data.len() != 2 {
        return Err(format!("多腿 market_data 必须含两条腿 {pointer}"));
    }
    let read_data_identity =
        |index: usize, role: &str| -> Result<CanonicalDatasetIdentity, String> {
            let row = &market_data[index];
            let instrument = row
                .get("instrument")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("多腿 {role} market_data 缺少 instrument {pointer}"))?;
            let content_fingerprint = row
                .get("content_hash")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("多腿 {role} market_data 缺少 content_hash {pointer}"))?;
            let start_ts_ms = row
                .get("start_ts_ms")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| format!("多腿 {role} market_data 缺少 start_ts_ms {pointer}"))?;
            let end_ts_ms = row
                .get("end_ts_ms")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| format!("多腿 {role} market_data 缺少 end_ts_ms {pointer}"))?;
            Ok(CanonicalDatasetIdentity {
                role: role.into(),
                instrument: instrument.into(),
                content_fingerprint: content_fingerprint.into(),
                start_ts_ms,
                end_ts_ms,
            })
        };
    let datasets = vec![
        read_data_identity(0, "bars:primary")?,
        read_data_identity(1, "bars:reference")?,
    ];
    if datasets[0].instrument == datasets[1].instrument
        || artifact
            .get("primary_instrument")
            .and_then(serde_json::Value::as_str)
            != Some(datasets[0].instrument.as_str())
        || artifact
            .get("reference_instrument")
            .and_then(serde_json::Value::as_str)
            != Some(datasets[1].instrument.as_str())
        || market_data[0]
            .get("path")
            .and_then(serde_json::Value::as_str)
            != Some(spec.primary_bars_path.as_str())
        || market_data[1]
            .get("path")
            .and_then(serde_json::Value::as_str)
            != Some(spec.reference_bars_path.as_str())
    {
        return Err(format!(
            "多腿 market_data 顺序或输入路径与 spec 不一致 {pointer}"
        ));
    }
    let primary_instrument = qx_core::InstrumentId::parse(&datasets[0].instrument)
        .filter(|value| value.to_string() == datasets[0].instrument)
        .ok_or_else(|| format!("多腿主腿标的格式无效 {pointer}"))?;
    let reference_instrument = qx_core::InstrumentId::parse(&datasets[1].instrument)
        .filter(|value| value.to_string() == datasets[1].instrument)
        .ok_or_else(|| format!("多腿对冲腿标的格式无效 {pointer}"))?;
    let quality = artifact
        .get("dataset_quality")
        .filter(|value| !value.is_null());
    let provider = artifact
        .get("funding_provider_provenance")
        .filter(|value| !value.is_null());
    let strategy_id = artifact
        .get("strategy_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("多腿产物缺少 strategy_id {pointer}"))?;
    let strategy_version = artifact
        .get("strategy_version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("多腿产物缺少 strategy_version {pointer}"))?;
    let strategy_source_sha256 = artifact
        .get("strategy_source_sha256")
        .and_then(serde_json::Value::as_str);
    let account_id = artifact
        .get("account_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("多腿产物缺少 account_id {pointer}"))?;
    let expected_account_id = spec
        .account_id
        .as_deref()
        .map(str::to_string)
        .unwrap_or_else(|| format!("multi-leg-{}", spec.run_id));
    let (primary_market, reference_market) =
        effective_market_specs(&spec, &primary_instrument, &reference_instrument);
    let expected_effective_market_specs = serde_json::json!({
        "primary": primary_market,
        "reference": reference_market,
    });
    let expected_fill_model = serde_json::json!(spec.fill_model);
    if artifact.get("effective_market_specs") != Some(&expected_effective_market_specs)
        || artifact.get("fill_model") != Some(&expected_fill_model)
    {
        return Err(format!(
            "多腿有效市场规格或撮合模型与原始 spec 不一致 {pointer}"
        ));
    }
    let clock_start = artifact
        .get("clock_start")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("多腿产物缺少 clock_start {pointer}"))?;
    let clock_end = artifact
        .get("clock_end")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("多腿产物缺少 clock_end {pointer}"))?;
    let rebuilt = build_multi_leg_canonical_run_spec(
        &spec,
        datasets.clone(),
        quality,
        &primary_market,
        &reference_market,
        strategy_id,
        strategy_version,
        strategy_source_sha256,
        account_id,
        provider,
        &manifest.code_commit,
        &manifest.runtime_version,
        clock_start,
        clock_end,
    )
    .map_err(|error| format!("重建多腿 CanonicalRunSpec 失败 {pointer}: {error}"))?;
    let rebuilt_json = rebuilt
        .canonical_json()
        .map_err(|error| format!("重建多腿 CanonicalRunSpec 规范化失败 {pointer}: {error}"))?;
    if rebuilt_json
        != canonical
            .canonical_json()
            .map_err(|error| error.to_string())?
    {
        return Err(format!("多腿 CanonicalRunSpec 与输入产物不一致 {pointer}"));
    }
    let data_fingerprint = artifact
        .get("data_fingerprint")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("多腿产物缺少 data_fingerprint {pointer}"))?;
    let config_hash = artifact
        .get("config_hash")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("多腿产物缺少 config_hash {pointer}"))?;
    let result_hash = artifact
        .get("shared_result_hash")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("多腿产物缺少 shared_result_hash {pointer}"))?;
    let event_hash = artifact
        .get("event_hash")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("多腿产物缺少 event_hash {pointer}"))?;
    let components = multi_leg_input_components(&datasets, quality, provider)
        .map_err(|error| format!("重建多腿输入组件失败 {pointer}: {error}"))?;
    let canonical_fingerprint = rebuilt
        .fingerprint()
        .map_err(|error| format!("重建多腿 CanonicalRunSpec 指纹失败 {pointer}: {error}"))?;
    if account_id != expected_account_id
        || rebuilt.random_seed != manifest.global_seed.to_string()
        || rebuilt.clock.start_ts_ms != manifest.clock_start
        || rebuilt.clock.end_ts_ms != manifest.clock_end
        || clock_start != manifest.clock_start
        || clock_end != manifest.clock_end
        || manifest.run_id != spec.run_id
        || manifest.data_fingerprint != data_fingerprint
        || manifest.config_hash != config_hash
        || config_hash != canonical_fingerprint
        || manifest.result_hash != result_hash
        || manifest.output_event_hash != event_hash
        || manifest.input_event_hash != data_fingerprint
        || manifest.strategy_version != strategy_version
        || manifest.instrument_spec_version != "multi-leg-market-rules-v1"
        || manifest.model_fingerprint
            != qx_strategy::sha256_hex(
                &serde_json::to_vec(&rebuilt.execution_model).map_err(|error| {
                    format!("多腿 execution_model 指纹编码失败 {pointer}: {error}")
                })?,
            )
        || manifest.input_components != components
        || artifact.get("run_id").and_then(serde_json::Value::as_str) != Some(spec.run_id.as_str())
        || artifact
            .get("primary_instrument")
            .and_then(serde_json::Value::as_str)
            != Some(datasets[0].instrument.as_str())
        || artifact
            .get("reference_instrument")
            .and_then(serde_json::Value::as_str)
            != Some(datasets[1].instrument.as_str())
    {
        return Err(format!(
            "多腿 CanonicalRunSpec 或 RunManifest 与产物字段不一致 {pointer}"
        ));
    }
    Ok(())
}

fn verify_multi_market_canonical_run_spec(
    manifest: &qx_core::RunManifest,
    artifact: &serde_json::Value,
    canonical: &qx_spec::CanonicalRunSpec,
    pointer: &str,
) -> Result<(), String> {
    use crate::cases::{
        build_multi_market_canonical_run_spec, MultiMarketDataIdentity, MultiMarketExecutionWindow,
        StrategyArtifactIdentity,
    };
    use crate::MultiMarketBacktestSpec;

    let spec: MultiMarketBacktestSpec = serde_json::from_value(
        artifact
            .get("spec")
            .cloned()
            .ok_or_else(|| format!("多市场产物缺少 spec {pointer}"))?,
    )
    .map_err(|error| format!("多市场 CanonicalRunSpec 对账 spec 无效 {pointer}: {error}"))?;
    spec.validate().map_err(|error| {
        format!("多市场 CanonicalRunSpec 对账 spec 校验失败 {pointer}: {error}")
    })?;
    let mut data_identities: Vec<MultiMarketDataIdentity> = serde_json::from_value(
        artifact
            .get("market_data")
            .cloned()
            .ok_or_else(|| format!("多市场产物缺少 market_data {pointer}"))?,
    )
    .map_err(|error| format!("多市场 CanonicalRunSpec 对账 market_data 无效 {pointer}: {error}"))?;
    let quality_records = artifact
        .get("dataset_quality")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("多市场产物 dataset_quality 格式无效 {pointer}"))?;
    for data in &mut data_identities {
        data.quality_fingerprint = quality_records
            .get(&data.instrument)
            .and_then(|record| record.get("manifest_fingerprint"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
    }
    let identities: Vec<StrategyArtifactIdentity> = serde_json::from_value(
        artifact
            .get("strategy_identities")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .map_err(|error| format!("多市场 CanonicalRunSpec 策略身份无效 {pointer}: {error}"))?;
    let execution_window = artifact
        .get("execution_window")
        .cloned()
        .map(serde_json::from_value::<MultiMarketExecutionWindow>)
        .transpose()
        .map_err(|error| format!("多市场 CanonicalRunSpec 执行窗口无效 {pointer}: {error}"))?;
    let experiment_parameters = artifact
        .get("experiment_parameters")
        .cloned()
        .map(serde_json::from_value::<std::collections::BTreeMap<String, i128>>)
        .transpose()
        .map_err(|error| format!("多市场 CanonicalRunSpec 实验参数无效 {pointer}: {error}"))?;
    let rebuilt = build_multi_market_canonical_run_spec(
        &spec,
        &identities,
        &data_identities,
        &manifest.code_commit,
        &manifest.runtime_version,
        manifest.clock_start,
        manifest.clock_end,
        execution_window,
        experiment_parameters.as_ref(),
    )
    .map_err(|error| format!("重建多市场 CanonicalRunSpec 失败 {pointer}: {error}"))?;
    let rebuilt_json = rebuilt
        .canonical_json()
        .map_err(|error| format!("重建多市场 CanonicalRunSpec 规范化失败 {pointer}: {error}"))?;
    if rebuilt_json
        != canonical
            .canonical_json()
            .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "多市场 CanonicalRunSpec 与输入产物不一致 {pointer}"
        ));
    }
    if rebuilt.random_seed != manifest.global_seed.to_string()
        || rebuilt.clock.start_ts_ms != manifest.clock_start
        || rebuilt.clock.end_ts_ms != manifest.clock_end
        || rebuilt
            .execution_model
            .get("runtime_version")
            .and_then(serde_json::Value::as_str)
            != Some(manifest.runtime_version.as_str())
        || manifest.instrument_spec_version != "multi-market-spec-v1"
    {
        return Err(format!(
            "多市场 CanonicalRunSpec 与 RunManifest 不一致 {pointer}"
        ));
    }
    let outcome = artifact
        .get("outcome")
        .ok_or_else(|| format!("多市场产物缺少 outcome {pointer}"))?;
    let run_id = outcome
        .get("run_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("多市场 outcome 缺少 run_id {pointer}"))?;
    let data_fingerprint = outcome
        .get("data_fingerprint")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("多市场 outcome 缺少 data_fingerprint {pointer}"))?;
    let result_hash = outcome
        .get("shared_result_hash")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("多市场 outcome 缺少 shared_result_hash {pointer}"))?;
    let spec_fingerprint = outcome
        .get("spec_fingerprint")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("多市场 outcome 缺少 spec_fingerprint {pointer}"))?;
    if run_id != manifest.run_id
        || data_fingerprint != manifest.data_fingerprint
        || result_hash != manifest.result_hash
        || manifest.input_event_hash != manifest.data_fingerprint
        || manifest.strategy_version != format!("multi-market-strategy-set:{spec_fingerprint}")
        || !manifest.determinism_mode
    {
        return Err(format!(
            "多市场 outcome 与 RunManifest 身份不一致 {pointer}"
        ));
    }
    let market_data = artifact
        .get("market_data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("多市场产物 market_data 无效 {pointer}"))?;
    if canonical.datasets.len() != market_data.len()
        || canonical.datasets.len() != spec.markets.len()
    {
        return Err(format!(
            "多市场 CanonicalRunSpec 数据集数量与产物不一致 {pointer}"
        ));
    }
    for dataset in &canonical.datasets {
        let recorded = market_data
            .iter()
            .find(|market| {
                market.get("instrument").and_then(serde_json::Value::as_str)
                    == Some(dataset.instrument.as_str())
            })
            .ok_or_else(|| format!("多市场产物缺少 {} 的输入身份 {pointer}", dataset.instrument))?;
        if recorded
            .get("content_hash")
            .and_then(serde_json::Value::as_str)
            != Some(dataset.content_fingerprint.as_str())
            || recorded
                .get("start_ts_ms")
                .and_then(serde_json::Value::as_u64)
                != Some(dataset.start_ts_ms)
            || recorded
                .get("end_ts_ms")
                .and_then(serde_json::Value::as_u64)
                != Some(dataset.end_ts_ms)
        {
            return Err(format!(
                "多市场 CanonicalRunSpec 与 {} 的市场数据身份不一致 {pointer}",
                dataset.instrument
            ));
        }
        if manifest
            .input_components
            .get(&format!("barframe:{}", dataset.instrument))
            != Some(&dataset.content_fingerprint)
        {
            return Err(format!(
                "多市场 RunManifest 缺少 {} 的输入组件指纹 {pointer}",
                dataset.instrument
            ));
        }
    }
    Ok(())
}

fn verify_multi_market_depth_canonical_run_spec(
    manifest: &qx_core::RunManifest,
    artifact: &serde_json::Value,
    canonical: &qx_spec::CanonicalRunSpec,
    pointer: &str,
) -> Result<(), String> {
    use crate::cases::{build_multi_market_depth_canonical_run_spec, StrategyArtifactIdentity};
    use crate::{
        MultiMarketDepthBacktestOutcome, MultiMarketDepthBacktestSpec,
        MultiMarketDepthInputIdentityV1,
    };
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct MarketDataIdentity {
        instrument: String,
        data_tier: String,
        source: String,
        content_hash: String,
        snapshot_count: u64,
        start_ts_ms: u64,
        end_ts_ms: u64,
        dataset_quality: Option<serde_json::Value>,
    }

    let spec: MultiMarketDepthBacktestSpec = serde_json::from_value(
        artifact
            .get("spec")
            .cloned()
            .ok_or_else(|| format!("多市场深度产物缺少 spec {pointer}"))?,
    )
    .map_err(|error| format!("多市场深度 CanonicalRunSpec spec 无效 {pointer}: {error}"))?;
    spec.validate()
        .map_err(|error| format!("多市场深度 CanonicalRunSpec spec 校验失败 {pointer}: {error}"))?;
    let market_data: Vec<MarketDataIdentity> = serde_json::from_value(
        artifact
            .get("market_data")
            .cloned()
            .ok_or_else(|| format!("多市场深度产物缺少 market_data {pointer}"))?,
    )
    .map_err(|error| format!("多市场深度 CanonicalRunSpec market_data 无效 {pointer}: {error}"))?;
    if market_data.len() != spec.markets.len() || market_data.len() != canonical.datasets.len() {
        return Err(format!(
            "多市场深度 CanonicalRunSpec 数据集数量与产物不一致 {pointer}"
        ));
    }
    let data_identities = market_data
        .iter()
        .map(|data| {
            let input_hash = u64::from_str_radix(&data.content_hash, 16).map_err(|error| {
                format!(
                    "{} DepthFrame content_hash 无效 {pointer}: {error}",
                    data.instrument
                )
            })?;
            if format!("{input_hash:016x}") != data.content_hash
                || data.snapshot_count == 0
                || data.start_ts_ms > data.end_ts_ms
            {
                return Err(format!(
                    "{} DepthFrame 数据身份非规范 {pointer}",
                    data.instrument
                ));
            }
            let quality_fingerprint = data
                .dataset_quality
                .as_ref()
                .and_then(|quality| quality.get("manifest_fingerprint"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            Ok(MultiMarketDepthInputIdentityV1 {
                instrument: data.instrument.clone(),
                data_tier: data.data_tier.clone(),
                source: data.source.clone(),
                input_hash,
                start_ts_ms: data.start_ts_ms,
                end_ts_ms: data.end_ts_ms,
                quality_fingerprint,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let identities: Vec<StrategyArtifactIdentity> = serde_json::from_value(
        artifact
            .get("strategy_identities")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .map_err(|error| format!("多市场深度 CanonicalRunSpec 策略身份无效 {pointer}: {error}"))?;
    let outcome: MultiMarketDepthBacktestOutcome = serde_json::from_value(
        artifact
            .get("outcome")
            .cloned()
            .ok_or_else(|| format!("多市场深度产物缺少 outcome {pointer}"))?,
    )
    .map_err(|error| format!("多市场深度 CanonicalRunSpec outcome 无效 {pointer}: {error}"))?;
    let clock_start = artifact
        .get("clock_start")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("多市场深度产物缺少合法 clock_start {pointer}"))?;
    let clock_end = artifact
        .get("clock_end")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("多市场深度产物缺少合法 clock_end {pointer}"))?;
    let rebuilt = build_multi_market_depth_canonical_run_spec(
        &spec,
        &identities,
        &data_identities,
        &outcome.execution_models,
        &manifest.code_commit,
        &manifest.runtime_version,
        clock_start,
        clock_end,
    )
    .map_err(|error| format!("重建多市场深度 CanonicalRunSpec 失败 {pointer}: {error}"))?;
    let rebuilt_json = rebuilt.canonical_json().map_err(|error| {
        format!("重建多市场深度 CanonicalRunSpec 规范化失败 {pointer}: {error}")
    })?;
    if rebuilt_json
        != canonical
            .canonical_json()
            .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "多市场深度 CanonicalRunSpec 与输入产物不一致 {pointer}"
        ));
    }
    let data_identity = crate::multi_market_depth_input_set_identity_v1(&spec, &data_identities)
        .map_err(|error| format!("多市场深度输入身份重建失败 {pointer}: {error}"))?;
    let spec_fingerprint = crate::multi_market_depth_spec_fingerprint_v1(&spec, &identities)
        .map_err(|error| format!("多市场深度规格指纹重建失败 {pointer}: {error}"))?;
    if clock_start != manifest.clock_start
        || clock_end != manifest.clock_end
        || rebuilt.random_seed != manifest.global_seed.to_string()
        || rebuilt.random_seed != "0"
        || rebuilt
            .execution_model
            .get("runtime_version")
            .and_then(serde_json::Value::as_str)
            != Some(manifest.runtime_version.as_str())
        || manifest.instrument_spec_version != "multi-market-depth-spec-v1"
        || manifest.data_fingerprint != data_identity.data_fingerprint
        || manifest.input_components != data_identity.input_components
        || manifest.input_event_hash != manifest.data_fingerprint
        || manifest.result_hash != outcome.shared_result_hash
        || manifest.output_event_hash != manifest.result_hash
        || manifest.strategy_version != format!("multi-market-depth:{spec_fingerprint}")
        || manifest.model_fingerprint
            != qx_strategy::sha256_hex(
                b"OrderBookPortfolioBacktestEngine/OrderBookExecutionModel/v1",
            )
        || manifest.run_id != outcome.run_id
        || outcome.account_id != spec.account_id
        || outcome.run_id != spec.run_id
        || outcome.markets_total != spec.markets.len()
        || outcome.data_fingerprint != data_identity.data_fingerprint
        || outcome.spec_fingerprint != spec_fingerprint
        || !manifest.determinism_mode
    {
        return Err(format!(
            "多市场深度 CanonicalRunSpec 与 RunManifest/outcome 不一致 {pointer}"
        ));
    }
    for dataset in &canonical.datasets {
        let data = market_data
            .iter()
            .find(|data| data.instrument == dataset.instrument)
            .ok_or_else(|| {
                format!(
                    "多市场深度产物缺少 {} 的输入身份 {pointer}",
                    dataset.instrument
                )
            })?;
        if dataset.content_fingerprint != format!("depth-frame-fnv1a64:{}", data.content_hash)
            || dataset.start_ts_ms != data.start_ts_ms
            || dataset.end_ts_ms != data.end_ts_ms
            || dataset.role != "depth_snapshots"
        {
            return Err(format!(
                "多市场深度 CanonicalRunSpec 与 {} 的市场数据身份不一致 {pointer}",
                dataset.instrument
            ));
        }
        let component = data_identity
            .input_components
            .get(&format!("depthframe:{}", dataset.instrument));
        if component.is_none() {
            return Err(format!(
                "多市场深度 RunManifest 缺少 {} 的输入组件指纹 {pointer}",
                dataset.instrument
            ));
        }
    }
    Ok(())
}

fn verify_multi_account_joint_canonical_run_spec(
    manifest: &qx_core::RunManifest,
    artifact: &serde_json::Value,
    canonical: &qx_spec::CanonicalRunSpec,
    pointer: &str,
) -> Result<(), String> {
    use crate::cases::{
        build_joint_run_identity, joint_child_run_id, joint_result_hash,
        JointAccountRunVerification,
    };
    use crate::{
        MultiAccountJointBacktestOutcome, MultiAccountJointBacktestSpec, MultiMarketBacktestSpec,
    };
    use qx_core::Fnv1a;

    let spec: MultiAccountJointBacktestSpec = serde_json::from_value(
        artifact
            .get("spec")
            .cloned()
            .ok_or_else(|| format!("联合账户产物缺少 spec {pointer}"))?,
    )
    .map_err(|error| format!("联合账户 CanonicalRunSpec spec 无效 {pointer}: {error}"))?;
    spec.validate()
        .map_err(|error| format!("联合账户 CanonicalRunSpec spec 校验失败 {pointer}: {error}"))?;
    let outcome: MultiAccountJointBacktestOutcome = serde_json::from_value(
        artifact
            .get("outcome")
            .cloned()
            .ok_or_else(|| format!("联合账户产物缺少 outcome {pointer}"))?,
    )
    .map_err(|error| format!("联合账户 CanonicalRunSpec outcome 无效 {pointer}: {error}"))?;
    let children: Vec<JointAccountRunVerification> = serde_json::from_value(
        artifact
            .get("account_run_verifications")
            .cloned()
            .ok_or_else(|| format!("联合账户产物缺少 account_run_verifications {pointer}"))?,
    )
    .map_err(|error| format!("联合账户子运行身份无效 {pointer}: {error}"))?;
    if children.len() != spec.accounts.len() || outcome.accounts.len() != spec.accounts.len() {
        return Err(format!("联合账户规格/结果/子运行数量不一致 {pointer}"));
    }

    let mut accounts = spec.accounts.clone();
    accounts.sort_by(|left, right| {
        left.exchange_id
            .cmp(&right.exchange_id)
            .then_with(|| left.account_id.cmp(&right.account_id))
    });
    let mut verified_children = Vec::with_capacity(children.len());
    let mut initial_cash_raw = 0_i128;
    let mut final_equity_raw = 0_i128;
    let mut successful_fills = 0_u64;
    let mut markets_total = 0_usize;
    for (index, ((account, child), result)) in accounts
        .iter()
        .zip(&children)
        .zip(&outcome.accounts)
        .enumerate()
    {
        if account.exchange_id != child.exchange_id
            || account.account_id != child.account_id
            || account.exchange_id != result.exchange_id
            || account.account_id != result.account_id
            || account.initial_cash_raw != result.initial_cash_raw
        {
            return Err(format!(
                "联合账户第 {} 个账户的规格、子运行与 outcome 顺序/身份不一致 {pointer}",
                index + 1
            ));
        }
        let expected_child_run_id =
            joint_child_run_id(&spec.run_id, &account.exchange_id, &account.account_id);
        if child.run_manifest.run_id != expected_child_run_id {
            return Err(format!(
                "联合账户 {}:{} 子运行 run_id 不符合确定性映射 {pointer}",
                account.exchange_id, account.account_id
            ));
        }
        child
            .run_manifest
            .validate()
            .map_err(|error| format!("联合账户子运行 RunManifest 无效 {pointer}: {error}"))?;
        let child_spec = MultiMarketBacktestSpec {
            schema_version: crate::MULTI_MARKET_BACKTEST_SCHEMA_VERSION,
            run_id: expected_child_run_id,
            account_id: account.account_id.clone(),
            settlement_currency: spec.settlement_currency.clone(),
            initial_cash_raw: account.initial_cash_raw,
            initial_cash_exact: None,
            seed: spec.seed,
            output_dir: spec.output_dir.clone(),
            markets: account.markets.clone(),
        };
        let child_outcome = child
            .identity
            .get("outcome")
            .ok_or_else(|| format!("联合账户子运行身份缺少 outcome {pointer}"))?;
        if child_outcome
            != &serde_json::to_value(&result.outcome)
                .map_err(|error| format!("联合账户子运行 outcome 编码失败 {pointer}: {error}"))?
        {
            return Err(format!(
                "联合账户子运行 outcome 与父 outcome 不一致 {pointer}"
            ));
        }
        let mut child_artifact = child.identity.clone();
        let child_artifact_fields = child_artifact
            .as_object_mut()
            .ok_or_else(|| format!("联合账户子运行 identity 不是对象 {pointer}"))?;
        child_artifact_fields.insert(
            "spec".into(),
            serde_json::to_value(&child_spec)
                .map_err(|error| format!("联合账户子规格编码失败 {pointer}: {error}"))?,
        );
        verify_canonical_run_spec(&child.run_manifest, &child_artifact, pointer)?;

        initial_cash_raw = initial_cash_raw
            .checked_add(account.initial_cash_raw)
            .ok_or_else(|| format!("联合账户初始资金合计溢出 {pointer}"))?;
        final_equity_raw = final_equity_raw
            .checked_add(result.outcome.final_equity_raw)
            .ok_or_else(|| format!("联合账户期末权益合计溢出 {pointer}"))?;
        successful_fills = successful_fills
            .checked_add(result.outcome.successful_fills)
            .ok_or_else(|| format!("联合账户成交数合计溢出 {pointer}"))?;
        markets_total = markets_total
            .checked_add(account.markets.len())
            .ok_or_else(|| format!("联合账户市场数溢出 {pointer}"))?;
        verified_children.push(child.clone());
    }

    let identity = build_joint_run_identity(
        &spec,
        &verified_children,
        &manifest.code_commit,
        &manifest.runtime_version,
    )
    .map_err(|error| format!("重建联合账户 CanonicalRunSpec 失败 {pointer}: {error}"))?;
    let rebuilt_json = identity
        .canonical_spec
        .canonical_json()
        .map_err(|error| format!("联合账户 CanonicalRunSpec 规范化失败 {pointer}: {error}"))?;
    if rebuilt_json
        != canonical
            .canonical_json()
            .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "联合账户 CanonicalRunSpec 与子运行身份不一致 {pointer}"
        ));
    }
    let canonical_fingerprint = identity
        .canonical_spec
        .fingerprint()
        .map_err(|error| format!("联合账户 CanonicalRunSpec 指纹失败 {pointer}: {error}"))?;
    let mut legacy_spec = spec.clone();
    legacy_spec.accounts = accounts;
    legacy_spec.output_dir.clear();
    for account in &mut legacy_spec.accounts {
        for market in &mut account.markets {
            market.bars_path.clear();
            market.dataset_manifest_path = None;
        }
    }
    let legacy_json = legacy_spec
        .to_json()
        .map_err(|error| format!("联合账户历史规格指纹序列化失败 {pointer}: {error}"))?;
    let mut legacy_hasher = Fnv1a::new();
    legacy_hasher.write_text(&legacy_json);
    let legacy_spec_fingerprint = format!("{:016x}", legacy_hasher.finish());

    let shared_result_hash = artifact
        .get("shared_result_hash")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("联合账户产物缺少 shared_result_hash {pointer}"))?;
    let result_hash = joint_result_hash(
        &canonical_fingerprint,
        &outcome.accounts,
        initial_cash_raw,
        final_equity_raw,
        outcome.return_bps,
        outcome.max_drawdown_bps,
        successful_fills,
        markets_total,
    );
    let expected_return_bps = final_equity_raw
        .checked_sub(initial_cash_raw)
        .and_then(|pnl| pnl.checked_mul(10_000))
        .and_then(|scaled| scaled.checked_div(initial_cash_raw))
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| format!("联合账户收益无法复核 {pointer}"))?;
    let run_manifest_path = artifact
        .get("run_manifest_path")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("联合账户产物缺少 RunManifest 路径 {pointer}"))?;
    if artifact
        .get("run_manifest")
        .and_then(serde_json::Value::as_str)
        != Some(run_manifest_path)
        || outcome.run_manifest_path.as_deref() != Some(run_manifest_path)
        || outcome.run_id != spec.run_id
        || outcome.settlement_currency != spec.settlement_currency
        || outcome.accounts_total != spec.accounts.len()
        || outcome.markets_total != markets_total
        || outcome.initial_cash_raw != initial_cash_raw
        || outcome.final_equity_raw != final_equity_raw
        || outcome.successful_fills != successful_fills
        || outcome.return_bps != expected_return_bps
        || outcome.spec_fingerprint != legacy_spec_fingerprint
        || artifact
            .get("result_hash")
            .and_then(serde_json::Value::as_str)
            != Some(result_hash.as_str())
        || shared_result_hash != result_hash
        || manifest.result_hash != result_hash
        || manifest.data_fingerprint != identity.data_fingerprint
        || manifest.input_components != identity.input_components
        || manifest.clock_start != identity.clock_start
        || manifest.clock_end != identity.clock_end
        || manifest.global_seed != spec.seed
        || manifest.config_hash != legacy_spec_fingerprint
        || manifest.input_event_hash != manifest.data_fingerprint
        || manifest.output_event_hash != result_hash
        || manifest.strategy_version
            != format!("multi-account-joint-strategies:{canonical_fingerprint}")
        || manifest.instrument_spec_version != "multi-account-joint-spec-v1"
        || manifest.model_fingerprint
            != qx_strategy::sha256_hex(b"MultiAccountJointBacktestEngine/v1")
        || !manifest.determinism_mode
        || artifact
            .get("data_fingerprint")
            .and_then(serde_json::Value::as_str)
            != Some(identity.data_fingerprint.as_str())
        || artifact
            .get("input_data_hash")
            .and_then(serde_json::Value::as_str)
            != Some(identity.data_fingerprint.as_str())
        || artifact
            .get("clock_start")
            .and_then(serde_json::Value::as_u64)
            != Some(identity.clock_start)
        || artifact
            .get("clock_end")
            .and_then(serde_json::Value::as_u64)
            != Some(identity.clock_end)
        || artifact
            .get("aggregation")
            .and_then(serde_json::Value::as_str)
            != Some("sum_of_independent_exchange_account_equity_no_cross_account_margin_netting")
    {
        return Err(format!(
            "联合账户 CanonicalRunSpec 与 RunManifest/outcome 不一致 {pointer}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_spec::{
        CanonicalDatasetIdentity, CanonicalRunClock, CanonicalRunSpec, CanonicalStrategyIdentity,
    };
    use std::collections::BTreeMap;

    fn fixture() -> (qx_core::RunManifest, serde_json::Value) {
        let spec = CanonicalRunSpec::new(
            "bar_backtest",
            "commit-1",
            vec![CanonicalStrategyIdentity {
                strategy_id: "user.Strategy:main".into(),
                strategy_version: "strategy-v1".into(),
                source_sha256: None,
                parameters: serde_json::json!({
                    "strategy_id": "user.Strategy:main",
                    "window": 3
                }),
            }],
            vec![CanonicalDatasetIdentity {
                role: "bars".into(),
                instrument: "BTCUSDT.BINANCE".into(),
                content_fingerprint: "bars-fp".into(),
                start_ts_ms: 1_000,
                end_ts_ms: 2_000,
            }],
            BTreeMap::from([("BTCUSDT.BINANCE".into(), "rules-v1".into())]),
            CanonicalRunClock {
                start_ts_ms: 1_000,
                end_ts_ms: 2_000,
                timestamp_unit: "unix_ms".into(),
                timezone: "UTC".into(),
            },
            7,
            serde_json::json!({
                "instrument": "BTCUSDT.BINANCE",
                "seed": "7",
                "strategy": {
                    "strategy_id": "user.Strategy:main",
                    "window": 3
                }
            }),
            serde_json::json!({
                "runtime_version": "qx-app-test",
                "account_id": "app"
            }),
            BTreeMap::new(),
        )
        .unwrap();
        let manifest = qx_core::RunManifest {
            data_fingerprint: "bars-fp".into(),
            clock_start: 1_000,
            clock_end: 2_000,
            global_seed: 7,
            code_commit: "commit-1".into(),
            strategy_version: "strategy-v1".into(),
            instrument_spec_version: "rules-v1".into(),
            runtime_version: "qx-app-test".into(),
            canonical_run_spec_json: Some(spec.canonical_json().unwrap()),
            canonical_run_spec_fingerprint: Some(spec.fingerprint().unwrap()),
            ..Default::default()
        };
        let summary = serde_json::json!({
            "instrument": "BTCUSDT.BINANCE",
            "seed": 7,
            "account_id": "app",
            "data_fingerprint": "bars-fp",
            "clock_start": 1_000,
            "clock_end": 2_000
        });
        (manifest, summary)
    }

    #[test]
    fn canonical_run_spec_read_verification_binds_legacy_manifest_facts() {
        let (manifest, summary) = fixture();
        verify_canonical_run_spec(&manifest, &summary, "test.run.json").unwrap();

        let mut changed = manifest.clone();
        changed.data_fingerprint = "different-bars".into();
        assert!(
            verify_canonical_run_spec(&changed, &summary, "test.run.json")
                .unwrap_err()
                .contains("数据指纹")
        );

        let mut spec = qx_spec::CanonicalRunSpec::from_json(
            manifest.canonical_run_spec_json.as_deref().unwrap(),
        )
        .unwrap();
        spec.behavior["strategy"]["window"] = serde_json::json!(4);
        let mut changed = manifest.clone();
        changed.canonical_run_spec_json = Some(spec.canonical_json().unwrap());
        changed.canonical_run_spec_fingerprint = Some(spec.fingerprint().unwrap());
        assert!(
            verify_canonical_run_spec(&changed, &summary, "test.run.json")
                .unwrap_err()
                .contains("策略身份与 behavior")
        );

        let mut changed = manifest;
        changed.global_seed = 8;
        assert!(
            verify_canonical_run_spec(&changed, &summary, "test.run.json")
                .unwrap_err()
                .contains("随机种子")
        );
    }

    #[test]
    fn canonical_run_spec_read_verification_keeps_legacy_manifests_readable() {
        verify_canonical_run_spec(
            &qx_core::RunManifest::default(),
            &serde_json::Value::Null,
            "legacy.run.json",
        )
        .unwrap();
    }

    #[test]
    fn canonical_run_spec_json_and_fingerprint_must_be_present_as_a_pair() {
        let (manifest, summary) = fixture();
        let mut missing_fingerprint = manifest.clone();
        missing_fingerprint.canonical_run_spec_fingerprint = None;
        assert!(
            verify_canonical_run_spec(&missing_fingerprint, &summary, "partial.json")
                .unwrap_err()
                .contains("必须成对出现")
        );

        let mut missing_json = manifest;
        missing_json.canonical_run_spec_json = None;
        assert!(
            verify_canonical_run_spec(&missing_json, &summary, "partial.json")
                .unwrap_err()
                .contains("必须成对出现")
        );
    }

    #[test]
    fn depth_canonical_read_verification_binds_depth_summary_facts() {
        let strategy = CanonicalStrategyIdentity {
            strategy_id: "depth-sma".into(),
            strategy_version: "builtin-sma_cross-v1".into(),
            source_sha256: None,
            parameters: serde_json::json!({"strategy_id": "depth-sma", "fast_window": 2}),
        };
        let execution = serde_json::json!({
            "runtime_version": "depth-backtest-v1",
            "account_id": "app",
            "execution_model_descriptor": {"fee_bps": 0}
        });
        let spec = CanonicalRunSpec::new(
            "depth_backtest",
            "commit-depth",
            vec![strategy],
            vec![CanonicalDatasetIdentity {
                role: "depth_snapshots".into(),
                instrument: "BTCUSDT.BINANCE".into(),
                content_fingerprint: "depth-frame-fnv1a64:0000000000000001".into(),
                start_ts_ms: 1_000,
                end_ts_ms: 2_000,
            }],
            BTreeMap::from([("BTCUSDT.BINANCE".into(), "instrument-spec:none".into())]),
            CanonicalRunClock {
                start_ts_ms: 1_000,
                end_ts_ms: 2_000,
                timestamp_unit: "unix_ms".into(),
                timezone: "UTC".into(),
            },
            0,
            serde_json::json!({
                "tier": "l1",
                "strategy": {"strategy_id": "depth-sma", "fast_window": 2}
            }),
            execution.clone(),
            BTreeMap::new(),
        )
        .unwrap();
        let manifest = qx_core::RunManifest {
            data_fingerprint: "depth-fp".into(),
            clock_start: 1_000,
            clock_end: 2_000,
            global_seed: 0,
            code_commit: "commit-depth".into(),
            strategy_version: "builtin-sma_cross-v1".into(),
            instrument_spec_version: "instrument-spec:none".into(),
            runtime_version: "depth-backtest-v1".into(),
            canonical_run_spec_json: Some(spec.canonical_json().unwrap()),
            canonical_run_spec_fingerprint: Some(spec.fingerprint().unwrap()),
            ..Default::default()
        };
        let summary = serde_json::json!({
            "instrument": "BTCUSDT.BINANCE",
            "data_fingerprint": "depth-fp",
            "input": {"fingerprint": "0000000000000001"},
            "clock_start": 1_000,
            "clock_end": 2_000,
            "tier": "l1",
            "execution_model": {"fee_bps": 0}
        });
        verify_canonical_run_spec(&manifest, &summary, "depth.run.json").unwrap();

        let mut changed_summary = summary;
        changed_summary["execution_model"]["fee_bps"] = serde_json::json!(1);
        assert!(
            verify_canonical_run_spec(&manifest, &changed_summary, "depth.run.json")
                .unwrap_err()
                .contains("撮合描述")
        );
    }
}
