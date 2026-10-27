//! Canonical parent identity for independently funded account batches.

use crate::artifact_store::{
    read_text_bounded, verify_declared_run_manifest_with_reader, ArtifactReader, FileArtifactReader,
};
use crate::error::{AppError, AppErrorCategory};
use crate::spec::{
    MultiAccountBacktestResult, MultiAccountBacktestSpec, MultiAccountBacktestSummary,
};
use qx_core::{Fnv1a, RunManifest};
use qx_spec::{
    CanonicalDatasetIdentity, CanonicalRunClock, CanonicalRunSpec, CanonicalStrategyIdentity,
};
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_CHILD_IDENTITY_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;

pub(crate) struct MultiAccountRunIdentity {
    pub canonical_spec: CanonicalRunSpec,
    pub data_fingerprint: String,
    pub input_components: BTreeMap<String, String>,
    pub clock_start: u64,
    pub clock_end: u64,
}

pub(crate) fn build_multi_account_run_manifest(
    spec: &MultiAccountBacktestSpec,
    result: &MultiAccountBacktestResult,
    code_revision: &str,
    runtime_version: &str,
) -> Result<Option<RunManifest>, AppError> {
    let Some(identity) =
        build_multi_account_run_identity(spec, &result.runs, code_revision, runtime_version)?
    else {
        // An all-failed batch consumed no data and is not a trading run. Its per-account
        // failures remain visible, but it cannot claim a dataset-backed RunManifest.
        return Ok(None);
    };
    let canonical_spec_json = identity
        .canonical_spec
        .canonical_json()
        .map_err(internal_error)?;
    let canonical_spec_fingerprint = identity
        .canonical_spec
        .fingerprint()
        .map_err(internal_error)?;
    let result_hash =
        multi_account_result_hash(&canonical_spec_fingerprint, &result.runs, &result.summary);
    let manifest = RunManifest {
        run_id: spec.batch_id.clone(),
        code_commit: code_revision.to_string(),
        config_hash: result.spec_fingerprint.clone(),
        data_fingerprint: identity.data_fingerprint.clone(),
        input_components: identity.input_components,
        clock_start: identity.clock_start,
        clock_end: identity.clock_end,
        // The batch scheduler has no RNG of its own; each child seed is retained in behavior and
        // independently declared by that child's RunManifest.
        global_seed: 0,
        // Partial account failures affect completeness, not determinism. Every successful member
        // is itself verified against a deterministic child manifest.
        determinism_mode: true,
        result_hash: result_hash.clone(),
        strategy_version: format!("multi-account-batch:{canonical_spec_fingerprint}"),
        instrument_spec_version: "multi-account-bar-batch-v1".into(),
        model_fingerprint: qx_strategy::sha256_hex(b"MultiAccountBacktestBatch/v1"),
        input_event_hash: identity.data_fingerprint.clone(),
        output_event_hash: result_hash,
        runtime_version: runtime_version.to_string(),
        canonical_run_spec_json: Some(canonical_spec_json),
        canonical_run_spec_fingerprint: Some(canonical_spec_fingerprint),
    };
    manifest.validate().map_err(internal_error)?;
    Ok(Some(manifest))
}

