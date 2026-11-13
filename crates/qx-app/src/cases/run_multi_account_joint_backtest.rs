//! Account-isolated exchange portfolios with synchronized aggregate reporting.

use super::artifacts::{read_text, write_text};
use super::attach;
use super::guard::guard_panics;
use super::run_multi_market_backtest::run_inner as run_market_inner;
use crate::{
    AppError, AppErrorCategory, CallerCapability, JointAccountBacktestResult,
    MultiAccountJointBacktestOutcome, MultiAccountJointBacktestSpec, MultiMarketBacktestSpec,
    RunContext,
};
use qx_core::Fnv1a;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

mod canonical_spec;
pub(crate) use canonical_spec::{
    build_joint_run_identity, joint_result_hash, JointAccountRunVerification,
};

pub fn run_multi_account_joint_backtest(
    spec: &MultiAccountJointBacktestSpec,
    context: &RunContext,
) -> Result<MultiAccountJointBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "多交易所多账户联合 Bar 回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || run_inner(spec, context, None))
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| {
            attach(
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "同步联合账户回测意外返回取消状态",
                ),
                &correlation_id,
            )
        })
}

impl MultiAccountJointBacktestSpec {
    pub fn start(self, context: RunContext) -> crate::RunHandle<MultiAccountJointBacktestOutcome> {
        let run_id = self.run_id.clone();
        crate::RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "多交易所多账户联合 Bar 回测")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(&self, &context, Some(&cancellation))
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }
}

