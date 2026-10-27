//! Canonical parent identity for independently funded exchange account runs.

use crate::error::{AppError, AppErrorCategory};
use crate::{JointAccountBacktestResult, MultiAccountJointBacktestSpec};
use qx_core::{Fnv1a, RunManifest};
use qx_spec::{
    CanonicalDatasetIdentity, CanonicalRunClock, CanonicalRunSpec, CanonicalStrategyIdentity,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JointAccountRunVerification {
    pub exchange_id: String,
    pub account_id: String,
    pub run_manifest: RunManifest,
    /// The identity-bearing portion of the child artifact; large equity and ledger arrays stay
    /// in their own files and are not copied into the joint artifact.
    pub identity: Value,
}

pub(crate) struct JointRunIdentity {
    pub canonical_spec: CanonicalRunSpec,
    pub data_fingerprint: String,
    pub input_components: BTreeMap<String, String>,
    pub clock_start: u64,
    pub clock_end: u64,
}

pub(crate) fn build_joint_run_identity(
    spec: &MultiAccountJointBacktestSpec,
    children: &[JointAccountRunVerification],
    code_revision: &str,
    runtime_version: &str,
) -> Result<JointRunIdentity, AppError> {
    spec.validate()?;
    if children.len() != spec.accounts.len() {
        return Err(invalid("联合账户 RunSpec 子运行数量与账户数不一致"));
    }

    let mut children_by_account = HashMap::new();
    for child in children {
        if children_by_account
            .insert(
                (child.exchange_id.as_str(), child.account_id.as_str()),
                child,
            )
            .is_some()
        {
            return Err(invalid("联合账户 RunSpec 子运行账户身份重复"));
        }
    }

    let mut canonical_accounts = spec.accounts.clone();
    canonical_accounts.sort_by(|left, right| {
        left.exchange_id
            .cmp(&right.exchange_id)
            .then_with(|| left.account_id.cmp(&right.account_id))
    });
    let mut canonical_behavior_spec = spec.clone();
    canonical_behavior_spec.accounts = canonical_accounts.clone();
    let mut behavior = serde_json::to_value(&canonical_behavior_spec)
        .map_err(|error| internal_error(format!("联合账户规格转换失败: {error}")))?;
    let behavior_fields = behavior
        .as_object_mut()
        .ok_or_else(|| internal_error("联合账户 behavior 不是对象".into()))?;
    behavior_fields.remove("run_id");
    behavior_fields.remove("output_dir");
    behavior_fields.insert("seed".into(), Value::String(spec.seed.to_string()));
    let behavior_accounts = behavior
        .get_mut("accounts")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| internal_error("联合账户 behavior.accounts 不是数组".into()))?;
    if behavior_accounts.len() != canonical_accounts.len() {
        return Err(internal_error(
            "联合账户 behavior.accounts 数量与规格不一致".into(),
        ));
    }

    let mut datasets = Vec::new();
    let mut market_rules = BTreeMap::new();
    let mut strategies = Vec::new();
    let mut extensions = BTreeMap::new();
    let mut account_execution_models = Vec::with_capacity(canonical_accounts.len());
    let mut input_components = BTreeMap::new();
    let mut input_hasher = Fnv1a::new();
    let mut clock_start = u64::MAX;
    let mut clock_end = 0_u64;

    for (account, behavior_account) in canonical_accounts.iter().zip(behavior_accounts) {
        let child = children_by_account
            .get(&(account.exchange_id.as_str(), account.account_id.as_str()))
            .ok_or_else(|| invalid("联合账户 RunSpec 缺少账户子运行身份"))?;
        let manifest = &child.run_manifest;
        manifest
            .validate()
            .map_err(|error| invalid(format!("联合账户子运行 RunManifest 无效: {error}")))?;
        if manifest.code_commit != code_revision
            || manifest.runtime_version != runtime_version
            || manifest.global_seed != spec.seed
        {
            return Err(invalid(format!(
                "{}:{} 子运行代码/运行时/seed 与联合运行不一致",
                account.exchange_id, account.account_id
            )));
        }
        let child_canonical_json = manifest
            .canonical_run_spec_json
            .as_deref()
            .ok_or_else(|| invalid("联合账户子运行缺少 CanonicalRunSpec"))?;
        let child_canonical = CanonicalRunSpec::from_json(child_canonical_json)
            .map_err(|error| invalid(format!("联合账户子运行 CanonicalRunSpec 无效: {error}")))?;
        if child_canonical.run_type != "multi_market_backtest"
            || child_canonical.code_revision != code_revision
            || child_canonical.random_seed != spec.seed.to_string()
            || child_canonical
                .execution_model
                .get("runtime_version")
                .and_then(Value::as_str)
                != Some(runtime_version)
        {
            return Err(invalid("联合账户子运行类型、代码或执行器身份不匹配"));
        }
        let child_behavior = child_canonical
            .behavior
            .as_object()
            .ok_or_else(|| internal_error("多市场子运行 behavior 不是对象".into()))?;
        if child_behavior.get("account_id").and_then(Value::as_str)
            != Some(account.account_id.as_str())
            || child_behavior
                .get("settlement_currency")
                .and_then(Value::as_str)
                != Some(spec.settlement_currency.as_str())
            || child_behavior
                .get("initial_cash_raw")
                .and_then(Value::as_str)
                != Some(account.initial_cash_raw.to_string().as_str())
        {
            return Err(invalid("联合账户子运行账户/结算币/本金与父规格不一致"));
        }
        let child_markets = child_behavior
            .get("markets")
            .cloned()
            .ok_or_else(|| invalid("联合账户子运行 CanonicalRunSpec 缺少 markets"))?;
        let behavior_account_fields = behavior_account
            .as_object_mut()
            .ok_or_else(|| internal_error("联合账户 behavior 项不是对象".into()))?;
        behavior_account_fields.insert("markets".into(), child_markets);

        let expected_instruments = account
            .markets
            .iter()
            .map(|market| market.instrument.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let actual_instruments = child_canonical
            .datasets
            .iter()
            .map(|dataset| dataset.instrument.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if expected_instruments != actual_instruments
            || child_canonical.datasets.len() != account.markets.len()
            || child_canonical.strategies.len()
                != account
                    .markets
                    .iter()
                    .map(|market| market.strategies.len())
                    .sum::<usize>()
        {
            return Err(invalid("联合账户子运行标的/策略数量与父规格不一致"));
        }

        for dataset in &child_canonical.datasets {
            datasets.push(CanonicalDatasetIdentity {
                role: format!("bars:{}:{}", account.exchange_id, account.account_id),
                instrument: dataset.instrument.clone(),
                content_fingerprint: dataset.content_fingerprint.clone(),
                start_ts_ms: dataset.start_ts_ms,
                end_ts_ms: dataset.end_ts_ms,
            });
            let scoped_instrument = format!(
                "{}/{}/{}",
                account.exchange_id, account.account_id, dataset.instrument
            );
            let market_fingerprint = child_canonical
                .market_rules
                .get(&dataset.instrument)
                .ok_or_else(|| invalid("联合账户子运行缺少市场规则指纹"))?;
            market_rules.insert(scoped_instrument.clone(), market_fingerprint.clone());
            clock_start = clock_start.min(dataset.start_ts_ms);
            clock_end = clock_end.max(dataset.end_ts_ms);
        }
        for child_strategy in &child_canonical.strategies {
            let scope = format!(
                "{}/{}/{}",
                account.exchange_id, account.account_id, child_strategy.strategy_id
            );
            let scoped_id = format!("joint-{}", qx_strategy::sha256_hex(scope.as_bytes()));
            extensions.insert(format!("strategy_scope:{scoped_id}"), Value::String(scope));
            strategies.push(CanonicalStrategyIdentity {
                strategy_id: scoped_id,
                strategy_version: child_strategy.strategy_version.clone(),
                source_sha256: child_strategy.source_sha256.clone(),
                parameters: child_strategy.parameters.clone(),
            });
        }
        for (key, value) in &child_canonical.extensions {
            extensions.insert(
                format!(
                    "account:{}:{}:{}",
                    account.exchange_id, account.account_id, key
                ),
                value.clone(),
            );
        }
        for (key, value) in &manifest.input_components {
            let scoped_key = format!(
                "account:{}:{}:{key}",
                account.exchange_id, account.account_id
            );
            if input_components.insert(scoped_key, value.clone()).is_some() {
                return Err(invalid("联合账户输入组件身份重复"));
            }
        }
        input_hasher.write_text(&account.exchange_id);
        input_hasher.write_text(&account.account_id);
        input_hasher.write_text(&manifest.data_fingerprint);
        extensions.insert(
            format!(
                "account_data_fingerprint:{}:{}",
                account.exchange_id, account.account_id
            ),
            Value::String(manifest.data_fingerprint.clone()),
        );
        account_execution_models.push(serde_json::json!({
            "exchange_id": account.exchange_id,
            "account_id": account.account_id,
            "execution_model": child_canonical.execution_model,
        }));
    }
    if children_by_account.len() != canonical_accounts.len() || datasets.is_empty() {
        return Err(invalid("联合账户子运行存在孤儿项或缺少数据集"));
    }
    normalize_cross_language_values(&mut behavior, "behavior")?;
    let mut execution_model = serde_json::json!({
        "runtime_version": runtime_version,
        "matching_kernel": "multi-account-joint-bar-v1",
        "risk_model": "account-isolated-per-child-v1",
        "aggregation": "sum-of-independent-account-equity-v1",
        "cross_account_margin_netting": false,
        "settlement_currency": spec.settlement_currency,
        "account_execution_models": account_execution_models,
    });
    normalize_cross_language_values(&mut execution_model, "execution_model")?;
    let canonical_spec = CanonicalRunSpec::new(
        "multi_account_joint_backtest",
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
        spec.seed,
        behavior,
        execution_model,
        extensions,
    )
    .map_err(|error| invalid(format!("联合账户 CanonicalRunSpec 无效: {error}")))?;
    Ok(JointRunIdentity {
        canonical_spec,
        data_fingerprint: format!("{:016x}", input_hasher.finish()),
        input_components,
        clock_start,
        clock_end,
    })
}

pub(crate) fn joint_result_hash(
    canonical_fingerprint: &str,
    accounts: &[JointAccountBacktestResult],
    initial_cash_raw: i128,
    final_equity_raw: i128,
    return_bps: i32,
    max_drawdown_bps: u32,
    successful_fills: u64,
    markets_total: usize,
) -> String {
    let mut hasher = Fnv1a::new();
    hasher.write_text(canonical_fingerprint);
    for account in accounts {
        hasher.write_text(&account.exchange_id);
        hasher.write_text(&account.account_id);
        hasher.write_text(&account.outcome.shared_result_hash);
    }
    for value in [initial_cash_raw, final_equity_raw] {
        hasher.write_text(&value.to_string());
    }
    hasher.write_text(&return_bps.to_string());
    hasher.write_text(&max_drawdown_bps.to_string());
    hasher.write_text(&successful_fills.to_string());
    hasher.write_text(&markets_total.to_string());
    format!("{:016x}", hasher.finish())
}

fn normalize_cross_language_values(value: &mut Value, path: &str) -> Result<(), AppError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(number) if number.is_f64() => Err(invalid(format!(
            "{path} 含浮点值；CanonicalRunSpec 精确数值必须使用十进制文本"
        ))),
        Value::Number(number) => {
            if number
                .as_i64()
                .is_some_and(|integer| integer.unsigned_abs() > 9_007_199_254_740_991)
                || number
                    .as_u64()
                    .is_some_and(|integer| integer > 9_007_199_254_740_991)
            {
                return Err(invalid(format!(
                    "{path} 含跨语言不安全整数；CanonicalRunSpec 必须使用十进制文本"
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
                if name.ends_with("_raw") && child.is_number() {
                    *child = Value::String(child.to_string());
                } else {
                    normalize_cross_language_values(child, &format!("{path}.{name}"))?;
                }
            }
            Ok(())
        }
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message.into())
}

fn internal_error(message: String) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}