pub(crate) fn verify_multi_account_backtest_result(
    result: &MultiAccountBacktestResult,
    reader: &dyn ArtifactReader,
) -> Result<(), String> {
    if result.batch_id.trim().is_empty() || result.runs.len() < 2 || result.runs.len() > 32 {
        return Err("多账户 Bar 批次身份或账户数量无效".into());
    }
    if result.schema_version != crate::spec::MULTI_ACCOUNT_BACKTEST_RESULT_SCHEMA_VERSION {
        return Err(format!(
            "多账户 Bar 父身份 verifier 只接受结果 v{}",
            crate::spec::MULTI_ACCOUNT_BACKTEST_RESULT_SCHEMA_VERSION
        ));
    }
    let Some(parent_manifest) = result.run_manifest.as_ref() else {
        return verify_all_failed_result(result);
    };
    parent_manifest.validate()?;
    let canonical_json = parent_manifest
        .canonical_run_spec_json
        .as_deref()
        .ok_or("多账户 Bar 父 RunManifest 缺少 CanonicalRunSpec")?;
    let canonical = CanonicalRunSpec::from_json(canonical_json)
        .map_err(|error| format!("多账户父 CanonicalRunSpec 无效: {error}"))?;
    if canonical
        .canonical_json()
        .map_err(|error| error.to_string())?
        != canonical_json
    {
        return Err("多账户父 CanonicalRunSpec 不是规范 JSON".into());
    }
    let canonical_fingerprint = canonical.fingerprint().map_err(|error| error.to_string())?;
    if canonical.run_type != "multi_account_backtest"
        || parent_manifest.canonical_run_spec_fingerprint.as_deref()
            != Some(canonical_fingerprint.as_str())
        || parent_manifest.run_id != result.batch_id
        || parent_manifest.config_hash != result.spec_fingerprint
        || result.spec_fingerprint.len() != 16
        || !result
            .spec_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || !parent_manifest.determinism_mode
        || parent_manifest.global_seed != 0
        || parent_manifest.instrument_spec_version != "multi-account-bar-batch-v1"
        || parent_manifest.model_fingerprint
            != qx_strategy::sha256_hex(b"MultiAccountBacktestBatch/v1")
        || parent_manifest.strategy_version
            != format!("multi-account-batch:{canonical_fingerprint}")
        || parent_manifest.input_event_hash != parent_manifest.data_fingerprint
        || parent_manifest.output_event_hash != parent_manifest.result_hash
    {
        return Err("多账户 Bar 父 RunManifest 与批次身份字段不一致".into());
    }

    let behavior = canonical
        .behavior
        .as_object()
        .ok_or("多账户父 CanonicalRunSpec behavior 不是对象")?;
    let behavior_runs = behavior
        .get("runs")
        .and_then(Value::as_array)
        .ok_or("多账户父 CanonicalRunSpec 缺少 behavior.runs")?;
    if behavior_runs.len() != result.runs.len() {
        return Err("多账户父 CanonicalRunSpec 与结果账户数量不一致".into());
    }
    let ids = result
        .runs
        .iter()
        .map(|run| run.account_id.as_str())
        .collect::<Vec<_>>();
    if ids.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err("多账户结果账户必须唯一且按 account_id 排序".into());
    }

    let mut successful = 0_usize;
    let mut weighted_returns = crate::report_analysis::WeightedReturnAccumulatorV1::default();
    let mut successful_fills = 0_u64;
    let mut datasets = Vec::new();
    let mut strategies = Vec::new();
    let mut market_rules = BTreeMap::new();
    let mut extensions = BTreeMap::new();
    let mut input_components = BTreeMap::new();
    let mut input_hasher = Fnv1a::new();
    let mut clock_start = u64::MAX;
    let mut clock_end = 0_u64;
    let mut account_execution_models = Vec::new();

    for (behavior_run, run) in behavior_runs.iter().zip(&result.runs) {
        let behavior_fields = behavior_run
            .as_object()
            .ok_or("多账户父 CanonicalRunSpec 中的账户项不是对象")?;
        let account_id = behavior_fields
            .get("account_id")
            .and_then(Value::as_str)
            .ok_or("多账户父 CanonicalRunSpec 账户项缺少 account_id")?;
        if account_id != run.account_id {
            return Err("多账户父 CanonicalRunSpec 账户顺序与结果不一致".into());
        }
        if behavior_fields
            .get("settlement_currency")
            .and_then(Value::as_str)
            != Some(result.settlement_currency.as_str())
        {
            return Err("多账户账户结算币与父 CanonicalRunSpec 不一致".into());
        }
        let status = behavior_fields
            .get("account_run_status")
            .and_then(Value::as_str)
            .ok_or("多账户父 CanonicalRunSpec 账户项缺少 account_run_status")?;
        if let Some(outcome) = &run.outcome {
            successful += 1;
            if status != "succeeded"
                || run.error_json.is_some()
                || outcome.account_id.as_deref() != Some(account_id)
            {
                return Err("多账户成功 outcome 状态或账户身份不一致".into());
            }
            let initial_cash_raw = behavior_fields
                .get("initial_cash_raw")
                .and_then(Value::as_str)
                .ok_or("多账户父 behavior 缺少十进制 initial_cash_raw")?
                .parse::<i128>()
                .map_err(|error| format!("多账户 initial_cash_raw 无效: {error}"))?;
            if initial_cash_raw <= 0 {
                return Err("多账户父 behavior 的 initial_cash_raw 必须为正数".into());
            }
            successful_fills = successful_fills
                .checked_add(outcome.fills)
                .ok_or("多账户复核成交数求和溢出")?;
            weighted_returns
                .push(outcome.return_bps, initial_cash_raw)
                .map_err(|error| format!("多账户复核{error}"))?;

            let child_summary_body = read_text_bounded(
                reader,
                &outcome.artifacts.summary,
                MAX_CHILD_IDENTITY_ARTIFACT_BYTES,
                "多账户子回测摘要",
            )
            .map_err(|error| format!("读取多账户子回测摘要失败: {error}"))?;
            let child_summary: Value = serde_json::from_str(&child_summary_body)
                .map_err(|error| format!("多账户子回测摘要 JSON 无效: {error}"))?;
            verify_declared_run_manifest_with_reader(&child_summary, reader)
                .map_err(|error| format!("多账户子回测 RunManifest 核验失败: {error}"))?;
            let child_manifest_body = read_text_bounded(
                reader,
                &outcome.artifacts.run_manifest,
                MAX_CHILD_IDENTITY_ARTIFACT_BYTES,
                "多账户子 RunManifest",
            )
            .map_err(|error| format!("读取多账户子 RunManifest 失败: {error}"))?;
            let child_manifest = RunManifest::from_json(&child_manifest_body)
                .map_err(|error| format!("多账户子 RunManifest 无效: {error}"))?;
            if child_manifest.run_id != outcome.run_id
                || child_manifest.result_hash != outcome.result_hash
                || child_manifest.data_fingerprint != outcome.data_fingerprint
                || child_manifest.code_commit != parent_manifest.code_commit
                || child_manifest.runtime_version != parent_manifest.runtime_version
                || !child_manifest.determinism_mode
            {
                return Err("多账户子 RunManifest 与 outcome/父运行身份不一致".into());
            }
            let child_canonical_json = child_manifest
                .canonical_run_spec_json
                .as_deref()
                .ok_or("多账户子 RunManifest 缺少 CanonicalRunSpec")?;
            let child_canonical = CanonicalRunSpec::from_json(child_canonical_json)
                .map_err(|error| format!("多账户子 CanonicalRunSpec 无效: {error}"))?;
            if child_canonical.run_type != "bar_backtest"
                || child_canonical.code_revision != parent_manifest.code_commit
                || child_canonical.random_seed != child_manifest.global_seed.to_string()
                || child_canonical.datasets.len() != 1
                || child_canonical.datasets[0].content_fingerprint != outcome.data_fingerprint
                || child_canonical.strategies.is_empty()
            {
                return Err("多账户子 CanonicalRunSpec 与 child manifest 不一致".into());
            }
            let mut expected_child_behavior = behavior_run.clone();
            let expected_child_fields = expected_child_behavior
                .as_object_mut()
                .ok_or("多账户父 behavior 账户项不是对象")?;
            expected_child_fields.remove("account_run_status");
            if child_canonical.behavior != expected_child_behavior
                || child_canonical
                    .behavior
                    .get("account_id")
                    .and_then(Value::as_str)
                    != Some(account_id)
                || child_canonical
                    .behavior
                    .get("instrument")
                    .and_then(Value::as_str)
                    != Some(outcome.instrument.as_str())
            {
                return Err("多账户父/子 CanonicalRunSpec behavior 不一致".into());
            }
            for dataset in &child_canonical.datasets {
                let role = format!("bars:{account_id}");
                datasets.push(CanonicalDatasetIdentity {
                    role: role.clone(),
                    instrument: dataset.instrument.clone(),
                    content_fingerprint: dataset.content_fingerprint.clone(),
                    start_ts_ms: dataset.start_ts_ms,
                    end_ts_ms: dataset.end_ts_ms,
                });
                if dataset.instrument != outcome.instrument {
                    return Err("多账户父/子数据集标的不一致".into());
                }
            }
            clock_start = clock_start.min(child_canonical.clock.start_ts_ms);
            clock_end = clock_end.max(child_canonical.clock.end_ts_ms);
            for (instrument, fingerprint) in &child_canonical.market_rules {
                market_rules.insert(format!("{account_id}/{instrument}"), fingerprint.clone());
            }
            for child_strategy in &child_canonical.strategies {
                let scope = format!("{account_id}/{}", child_strategy.strategy_id);
                let scoped_id = qx_strategy::sha256_hex(scope.as_bytes());
                extensions.insert(format!("strategy_scope:{scoped_id}"), Value::String(scope));
                strategies.push(CanonicalStrategyIdentity {
                    strategy_id: scoped_id,
                    strategy_version: child_strategy.strategy_version.clone(),
                    source_sha256: child_strategy.source_sha256.clone(),
                    parameters: child_strategy.parameters.clone(),
                });
            }
            for (key, value) in &child_canonical.extensions {
                extensions.insert(format!("account:{account_id}:{key}"), value.clone());
            }
            for (key, value) in &child_manifest.input_components {
                input_components.insert(format!("account:{account_id}:{key}"), value.clone());
            }
            input_hasher.write_text(account_id);
            input_hasher.write_text(&child_manifest.data_fingerprint);
            extensions.insert(
                format!("account_data_fingerprint:{account_id}"),
                Value::String(child_manifest.data_fingerprint.clone()),
            );
            account_execution_models.push(serde_json::json!({
                "account_id": account_id,
                "execution_model": child_canonical.execution_model,
            }));
        } else {
            let error_json = run
                .error_json
                .as_deref()
                .ok_or("多账户失败结果缺少 error_json")?;
            let error: Value = serde_json::from_str(error_json)
                .map_err(|error| format!("多账户失败详情 JSON 无效: {error}"))?;
            if status != "failed" {
                return Err("多账户失败结果与父 behavior 状态不一致".into());
            }
            let category = error
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            extensions.insert(
                format!("account_failure_category:{account_id}"),
                Value::String(category.into()),
            );
        }
    }
    if successful == 0 {
        return Err("存在父 RunManifest 的多账户结果必须至少有一个成功账户".into());
    }
    let failed = result.runs.len() - successful;
    let successful_initial_cash_raw = weighted_returns.initial_cash_raw();
    let expected_weighted_return_bps = weighted_returns
        .finish()
        .map_err(|error| format!("多账户复核{error}"))?;
    if result.summary.accounts_total != result.runs.len()
        || result.summary.accounts_succeeded != successful
        || result.summary.accounts_failed != failed
        || result.summary.complete != (failed == 0)
        || result.summary.successful_initial_cash_raw != successful_initial_cash_raw
        || result.summary.successful_fills != successful_fills
        || result.summary.weighted_return_bps != expected_weighted_return_bps
    {
        return Err("多账户 Bar 批次结果摘要与账户 outcome 汇总不一致".into());
    }

    datasets.sort_by(|left, right| left.role.cmp(&right.role));
    let mut expected_execution_model = serde_json::json!({
        "runtime_version": parent_manifest.runtime_version,
        "matching_kernel": "multi-account-independent-bar-v1",
        "risk_model": "per-account-isolated-v1",
        "cross_account_margin_netting": false,
        "account_execution_models": account_execution_models,
    });
    normalize_cross_language_values(&mut expected_execution_model, "execution_model")
        .map_err(|error| error.to_string())?;
    let expected_clock = CanonicalRunClock {
        start_ts_ms: clock_start,
        end_ts_ms: clock_end,
        timestamp_unit: "unix_ms".into(),
        timezone: "UTC".into(),
    };
    if canonical.strategies != strategies {
        return Err("多账户父 CanonicalRunSpec strategies 与账户子运行不一致".into());
    }
    if canonical.datasets != datasets {
        return Err("多账户父 CanonicalRunSpec datasets 与账户子运行不一致".into());
    }
    if canonical.market_rules != market_rules {
        return Err("多账户父 CanonicalRunSpec market_rules 与账户子运行不一致".into());
    }
    if canonical.extensions != extensions {
        return Err("多账户父 CanonicalRunSpec extensions 与账户子运行不一致".into());
    }
    if canonical.clock != expected_clock || canonical.random_seed != "0" {
        return Err("多账户父 CanonicalRunSpec 时钟或 seed 与账户子运行不一致".into());
    }
    if canonical.execution_model != expected_execution_model {
        return Err("多账户父 CanonicalRunSpec execution_model 与账户子运行不一致".into());
    }
    if parent_manifest.data_fingerprint != format!("{:016x}", input_hasher.finish())
        || parent_manifest.input_components != input_components
        || parent_manifest.clock_start != clock_start
        || parent_manifest.clock_end != clock_end
    {
        return Err("多账户父 RunManifest 数据集合/时钟与账户子运行不一致".into());
    }
    let expected_result_hash =
        multi_account_result_hash(&canonical_fingerprint, &result.runs, &result.summary);
    if parent_manifest.result_hash != expected_result_hash {
        return Err("多账户父 RunManifest result_hash 与账户结果/汇总不一致".into());
    }
    Ok(())
}