fn run_inner(
    spec: &MultiAccountJointBacktestSpec,
    context: &RunContext,
    cancellation: Option<&crate::run_handle::CancellationToken>,
) -> Result<Option<MultiAccountJointBacktestOutcome>, AppError> {
    if cancellation.is_some_and(|token| token.is_cancelled()) {
        return Ok(None);
    }
    spec.validate()?;

    let mut canonical_accounts = spec.accounts.clone();
    canonical_accounts.sort_by(|left, right| {
        left.exchange_id
            .cmp(&right.exchange_id)
            .then_with(|| left.account_id.cmp(&right.account_id))
    });
    let mut canonical = spec.clone();
    canonical.accounts = canonical_accounts.clone();
    canonical.output_dir.clear();
    for account in &mut canonical.accounts {
        for market in &mut account.markets {
            market.bars_path.clear();
            market.dataset_manifest_path = None;
        }
    }
    let canonical_json = canonical.to_json()?;
    let mut spec_hasher = Fnv1a::new();
    spec_hasher.write_text(&canonical_json);
    let spec_fingerprint = format!("{:016x}", spec_hasher.finish());

    let mut accounts = Vec::with_capacity(canonical_accounts.len());
    let mut curves = BTreeMap::<(String, String), (i128, Vec<(u64, i128)>)>::new();
    let mut fills = Vec::<Value>::new();
    let mut total_cash_raw = 0_i128;
    let mut markets_total = 0_usize;
    let mut successful_fills = 0_u64;
    let mut child_run_verifications = Vec::with_capacity(canonical_accounts.len());
    for (index, account) in canonical_accounts.iter().enumerate() {
        if cancellation.is_some_and(|token| token.is_cancelled()) {
            return Ok(None);
        }
        let child_run_id =
            joint_child_run_id(&spec.run_id, &account.exchange_id, &account.account_id);
        let child_spec = MultiMarketBacktestSpec {
            schema_version: crate::MULTI_MARKET_BACKTEST_SCHEMA_VERSION,
            run_id: child_run_id.clone(),
            account_id: account.account_id.clone(),
            settlement_currency: spec.settlement_currency.clone(),
            initial_cash_raw: account.initial_cash_raw,
            initial_cash_exact: None,
            seed: spec.seed,
            output_dir: spec.output_dir.clone(),
            markets: account.markets.clone(),
        };
        let member_context = RunContext::new(
            context.capability(),
            format!("{}-account-{index}", context.correlation_id()),
        )
        .with_code_commit(context.code_commit());
        let child_outcome =
            run_market_inner(&child_spec, &member_context, cancellation, BTreeMap::new())?
                .ok_or_else(|| invalid("联合账户成员回测被取消"))?;

        let child_artifact: Value = serde_json::from_str(&read_text(&child_outcome.artifact_path)?)
            .map_err(|error| invalid(format!("读取联合账户子运行产物失败: {error}")))?;
        let child_manifest_path = child_artifact
            .get("run_manifest_path")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("联合账户子运行产物缺少 RunManifest 路径"))?;
        let child_manifest = qx_core::RunManifest::from_json(&read_text(child_manifest_path)?)
            .map_err(|error| invalid(format!("联合账户子运行 RunManifest 无效: {error}")))?;
        crate::artifact_store::verify_canonical_run_spec_for_manifest(
            &child_manifest,
            &child_artifact,
        )
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("联合账户子运行 CanonicalRunSpec 复核失败: {error}"),
            )
        })?;
        let identity = serde_json::json!({
            "market_data": child_artifact.get("market_data").cloned().ok_or_else(|| invalid("联合账户子运行产物缺少 market_data"))?,
            "dataset_quality": child_artifact.get("dataset_quality").cloned().ok_or_else(|| invalid("联合账户子运行产物缺少 dataset_quality"))?,
            "strategy_identities": child_artifact.get("strategy_identities").cloned().ok_or_else(|| invalid("联合账户子运行产物缺少 strategy_identities"))?,
            "outcome": child_artifact.get("outcome").cloned().ok_or_else(|| invalid("联合账户子运行产物缺少 outcome"))?,
        });
        child_run_verifications.push(JointAccountRunVerification {
            exchange_id: account.exchange_id.clone(),
            account_id: account.account_id.clone(),
            run_manifest: child_manifest,
            identity,
        });

        let root = PathBuf::from(&spec.output_dir);
        let curve_path = root.join(format!("{child_run_id}.multi-market.equity.json"));
        let fills_path = root.join(format!("{child_run_id}.multi-market.fills.json"));
        let curve: Vec<(u64, i128)> =
            serde_json::from_str(&read_text(&curve_path.to_string_lossy())?).map_err(|error| {
                invalid(format!(
                    "联合账户 {} 权益曲线格式非法: {error}",
                    account.account_id
                ))
            })?;
        let account_fills: Vec<Value> =
            serde_json::from_str(&read_text(&fills_path.to_string_lossy())?).map_err(|error| {
                invalid(format!(
                    "联合账户 {} 成交明细格式非法: {error}",
                    account.account_id
                ))
            })?;
        if curve.is_empty() {
            return Err(invalid(format!(
                "联合账户 {} 没有权益曲线",
                account.account_id
            )));
        }
        total_cash_raw = total_cash_raw
            .checked_add(account.initial_cash_raw)
            .ok_or_else(|| invalid("联合账户初始资金合计溢出"))?;
        markets_total = markets_total
            .checked_add(account.markets.len())
            .ok_or_else(|| invalid("联合账户市场数溢出"))?;
        successful_fills = successful_fills
            .checked_add(child_outcome.successful_fills)
            .ok_or_else(|| invalid("联合账户成交数溢出"))?;
        fills.extend(account_fills);
        curves.insert(
            (account.exchange_id.clone(), account.account_id.clone()),
            (account.initial_cash_raw, curve),
        );
        accounts.push(JointAccountBacktestResult {
            exchange_id: account.exchange_id.clone(),
            account_id: account.account_id.clone(),
            initial_cash_raw: account.initial_cash_raw,
            outcome: child_outcome,
        });
    }

    if cancellation.is_some_and(|token| token.is_cancelled()) {
        return Ok(None);
    }
    let mut timestamps = curves
        .values()
        .flat_map(|(_, curve)| curve.iter().map(|(ts, _)| *ts))
        .collect::<Vec<_>>();
    timestamps.sort_unstable();
    timestamps.dedup();
    let mut combined_equity = Vec::with_capacity(timestamps.len());
    for ts in timestamps {
        let mut total = 0_i128;
        for (initial, curve) in curves.values() {
            let end = curve.partition_point(|(point_ts, _)| *point_ts <= ts);
            let value = end
                .checked_sub(1)
                .and_then(|index| curve.get(index))
                .map(|(_, equity)| *equity)
                .unwrap_or(*initial);
            total = total
                .checked_add(value)
                .ok_or_else(|| invalid("联合权益合计溢出"))?;
        }
        combined_equity.push((ts, total));
    }
    let final_equity_raw = combined_equity
        .last()
        .map(|(_, value)| *value)
        .ok_or_else(|| invalid("联合权益曲线为空"))?;
    let return_bps = final_equity_raw
        .checked_sub(total_cash_raw)
        .and_then(|pnl| pnl.checked_mul(10_000))
        .and_then(|value| value.checked_div(total_cash_raw))
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid("联合账户收益无法表示"))?;
    let max_drawdown_bps =
        crate::report_analysis::joint_equity_max_drawdown_bps_v1(&combined_equity)
            .map_err(invalid)?;
    let runtime_version = format!("qx-app-{}", env!("CARGO_PKG_VERSION"));
    let joint_identity = build_joint_run_identity(
        spec,
        &child_run_verifications,
        context.code_commit(),
        &runtime_version,
    )?;
    let canonical_spec_json = joint_identity
        .canonical_spec
        .canonical_json()
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error))?;
    let canonical_spec_fingerprint = joint_identity
        .canonical_spec
        .fingerprint()
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error))?;
    let result_hash = joint_result_hash(
        &canonical_spec_fingerprint,
        &accounts,
        total_cash_raw,
        final_equity_raw,
        return_bps,
        max_drawdown_bps,
        successful_fills,
        markets_total,
    );

    std::fs::create_dir_all(&spec.output_dir)
        .map_err(|error| AppError::from_io("创建联合账户回测产物目录", &error))?;
    let output_root = PathBuf::from(&spec.output_dir)
        .canonicalize()
        .map_err(|error| AppError::from_io("解析联合账户产物目录", &error))?;
    let equity_path = output_root.join(format!("{}.joint-equity.json", spec.run_id));
    let fills_path = output_root.join(format!("{}.joint-fills.json", spec.run_id));
    let artifact_path = output_root.join(format!("{}.joint-portfolio.json", spec.run_id));
    let run_manifest_path = output_root.join(format!("{}.joint.run.json", spec.run_id));
    let run_record_path = output_root.join(format!("{}.joint.record.json", spec.run_id));
    let equity_json = serde_json::to_string(&combined_equity)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let fills_json = serde_json::to_string(&fills)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let artifact_locations = BTreeMap::from([
        (
            "joint_portfolio".to_string(),
            artifact_path.to_string_lossy().into_owned(),
        ),
        (
            "joint_equity".to_string(),
            equity_path.to_string_lossy().into_owned(),
        ),
        (
            "joint_fills".to_string(),
            fills_path.to_string_lossy().into_owned(),
        ),
        (
            "run_manifest".to_string(),
            run_manifest_path.to_string_lossy().into_owned(),
        ),
    ]);
    let outcome = MultiAccountJointBacktestOutcome {
        run_id: spec.run_id.clone(),
        settlement_currency: spec.settlement_currency.clone(),
        spec_fingerprint: spec_fingerprint.clone(),
        accounts_total: accounts.len(),
        markets_total,
        initial_cash_raw: total_cash_raw,
        final_equity_raw,
        return_bps,
        max_drawdown_bps,
        successful_fills,
        equity_path: equity_path.to_string_lossy().into_owned(),
        fills_path: fills_path.to_string_lossy().into_owned(),
        artifact_path: artifact_path.to_string_lossy().into_owned(),
        run_manifest_path: Some(run_manifest_path.to_string_lossy().into_owned()),
        result_hash: Some(result_hash.clone()),
        run_record: Some(run_record_path.to_string_lossy().into_owned()),
        artifact_locations: Some(artifact_locations),
        accounts,
    };
    let manifest = qx_core::RunManifest {
        run_id: spec.run_id.clone(),
        code_commit: context.code_commit().to_string(),
        config_hash: spec_fingerprint.clone(),
        data_fingerprint: joint_identity.data_fingerprint.clone(),
        input_components: joint_identity.input_components.clone(),
        clock_start: joint_identity.clock_start,
        clock_end: joint_identity.clock_end,
        global_seed: spec.seed,
        determinism_mode: true,
        result_hash: result_hash.clone(),
        strategy_version: format!("multi-account-joint-strategies:{canonical_spec_fingerprint}"),
        instrument_spec_version: "multi-account-joint-spec-v1".into(),
        model_fingerprint: qx_strategy::sha256_hex(b"MultiAccountJointBacktestEngine/v1"),
        input_event_hash: joint_identity.data_fingerprint.clone(),
        output_event_hash: result_hash.clone(),
        runtime_version,
        canonical_run_spec_json: Some(canonical_spec_json),
        canonical_run_spec_fingerprint: Some(canonical_spec_fingerprint),
    };
    let manifest_json = manifest
        .to_json()
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error))?;
    let joint_run_verifications_json = serde_json::to_value(&child_run_verifications)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let run_manifest_path_string = run_manifest_path.to_string_lossy().into_owned();
    let artifact_json = serde_json::to_string(&serde_json::json!({
        "schema_version": crate::artifact_store::MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_SCHEMA_VERSION,
        "spec": spec,
        "outcome": outcome,
        "shared_result_hash": result_hash,
        "result_hash": result_hash,
        "input_data_hash": joint_identity.data_fingerprint,
        "data_fingerprint": joint_identity.data_fingerprint,
        "clock_start": joint_identity.clock_start,
        "clock_end": joint_identity.clock_end,
        "run_manifest": run_manifest_path_string,
        "run_manifest_path": run_manifest_path_string,
        "account_run_verifications": joint_run_verifications_json,
        "joint_equity_path": outcome.equity_path,
        "joint_fills_path": outcome.fills_path,
        "aggregation": "sum_of_independent_exchange_account_equity_no_cross_account_margin_netting",
    }))
    .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let run_record = qx_spec::RunRecordV2 {
        schema_version: qx_spec::RUN_RECORD_V2_SCHEMA_VERSION,
        run_id: spec.run_id.clone(),
        status: qx_spec::RunStatus::Completed,
        input_digest: manifest.data_fingerprint.clone(),
        code_identity: format!("{}:{}", manifest.code_commit, manifest.runtime_version),
        config_fingerprint: manifest.config_hash.clone(),
        started_at_ms: manifest.clock_start,
        finished_at_ms: Some(manifest.clock_end),
        artifact_refs: vec![
            joint_artifact_ref(
                "joint_portfolio",
                crate::artifact_store::MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_KIND,
                crate::artifact_store::MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_SCHEMA_VERSION,
                artifact_json.as_bytes(),
            ),
            joint_artifact_ref(
                "joint_equity",
                "joint-equity-json",
                1,
                equity_json.as_bytes(),
            ),
            joint_artifact_ref("joint_fills", "joint-fills-json", 1, fills_json.as_bytes()),
            joint_artifact_ref(
                "run_manifest",
                "run-manifest-json",
                1,
                manifest_json.as_bytes(),
            ),
        ],
        replay_verdict: qx_spec::ReplayVerdict::Verified,
        capability_level: qx_spec::CapabilityLevel::L1,
    };
    let run_record_json = run_record
        .to_json()
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error))?;
    let manifest_exists = match std::fs::read_to_string(&run_manifest_path) {
        Ok(existing) if existing == manifest_json => true,
        Ok(_) => {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!(
                    "联合账户 RunManifest 已存在且内容不同: {}",
                    run_manifest_path.display()
                ),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(AppError::from_io("读取联合账户 RunManifest", &error)),
    };
    let equity_exists = existing_identical(&equity_path, &equity_json, "联合权益曲线")?;
    let fills_exists = existing_identical(&fills_path, &fills_json, "联合成交明细")?;
    let (artifact_exists, upgrade_legacy_artifact) = match std::fs::read_to_string(&artifact_path) {
        Ok(existing) if existing == artifact_json => (true, false),
        Ok(existing) => {
            if !verify_legacy_joint_artifact(&existing, &result_hash, context)? {
                return Err(AppError::new(
                    AppErrorCategory::Conflict,
                    format!(
                        "联合账户回测联合组合产物已存在且内容不同: {}",
                        artifact_path.display()
                    ),
                ));
            }
            (false, true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (false, false),
        Err(error) => return Err(AppError::from_io("读取联合账户联合组合产物", &error)),
    };
    let outcome_summary = serde_json::to_value(&outcome)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let (run_record_exists, upgrade_legacy_run_record) =
        existing_joint_run_record(&run_record_path, &run_record_json, &outcome_summary)?;
    for (path, contents, exists) in [
        (&equity_path, equity_json.as_str(), equity_exists),
        (&fills_path, fills_json.as_str(), fills_exists),
    ] {
        if !exists {
            write_text(&path.to_string_lossy(), contents)?;
        }
    }
    if !manifest_exists {
        write_text(&run_manifest_path.to_string_lossy(), &manifest_json)?;
    }
    if !artifact_exists {
        let mode = if upgrade_legacy_artifact {
            crate::artifact_store::sink::PublishMode::Overwrite
        } else {
            crate::artifact_store::sink::PublishMode::CreateNew
        };
        crate::artifact_store::sink::publish(&artifact_path, &artifact_json, mode).map_err(
            |error| match error {
                crate::artifact_store::sink::PublishError::Conflict(message) => {
                    AppError::new(AppErrorCategory::Conflict, message)
                }
                crate::artifact_store::sink::PublishError::Io(error) => {
                    AppError::from_io("发布联合账户联合组合产物", &error)
                }
            },
        )?;
    }
    if !run_record_exists {
        let mode = if upgrade_legacy_run_record {
            crate::artifact_store::sink::PublishMode::Overwrite
        } else {
            crate::artifact_store::sink::PublishMode::CreateNew
        };
        crate::artifact_store::sink::publish(&run_record_path, &run_record_json, mode).map_err(
            |error| match error {
                crate::artifact_store::sink::PublishError::Conflict(message) => {
                    AppError::new(AppErrorCategory::Conflict, message)
                }
                crate::artifact_store::sink::PublishError::Io(error) => {
                    AppError::from_io("发布联合账户 RunRecord V2", &error)
                }
            },
        )?;
    }
    Ok(Some(outcome))
}

fn joint_artifact_ref(
    name: &str,
    kind: &str,
    schema_version: u32,
    bytes: &[u8],
) -> qx_spec::ArtifactRefV2 {
    qx_spec::ArtifactRefV2 {
        name: name.to_string(),
        artifact_id: qx_strategy::sha256_hex(bytes),
        kind: kind.to_string(),
        schema_version,
        byte_len: bytes.len() as u64,
    }
}

fn existing_joint_run_record(
    path: &std::path::Path,
    expected: &str,
    summary: &Value,
) -> Result<(bool, bool), AppError> {
    match std::fs::read_to_string(path) {
        Ok(existing) if existing == expected => Ok((true, false)),
        Ok(existing) if is_legacy_joint_run_record(&existing, expected) => {
            crate::artifact_store::verify_declared_run_record(summary).map_err(|error| {
                AppError::new(
                    AppErrorCategory::Conflict,
                    format!("既有联合账户 RunRecord 未通过内容复核，拒绝升级: {error}"),
                )
            })?;
            Ok((false, true))
        }
        Ok(_) => Err(AppError::new(
            AppErrorCategory::Conflict,
            format!(
                "联合账户回测 RunRecord V2 已存在且内容不同: {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((false, false)),
        Err(error) => Err(AppError::from_io("读取联合账户 RunRecord V2", &error)),
    }
}

fn is_legacy_joint_run_record(existing: &str, expected: &str) -> bool {
    let (Ok(mut existing), Ok(expected)) = (
        serde_json::from_str::<Value>(existing),
        serde_json::from_str::<Value>(expected),
    ) else {
        return false;
    };
    let Some(artifact_refs) = existing
        .get_mut("artifact_refs")
        .and_then(Value::as_array_mut)
    else {
        return false;
    };
    let mut migrated = false;
    for reference in artifact_refs {
        if reference.get("name").and_then(Value::as_str) != Some("joint_portfolio") {
            continue;
        }
        if migrated
            || reference.get("kind").and_then(Value::as_str)
                != Some("multi-account-joint-backtest-result-json")
            || reference.get("schema_version").and_then(Value::as_u64) != Some(1)
        {
            return false;
        }
        reference["kind"] = Value::String(
            crate::artifact_store::MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_KIND.to_string(),
        );
        reference["schema_version"] = Value::from(
            crate::artifact_store::MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_SCHEMA_VERSION,
        );
        migrated = true;
    }
    migrated && existing == expected
}

fn verify_legacy_joint_artifact(
    payload: &str,
    expected_result_hash: &str,
    context: &RunContext,
) -> Result<bool, AppError> {
    let artifact: Value = match serde_json::from_str(payload) {
        Ok(artifact) => artifact,
        Err(_) => return Ok(false),
    };
    if artifact.get("schema_version").and_then(Value::as_u64)
        != Some(crate::artifact_store::MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_SCHEMA_VERSION as u64)
        || artifact.get("result_hash").and_then(Value::as_str) != Some(expected_result_hash)
    {
        return Ok(false);
    }
    let Some(outcome_value) = artifact.get("outcome").cloned() else {
        return Ok(false);
    };
    let old_outcome: MultiAccountJointBacktestOutcome = match serde_json::from_value(outcome_value)
    {
        Ok(outcome) => outcome,
        Err(_) => return Ok(false),
    };
    if old_outcome.result_hash.is_some()
        || old_outcome.run_record.is_some()
        || old_outcome.artifact_locations.is_some()
    {
        return Ok(false);
    }
    crate::read_multi_account_joint_report_v1(&old_outcome, context).map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("既有联合账户产物未通过完整复核，拒绝升级 RunRecord V2: {error}"),
        )
    })?;
    Ok(true)
}

fn existing_identical(
    path: &std::path::Path,
    expected: &str,
    label: &str,
) -> Result<bool, AppError> {
    match std::fs::read_to_string(path) {
        Ok(existing) if existing == expected => Ok(true),
        Ok(_) => Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("联合账户回测 {label} 已存在且内容不同: {}", path.display()),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(AppError::from_io(&format!("读取联合账户 {label}"), &error)),
    }
}

pub(crate) fn joint_child_run_id(run_id: &str, exchange_id: &str, account_id: &str) -> String {
    let mut hasher = Fnv1a::new();
    hasher.write_text(run_id);
    hasher.write_text(exchange_id);
    hasher.write_text(account_id);
    format!("joint-{:016x}", hasher.finish())
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
