//! Canonical parent identity for independently funded Tick/OrderBook account batches.

use super::DEPTH_RUNTIME_VERSION;
use crate::artifact_store::{
    read_text_bounded, verify_declared_run_manifest_with_reader, ArtifactReader, FileArtifactReader,
};
use crate::error::{AppError, AppErrorCategory};
use crate::spec::{
    MultiAccountDepthBacktestResult, MultiAccountDepthBacktestSpec,
    MultiAccountDepthBacktestSummary,
};
use qx_core::{Fnv1a, RunManifest};
use qx_spec::{
    CanonicalDatasetIdentity, CanonicalRunClock, CanonicalRunSpec, CanonicalStrategyIdentity,
};
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_CHILD_IDENTITY_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;

struct MultiAccountDepthRunIdentity {
    canonical_spec: CanonicalRunSpec,
    data_fingerprint: String,
    input_components: BTreeMap<String, String>,
    clock_start: u64,
    clock_end: u64,
}

pub(crate) fn build_multi_account_depth_run_manifest(
    spec: &MultiAccountDepthBacktestSpec,
    result: &MultiAccountDepthBacktestResult,
    code_revision: &str,
    runtime_version: &str,
) -> Result<Option<RunManifest>, AppError> {
    let Some(identity) = build_identity(
        spec,
        &result.runs,
        code_revision,
        runtime_version,
        &FileArtifactReader,
    )?
    else {
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
    let result_hash = depth_result_hash(&canonical_spec_fingerprint, &result.runs, &result.summary);
    let manifest = RunManifest {
        run_id: spec.batch_id.clone(),
        code_commit: code_revision.to_string(),
        config_hash: result.spec_fingerprint.clone(),
        data_fingerprint: identity.data_fingerprint.clone(),
        input_components: identity.input_components,
        clock_start: identity.clock_start,
        clock_end: identity.clock_end,
        global_seed: 0,
        determinism_mode: true,
        result_hash: result_hash.clone(),
        strategy_version: format!("multi-account-depth-batch:{canonical_spec_fingerprint}"),
        instrument_spec_version: "multi-account-depth-batch-v1".into(),
        model_fingerprint: qx_strategy::sha256_hex(b"MultiAccountDepthBacktestBatch/v1"),
        input_event_hash: identity.data_fingerprint.clone(),
        output_event_hash: result_hash,
        runtime_version: runtime_version.to_string(),
        canonical_run_spec_json: Some(canonical_spec_json),
        canonical_run_spec_fingerprint: Some(canonical_spec_fingerprint),
    };
    manifest.validate().map_err(internal_error)?;
    Ok(Some(manifest))
}

pub(crate) fn verify_multi_account_depth_backtest_result(
    result: &MultiAccountDepthBacktestResult,
    reader: &dyn ArtifactReader,
) -> Result<(), String> {
    if result.schema_version != crate::spec::MULTI_ACCOUNT_DEPTH_BACKTEST_RESULT_SCHEMA_VERSION {
        return Err(format!(
            "多账户 Depth verifier 只接受结果 v{}",
            crate::spec::MULTI_ACCOUNT_DEPTH_BACKTEST_RESULT_SCHEMA_VERSION
        ));
    }
    if result.batch_id.trim().is_empty()
        || result.result_kind != "multi_account_depth"
        || result.runs.len() < 2
        || result.runs.len() > 32
        || result.settlement_currency.trim().is_empty()
        || result.spec_fingerprint.len() != 16
        || !result
            .spec_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("多账户 Depth 批次身份或账户数量无效".into());
    }
    if result
        .runs
        .windows(2)
        .any(|pair| pair[0].account_id >= pair[1].account_id)
    {
        return Err("多账户 Depth 结果账户必须唯一且按 account_id 排序".into());
    }
    let Some(parent_manifest) = result.run_manifest.as_ref() else {
        return verify_all_failed_result(result);
    };
    parent_manifest.validate()?;
    let canonical_json = parent_manifest
        .canonical_run_spec_json
        .as_deref()
        .ok_or("多账户 Depth 父 RunManifest 缺少 CanonicalRunSpec")?;
    let canonical = CanonicalRunSpec::from_json(canonical_json)
        .map_err(|error| format!("多账户 Depth 父 CanonicalRunSpec 无效: {error}"))?;
    if canonical
        .canonical_json()
        .map_err(|error| error.to_string())?
        != canonical_json
    {
        return Err("多账户 Depth 父 CanonicalRunSpec 不是规范 JSON".into());
    }
    let fingerprint = canonical.fingerprint().map_err(|error| error.to_string())?;
    if canonical.run_type != "multi_account_depth_backtest"
        || canonical.code_revision != parent_manifest.code_commit
        || parent_manifest.canonical_run_spec_fingerprint.as_deref() != Some(fingerprint.as_str())
        || parent_manifest.run_id != result.batch_id
        || parent_manifest.config_hash != result.spec_fingerprint
        || parent_manifest.global_seed != 0
        || !parent_manifest.determinism_mode
        || parent_manifest.runtime_version != DEPTH_RUNTIME_VERSION
        || parent_manifest.instrument_spec_version != "multi-account-depth-batch-v1"
        || parent_manifest.model_fingerprint
            != qx_strategy::sha256_hex(b"MultiAccountDepthBacktestBatch/v1")
        || parent_manifest.strategy_version != format!("multi-account-depth-batch:{fingerprint}")
        || parent_manifest.input_event_hash != parent_manifest.data_fingerprint
        || parent_manifest.output_event_hash != parent_manifest.result_hash
    {
        return Err("多账户 Depth 父 RunManifest 与批次身份字段不一致".into());
    }
    let behavior = canonical
        .behavior
        .as_object()
        .ok_or("多账户 Depth 父 CanonicalRunSpec behavior 不是对象")?;
    let behavior_runs = behavior
        .get("runs")
        .and_then(Value::as_array)
        .ok_or("多账户 Depth 父 CanonicalRunSpec 缺少 behavior.runs")?;
    if behavior_runs.len() != result.runs.len() {
        return Err("多账户 Depth 父 CanonicalRunSpec 与结果账户数量不一致".into());
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
        let fields = behavior_run
            .as_object()
            .ok_or("多账户 Depth 父 behavior 账户项不是对象")?;
        let account_id = fields
            .get("account_id")
            .and_then(Value::as_str)
            .ok_or("多账户 Depth 父 behavior 账户项缺少 account_id")?;
        if account_id != run.account_id
            || fields.get("settlement_currency").and_then(Value::as_str)
                != Some(result.settlement_currency.as_str())
        {
            return Err("多账户 Depth 父 behavior 账户顺序或结算币与结果不一致".into());
        }
        let status = fields
            .get("account_run_status")
            .and_then(Value::as_str)
            .ok_or("多账户 Depth 父 behavior 缺少 account_run_status")?;
        if let Some(outcome) = run.outcome.as_ref() {
            successful += 1;
            if status != "succeeded"
                || run.error_json.is_some()
                || outcome
                    .account_id
                    .as_deref()
                    .is_some_and(|id| id != account_id)
            {
                return Err("多账户 Depth 成功 outcome 状态或账户身份不一致".into());
            }
            let initial_cash_raw = fields
                .get("initial_cash_raw")
                .and_then(Value::as_str)
                .ok_or("多账户 Depth 父 behavior 缺少十进制 initial_cash_raw")?
                .parse::<i128>()
                .map_err(|error| format!("多账户 Depth initial_cash_raw 无效: {error}"))?;
            if initial_cash_raw <= 0 {
                return Err("多账户 Depth 父 behavior 的 initial_cash_raw 必须为正数".into());
            }
            successful_fills = successful_fills
                .checked_add(outcome.fills)
                .ok_or("多账户 Depth 复核成交数求和溢出")?;
            weighted_returns
                .push(outcome.return_bps, initial_cash_raw)
                .map_err(|error| format!("多账户 Depth 复核{error}"))?;
            let summary_body = read_text_bounded(
                reader,
                &outcome.artifacts.summary,
                MAX_CHILD_IDENTITY_ARTIFACT_BYTES,
                "多账户 Depth 子回测摘要",
            )
            .map_err(|error| format!("读取多账户 Depth 子摘要失败: {error}"))?;
            let summary: Value = serde_json::from_str(&summary_body)
                .map_err(|error| format!("多账户 Depth 子摘要 JSON 无效: {error}"))?;
            verify_declared_run_manifest_with_reader(&summary, reader)
                .map_err(|error| format!("多账户 Depth 子 RunManifest 核验失败: {error}"))?;
            let manifest_body = read_text_bounded(
                reader,
                &outcome.artifacts.run_manifest,
                MAX_CHILD_IDENTITY_ARTIFACT_BYTES,
                "多账户 Depth 子 RunManifest",
            )
            .map_err(|error| format!("读取多账户 Depth 子 RunManifest 失败: {error}"))?;
            let child_manifest = RunManifest::from_json(&manifest_body)
                .map_err(|error| format!("多账户 Depth 子 RunManifest 无效: {error}"))?;
            if child_manifest.run_id != outcome.run_id
                || child_manifest.result_hash != outcome.result_hash
                || child_manifest.data_fingerprint != outcome.data_fingerprint
                || child_manifest.code_commit != parent_manifest.code_commit
                || child_manifest.runtime_version != parent_manifest.runtime_version
                || child_manifest.global_seed != 0
                || !child_manifest.determinism_mode
            {
                return Err("多账户 Depth 子 RunManifest 与 outcome/父身份不一致".into());
            }
            let child_json = child_manifest
                .canonical_run_spec_json
                .as_deref()
                .ok_or("多账户 Depth 子 RunManifest 缺少 CanonicalRunSpec")?;
            let child_spec = CanonicalRunSpec::from_json(child_json)
                .map_err(|error| format!("多账户 Depth 子 CanonicalRunSpec 无效: {error}"))?;
            if child_spec.run_type != "depth_backtest"
                || child_spec.code_revision != parent_manifest.code_commit
                || child_spec.random_seed != "0"
                || child_spec.datasets.len() != 1
                || child_spec.datasets[0].instrument != outcome.instrument
                || child_spec.strategies.is_empty()
                || child_spec.behavior != without_status(behavior_run)?
            {
                return Err(
                    "多账户 Depth 子 CanonicalRunSpec 与 outcome/父 behavior 不一致".into(),
                );
            }
            for dataset in &child_spec.datasets {
                datasets.push(CanonicalDatasetIdentity {
                    role: format!("depth:{account_id}"),
                    instrument: dataset.instrument.clone(),
                    content_fingerprint: dataset.content_fingerprint.clone(),
                    start_ts_ms: dataset.start_ts_ms,
                    end_ts_ms: dataset.end_ts_ms,
                });
            }
            clock_start = clock_start.min(child_spec.clock.start_ts_ms);
            clock_end = clock_end.max(child_spec.clock.end_ts_ms);
            for (instrument, rule) in &child_spec.market_rules {
                market_rules.insert(format!("{account_id}/{instrument}"), rule.clone());
            }
            for strategy in &child_spec.strategies {
                let scope = format!("{account_id}/{}", strategy.strategy_id);
                let scoped_id = qx_strategy::sha256_hex(scope.as_bytes());
                extensions.insert(format!("strategy_scope:{scoped_id}"), Value::String(scope));
                strategies.push(CanonicalStrategyIdentity {
                    strategy_id: scoped_id,
                    strategy_version: strategy.strategy_version.clone(),
                    source_sha256: strategy.source_sha256.clone(),
                    parameters: strategy.parameters.clone(),
                });
            }
            for (key, value) in &child_spec.extensions {
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
                "execution_model": child_spec.execution_model,
            }));
        } else {
            if status != "failed" {
                return Err("多账户 Depth 失败结果与父 behavior 状态不一致".into());
            }
            let error_json = run
                .error_json
                .as_deref()
                .ok_or("多账户 Depth 失败结果缺少 error_json")?;
            let error: Value = serde_json::from_str(error_json)
                .map_err(|error| format!("多账户 Depth 失败详情 JSON 无效: {error}"))?;
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
        return Err("存在父 RunManifest 的多账户 Depth 结果必须至少有一个成功账户".into());
    }
    let failed = result.runs.len() - successful;
    let successful_initial_cash_raw = weighted_returns.initial_cash_raw();
    let weighted_return_bps = Some(
        weighted_returns
            .finish()
            .map_err(|error| format!("多账户 Depth 复核{error}"))?
            .ok_or("多账户 Depth 复核缺少成功账户加权收益")?,
    );
    if result.summary.accounts_total != result.runs.len()
        || result.summary.accounts_succeeded != successful
        || result.summary.accounts_failed != failed
        || result.summary.complete != (failed == 0)
        || result.summary.successful_initial_cash_raw != successful_initial_cash_raw
        || result.summary.successful_fills != successful_fills
        || result.summary.weighted_return_bps != weighted_return_bps
    {
        return Err("多账户 Depth 批次结果摘要与账户 outcome 汇总不一致".into());
    }
    datasets.sort_by(|left, right| left.role.cmp(&right.role));
    let mut expected_execution_model = serde_json::json!({
        "runtime_version": parent_manifest.runtime_version,
        "matching_kernel": "multi-account-independent-depth-v1",
        "risk_model": "per-account-isolated-v1",
        "cross_account_margin_netting": false,
        "account_execution_models": account_execution_models,
    });
    super::canonical_spec::normalize_cross_language_values(
        &mut expected_execution_model,
        "execution_model",
    )
    .map_err(|error| error.message().to_string())?;
    let expected_clock = qx_spec::CanonicalRunClock {
        start_ts_ms: clock_start,
        end_ts_ms: clock_end,
        timestamp_unit: "unix_ms".into(),
        timezone: "UTC".into(),
    };
    if canonical.strategies != strategies
        || canonical.datasets != datasets
        || canonical.market_rules != market_rules
        || canonical.extensions != extensions
        || canonical.clock != expected_clock
        || canonical.random_seed != "0"
        || canonical.execution_model != expected_execution_model
    {
        return Err("多账户 Depth 父 CanonicalRunSpec 与账户子运行身份不一致".into());
    }
    if parent_manifest.data_fingerprint != format!("{:016x}", input_hasher.finish())
        || parent_manifest.input_components != input_components
        || parent_manifest.clock_start != clock_start
        || parent_manifest.clock_end != clock_end
    {
        return Err("多账户 Depth 父 RunManifest 数据集合/时钟与子运行不一致".into());
    }
    if parent_manifest.result_hash != depth_result_hash(&fingerprint, &result.runs, &result.summary)
    {
        return Err("多账户 Depth 父 RunManifest result_hash 与结果/汇总不一致".into());
    }
    Ok(())
}