fn verify_all_failed_result(result: &MultiAccountBacktestResult) -> Result<(), String> {
    let mut previous_id: Option<&str> = None;
    for run in &result.runs {
        if previous_id.is_some_and(|previous| previous >= run.account_id.as_str()) {
            return Err("全失败多账户结果必须按唯一 account_id 排序".into());
        }
        previous_id = Some(&run.account_id);
        if run.outcome.is_some()
            || run.error_json.as_deref().map_or(true, |payload| {
                serde_json::from_str::<Value>(payload).is_err()
            })
        {
            return Err("无父 RunManifest 的多账户结果只能包含结构化失败账户".into());
        }
    }
    if result.summary.accounts_total != result.runs.len()
        || result.summary.accounts_succeeded != 0
        || result.summary.accounts_failed != result.runs.len()
        || result.summary.successful_initial_cash_raw != 0
        || result.summary.successful_fills != 0
        || result.summary.weighted_return_bps.is_some()
        || result.summary.complete
    {
        return Err("全失败多账户结果摘要不一致".into());
    }
    Ok(())
}

pub(crate) fn legacy_spec_fingerprint(spec: &MultiAccountBacktestSpec) -> Result<String, AppError> {
    let mut canonical = spec.clone();
    canonical
        .runs
        .sort_by(|left, right| left.account_id.cmp(&right.account_id));
    for run in &mut canonical.runs {
        run.bars_path.clear();
        run.output_dir.clear();
    }
    let input = canonical.to_json()?;
    let mut hasher = Fnv1a::new();
    hasher.write_text(&input);
    Ok(format!("{:016x}", hasher.finish()))
}