fn without_status(value: &Value) -> Result<Value, String> {
    let mut value = value.clone();
    value
        .as_object_mut()
        .ok_or("多账户 Depth 父 behavior 账户项不是对象")?
        .remove("account_run_status");
    Ok(value)
}

fn verify_all_failed_result(result: &MultiAccountDepthBacktestResult) -> Result<(), String> {
    for run in &result.runs {
        if run.outcome.is_some()
            || run.error_json.as_deref().map_or(true, |payload| {
                serde_json::from_str::<Value>(payload).is_err()
            })
        {
            return Err("无父 RunManifest 的多账户 Depth 结果只能包含结构化失败账户".into());
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
        return Err("全失败多账户 Depth 结果摘要不一致".into());
    }
    Ok(())
}

fn build_identity(
    spec: &MultiAccountDepthBacktestSpec,
    runs: &[crate::spec::MultiAccountDepthRunResult],
    code_revision: &str,
    runtime_version: &str,
    reader: &dyn ArtifactReader,
) -> Result<Option<MultiAccountDepthRunIdentity>, AppError> {
    if runs.len() != spec.runs.len() {
        return Err(invalid("多账户深度父 RunSpec 成员结果数与输入账户数不一致"));
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
        return Err(invalid("多账户深度父 RunSpec 存在重复账户身份"));
    }

    let mut behavior = serde_json::to_value(spec)
        .map_err(|error| internal_error(format!("多账户深度规格转换失败: {error}")))?;
    let behavior_fields = behavior
        .as_object_mut()
        .ok_or_else(|| internal_error("多账户深度 behavior 不是对象"))?;
    behavior_fields.remove("batch_id");
    let behavior_runs = behavior_fields
        .get_mut("runs")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| internal_error("多账户深度 behavior.runs 不是数组"))?;
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

    for behavior_run in behavior_runs {
        let account_id = behavior_run
            .get("account_id")
            .and_then(Value::as_str)
            .ok_or_else(|| internal_error("多账户深度 behavior 中缺少 account_id"))?
            .to_string();
        let input_spec = specs
            .get(account_id.as_str())
            .ok_or_else(|| invalid("多账户深度 behavior 有不属于输入规格的账户"))?;
        let run_result = results
            .get(account_id.as_str())
            .ok_or_else(|| invalid("多账户深度父 RunSpec 缺少账户运行结果"))?;

        if let Some(outcome) = run_result.outcome.as_ref() {
            successful_accounts += 1;
            let summary_body = read_text_bounded(
                reader,
                &outcome.artifacts.summary,
                MAX_CHILD_IDENTITY_ARTIFACT_BYTES,
                "多账户深度子回测摘要",
            )?;
            let summary: Value = serde_json::from_str(&summary_body)
                .map_err(|error| invalid(format!("多账户深度子摘要 JSON 无效: {error}")))?;
            verify_declared_run_manifest_with_reader(&summary, reader)
                .map_err(|error| invalid(format!("多账户深度子 RunManifest 核验失败: {error}")))?;
            let manifest_body = read_text_bounded(
                reader,
                &outcome.artifacts.run_manifest,
                MAX_CHILD_IDENTITY_ARTIFACT_BYTES,
                "多账户深度子 RunManifest",
            )?;
            let child_manifest = RunManifest::from_json(&manifest_body)
                .map_err(|error| invalid(format!("多账户深度子 RunManifest 无效: {error}")))?;
            if child_manifest.run_id != outcome.run_id
                || child_manifest.result_hash != outcome.result_hash
                || child_manifest.code_commit != code_revision
                || child_manifest.runtime_version != runtime_version
                || child_manifest.global_seed != 0
                || !child_manifest.determinism_mode
            {
                return Err(invalid("多账户深度子运行身份与 outcome/父代码版本不一致"));
            }
            let child_spec_json = child_manifest
                .canonical_run_spec_json
                .as_deref()
                .ok_or_else(|| invalid("多账户深度子运行缺少 CanonicalRunSpec"))?;
            let child_spec = CanonicalRunSpec::from_json(child_spec_json)
                .map_err(|error| invalid(format!("多账户深度子 CanonicalRunSpec 无效: {error}")))?;
            if child_spec.run_type != "depth_backtest"
                || child_spec.code_revision != code_revision
                || child_spec.random_seed != "0"
                || child_spec.datasets.len() != 1
                || child_spec.datasets[0].instrument != outcome.instrument
                || child_spec.strategies.is_empty()
            {
                return Err(invalid("多账户深度子 CanonicalRunSpec 与结果不匹配"));
            }
            let mut expected_child_behavior = serde_json::to_value(input_spec)
                .map_err(|error| internal_error(format!("多账户深度子规格转换失败: {error}")))?;
            let expected_fields = expected_child_behavior
                .as_object_mut()
                .ok_or_else(|| internal_error("多账户深度子 behavior 不是对象"))?;
            for locator_or_provenance in [
                "run_id",
                "manifest_run_id",
                "depth_path",
                "output_dir",
                "quality_manifest_path",
                "runtime_config_path",
                "market_spec_path",
                "market_spec_source",
                "execution_cost_source",
                "account_base_source",
                "signal_source",
                "signal_declared_unused",
                "declared_ashare_rules",
            ] {
                expected_fields.remove(locator_or_provenance);
            }
            super::canonical_spec::normalize_cross_language_values(
                &mut expected_child_behavior,
                "child_behavior",
            )?;
            if child_spec.behavior != expected_child_behavior
                || child_spec
                    .behavior
                    .get("account_id")
                    .and_then(Value::as_str)
                    != Some(account_id.as_str())
            {
                return Err(invalid(
                    "多账户深度子 CanonicalRunSpec 与完整账户输入不一致",
                ));
            }

            *behavior_run = child_spec.behavior.clone();
            let child = behavior_run
                .as_object_mut()
                .ok_or_else(|| internal_error("多账户深度 Canonical behavior 不是对象"))?;
            child.insert(
                "account_run_status".into(),
                Value::String("succeeded".into()),
            );

            for dataset in &child_spec.datasets {
                datasets.push(CanonicalDatasetIdentity {
                    role: format!("depth:{account_id}"),
                    instrument: dataset.instrument.clone(),
                    content_fingerprint: dataset.content_fingerprint.clone(),
                    start_ts_ms: dataset.start_ts_ms,
                    end_ts_ms: dataset.end_ts_ms,
                });
            }
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
                .ok_or_else(|| internal_error("多账户深度 behavior 账户项不是对象"))?;
            for locator_or_provenance in [
                "run_id",
                "manifest_run_id",
                "depth_path",
                "output_dir",
                "quality_manifest_path",
                "runtime_config_path",
                "market_spec_path",
                "market_spec_source",
                "execution_cost_source",
                "account_base_source",
                "signal_source",
                "signal_declared_unused",
                "declared_ashare_rules",
            ] {
                child.remove(locator_or_provenance);
            }
            let error_json = run_result
                .error_json
                .as_deref()
                .ok_or_else(|| invalid("多账户深度失败账户缺少结构化 error_json"))?;
            let error: Value = serde_json::from_str(error_json)
                .map_err(|error| invalid(format!("多账户深度失败详情 JSON 无效: {error}")))?;
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
    super::canonical_spec::normalize_cross_language_values(&mut behavior, "behavior")?;
    let mut execution_model = serde_json::json!({
        "runtime_version": runtime_version,
        "matching_kernel": "multi-account-independent-depth-v1",
        "risk_model": "per-account-isolated-v1",
        "cross_account_margin_netting": false,
        "account_execution_models": account_execution_models,
    });
    super::canonical_spec::normalize_cross_language_values(
        &mut execution_model,
        "execution_model",
    )?;
    let canonical_spec = CanonicalRunSpec::new(
        "multi_account_depth_backtest",
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
    .map_err(|error| invalid(format!("多账户深度 CanonicalRunSpec 无效: {error}")))?;
    Ok(Some(MultiAccountDepthRunIdentity {
        canonical_spec,
        data_fingerprint: format!("{:016x}", input_hasher.finish()),
        input_components,
        clock_start,
        clock_end,
    }))
}

fn depth_result_hash(
    canonical_fingerprint: &str,
    runs: &[crate::spec::MultiAccountDepthRunResult],
    summary: &MultiAccountDepthBacktestSummary,
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

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn internal_error(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}