fn build_multi_account_run_identity(
    spec: &MultiAccountBacktestSpec,
    runs: &[crate::spec::MultiAccountRunResult],
    code_revision: &str,
    runtime_version: &str,
) -> Result<Option<MultiAccountRunIdentity>, AppError> {
    if runs.len() != spec.runs.len() {
        return Err(invalid("多账户父 RunSpec 的成员结果数与输入账户数不一致"));
    }
    let specs = spec
        .runs
        .iter()
        .map(|run| (run.account_id.as_deref().unwrap_or_default(), run))
        .collect::<BTreeMap<_, _>>();
    let results = runs
        .iter()
        .map(|run| (run.account_id.as_str(), run))
        .collect::<BTreeMap<_, _>>();
    if specs.len() != spec.runs.len() || results.len() != runs.len() {
        return Err(invalid("多账户父 RunSpec 存在重复账户身份"));
    }

    let mut behavior = serde_json::to_value(spec)
        .map_err(|error| internal_error(format!("多账户规格转换失败: {error}")))?;
    let behavior_fields = behavior
        .as_object_mut()
        .ok_or_else(|| internal_error("多账户 behavior 不是对象"))?;
    behavior_fields.remove("batch_id");
    let behavior_runs = behavior_fields
        .get_mut("runs")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| internal_error("多账户 behavior.runs 不是数组"))?;
    behavior_runs.sort_by(|left, right| {
        left.get("account_id")
            .and_then(Value::as_str)
            .cmp(&right.get("account_id").and_then(Value::as_str))
    });

    let mut datasets = Vec::new();
    let mut strategies = Vec::new();
    let mut market_rules = BTreeMap::new();
    let mut extensions = BTreeMap::new();
    let mut input_components = BTreeMap::new();
    let mut input_hasher = Fnv1a::new();
    let mut clock_start = u64::MAX;
    let mut clock_end = 0_u64;
    let mut account_execution_models = Vec::with_capacity(runs.len());
    let mut successful_accounts = 0_usize;
    let reader = FileArtifactReader;

    for behavior_run in behavior_runs {
        let account_id = behavior_run
            .get("account_id")
            .and_then(Value::as_str)
            .ok_or_else(|| internal_error("多账户 behavior 中缺少 account_id"))?
            .to_string();
        let input_spec = specs
            .get(account_id.as_str())
            .ok_or_else(|| invalid("多账户 behavior 有不属于输入规格的账户"))?;
        let run_result = results
            .get(account_id.as_str())
            .ok_or_else(|| invalid("多账户父 RunSpec 缺少账户运行结果"))?;

        if let Some(outcome) = run_result.outcome.as_ref() {
            successful_accounts += 1;
            let child_summary_body = read_text_bounded(
                &reader,
                &outcome.artifacts.summary,
                MAX_CHILD_IDENTITY_ARTIFACT_BYTES,
                "多账户子回测摘要",
            )
            .map_err(|error| invalid(format!("读取多账户子回测摘要失败: {error}")))?;
            let child_summary: Value = serde_json::from_str(&child_summary_body)
                .map_err(|error| invalid(format!("多账户子回测摘要 JSON 无效: {error}")))?;
            verify_declared_run_manifest_with_reader(&child_summary, &reader)
                .map_err(|error| invalid(format!("多账户子回测 RunManifest 核验失败: {error}")))?;
            let child_manifest = RunManifest::from_json(
                &read_text_bounded(
                    &reader,
                    &outcome.artifacts.run_manifest,
                    MAX_CHILD_IDENTITY_ARTIFACT_BYTES,
                    "多账户子 RunManifest",
                )
                .map_err(|error| invalid(format!("读取多账户子 RunManifest 失败: {error}")))?,
            )
            .map_err(|error| invalid(format!("多账户子 RunManifest 无效: {error}")))?;
            if child_manifest.run_id != outcome.run_id
                || child_manifest.result_hash != outcome.result_hash
                || child_manifest.data_fingerprint != outcome.data_fingerprint
                || child_manifest.code_commit != code_revision
                || child_manifest.runtime_version != runtime_version
                || child_manifest.global_seed != input_spec.seed
                || !child_manifest.determinism_mode
            {
                return Err(invalid("多账户子运行身份与 outcome/父代码版本不一致"));
            }
            let child_spec_json = child_manifest
                .canonical_run_spec_json
                .as_deref()
                .ok_or_else(|| invalid("多账户子回测缺少 CanonicalRunSpec"))?;
            let child_spec = CanonicalRunSpec::from_json(child_spec_json)
                .map_err(|error| invalid(format!("多账户子 CanonicalRunSpec 无效: {error}")))?;
            let child_behavior = child_spec
                .behavior
                .as_object()
                .ok_or_else(|| internal_error("多账户子 behavior 不是对象"))?;
            if child_spec.run_type != "bar_backtest"
                || child_spec.code_revision != code_revision
                || child_spec.random_seed != input_spec.seed.to_string()
                || child_behavior.get("account_id").and_then(Value::as_str)
                    != Some(account_id.as_str())
                || child_behavior.get("instrument").and_then(Value::as_str)
                    != Some(input_spec.instrument.as_str())
                || child_spec.datasets.len() != 1
                || child_spec.datasets[0].content_fingerprint != outcome.data_fingerprint
                || child_spec.strategies.is_empty()
            {
                return Err(invalid(
                    "多账户子 CanonicalRunSpec 与输入账户/outcome 不匹配",
                ));
            }
            let mut expected_child_behavior = serde_json::to_value(input_spec)
                .map_err(|error| internal_error(format!("多账户子规格转换失败: {error}")))?;
            let expected_fields = expected_child_behavior
                .as_object_mut()
                .ok_or_else(|| internal_error("多账户子规格 behavior 不是对象"))?;
            for path_or_run_id in ["run_id", "bars_path", "output_dir"] {
                expected_fields.remove(path_or_run_id);
            }
            normalize_cross_language_values(&mut expected_child_behavior, "child_behavior")?;
            if child_spec.behavior != expected_child_behavior {
                return Err(invalid(
                    "多账户子 CanonicalRunSpec 与完整账户输入行为不一致",
                ));
            }
            *behavior_run = child_spec.behavior.clone();
            let child = behavior_run
                .as_object_mut()
                .ok_or_else(|| internal_error("多账户 Canonical behavior 不是对象"))?;
            child.insert(
                "account_run_status".into(),
                Value::String("succeeded".into()),
            );

            let dataset = &child_spec.datasets[0];
            datasets.push(CanonicalDatasetIdentity {
                role: format!("bars:{account_id}"),
                instrument: dataset.instrument.clone(),
                content_fingerprint: dataset.content_fingerprint.clone(),
                start_ts_ms: dataset.start_ts_ms,
                end_ts_ms: dataset.end_ts_ms,
            });
            clock_start = clock_start.min(child_spec.clock.start_ts_ms);
            clock_end = clock_end.max(child_spec.clock.end_ts_ms);
            for (instrument, fingerprint) in &child_spec.market_rules {
                market_rules.insert(format!("{account_id}/{instrument}"), fingerprint.clone());
            }
            for child_strategy in &child_spec.strategies {
                let scope = format!("{account_id}/{}", child_strategy.strategy_id);
                let scoped_id = qx_strategy::sha256_hex(scope.as_bytes());
                extensions.insert(format!("strategy_scope:{scoped_id}"), Value::String(scope));
                strategies.push(CanonicalStrategyIdentity {
                    strategy_id: scoped_id,
                    strategy_version: child_strategy.strategy_version.clone(),
                    source_sha256: child_strategy.source_sha256.clone(),
                    parameters: child_strategy.parameters.clone(),
                });
            }
            for (key, value) in &child_spec.extensions {
                extensions.insert(format!("account:{account_id}:{key}"), value.clone());
            }
            for (key, value) in &child_manifest.input_components {
                input_components.insert(format!("account:{account_id}:{key}"), value.clone());
            }
            input_hasher.write_text(&account_id);
            input_hasher.write_text(&child_manifest.data_fingerprint);
            extensions.insert(
                format!("account_data_fingerprint:{account_id}"),
                Value::String(child_manifest.data_fingerprint.clone()),
            );
            account_execution_models.push(serde_json::json!({
                    "account_id": account_id,
                "execution_model": child_spec.execution_model,
            }));
        } else {
            let child = behavior_run
                .as_object_mut()
                .ok_or_else(|| internal_error("多账户 behavior 成员不是对象"))?;
            for path_or_run_id in ["run_id", "bars_path", "output_dir"] {
                child.remove(path_or_run_id);
            }
            let error_json = run_result
                .error_json
                .as_deref()
                .ok_or_else(|| invalid("失败账户缺少结构化 error_json"))?;
            let error: Value = serde_json::from_str(error_json)
                .map_err(|error| invalid(format!("多账户失败详情不是 JSON: {error}")))?;
            let category = error
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            extensions.insert(
                format!("account_failure_category:{account_id}"),
                Value::String(category.into()),
            );
            child.insert("account_run_status".into(), Value::String("failed".into()));
        }
    }
    if successful_accounts == 0 {
        return Ok(None);
    }
    if datasets.is_empty() || clock_start == u64::MAX {
        return Err(internal_error(
            "多账户有成功结果但父 CanonicalRunSpec 缺数据",
        ));
    }
    normalize_cross_language_values(&mut behavior, "behavior")?;
    let mut execution_model = serde_json::json!({
        "runtime_version": runtime_version,
        "matching_kernel": "multi-account-independent-bar-v1",
        "risk_model": "per-account-isolated-v1",
        "cross_account_margin_netting": false,
        "account_execution_models": account_execution_models,
    });
    normalize_cross_language_values(&mut execution_model, "execution_model")?;
    let canonical_spec = CanonicalRunSpec::new(
        "multi_account_backtest",
        code_revision,
        strategies,
        datasets,
        market_rules,
        CanonicalRunClock {
            start_ts_ms: clock_start,
            end_ts_ms: clock_end,
            timestamp_unit: "unix_ms".into(),
            timezone: "UTC".into(),
        },
        0,
        behavior,
        execution_model,
        extensions,
    )
    .map_err(|error| invalid(format!("多账户 CanonicalRunSpec 无效: {error}")))?;
    Ok(Some(MultiAccountRunIdentity {
        canonical_spec,
        data_fingerprint: format!("{:016x}", input_hasher.finish()),
        input_components,
        clock_start,
        clock_end,
    }))
}

fn multi_account_result_hash(
    canonical_fingerprint: &str,
    runs: &[crate::spec::MultiAccountRunResult],
    summary: &MultiAccountBacktestSummary,
) -> String {
    let mut hasher = Fnv1a::new();
    hasher.write_text(canonical_fingerprint);
    for run in runs {
        hasher.write_text(&run.account_id);
        if let Some(outcome) = &run.outcome {
            hasher.write_text("succeeded");
            hasher.write_text(&outcome.result_hash);
        } else {
            hasher.write_text("failed");
        }
    }
    for value in [
        summary.successful_initial_cash_raw,
        summary.successful_fills as i128,
        i128::from(summary.weighted_return_bps.unwrap_or_default()),
        summary.accounts_total as i128,
        summary.accounts_succeeded as i128,
        summary.accounts_failed as i128,
        i128::from(summary.complete),
    ] {
        hasher.write_text(&value.to_string());
    }
    format!("{:016x}", hasher.finish())
}

pub(crate) fn normalize_cross_language_values(
    value: &mut Value,
    path: &str,
) -> Result<(), AppError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(number) if number.is_f64() => Err(invalid(format!(
            "{path} 含浮点值；CanonicalRunSpec 的精确数值必须使用十进制文本"
        ))),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                if integer.unsigned_abs() > 9_007_199_254_740_991 {
                    return Err(invalid(format!(
                        "{path} 含跨语言不安全整数；请在 RunSpec 中用十进制文本表示"
                    )));
                }
            } else if number
                .as_u64()
                .is_some_and(|integer| integer > 9_007_199_254_740_991)
            {
                return Err(invalid(format!(
                    "{path} 含跨语言不安全整数；请在 RunSpec 中用十进制文本表示"
                )));
            }
            Ok(())
        }
        Value::Array(values) => {
            for (index, child) in values.iter_mut().enumerate() {
                normalize_cross_language_values(child, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            for (name, child) in fields.iter_mut() {
                if name.ends_with("_raw") || name == "seed" {
                    if child.is_number() {
                        *child = Value::String(child.to_string());
                        continue;
                    }
                }
                normalize_cross_language_values(child, &format!("{path}.{name}"))?;
            }
            Ok(())
        }
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message.into())
}

fn internal_error(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message.into())
}
