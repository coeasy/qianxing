//! Canonical identity for a shared-account two-leg Bar run.

use crate::error::{AppError, AppErrorCategory};
use crate::MultiLegBacktestSpec;
use qx_core::TradingInstrumentSpec;
use qx_spec::{
    CanonicalDatasetIdentity, CanonicalRunClock, CanonicalRunSpec, CanonicalStrategyIdentity,
};
use serde_json::Value;
use std::collections::BTreeMap;

const MATCHING_KERNEL: &str = "PortfolioBarEngine/NextBarOpenFillModel/ZeroLatency/v1";

pub(crate) fn build_multi_leg_canonical_run_spec(
    spec: &MultiLegBacktestSpec,
    datasets: Vec<CanonicalDatasetIdentity>,
    dataset_quality: Option<&Value>,
    primary_execution_spec: &TradingInstrumentSpec,
    reference_execution_spec: &TradingInstrumentSpec,
    strategy_id: &str,
    strategy_version: &str,
    strategy_source_sha256: Option<&str>,
    account_id: &str,
    funding_provider: Option<&Value>,
    code_revision: &str,
    runtime_version: &str,
    clock_start: u64,
    clock_end: u64,
) -> Result<CanonicalRunSpec, AppError> {
    if datasets.len() != 2
        || datasets[0].role != "bars:primary"
        || datasets[1].role != "bars:reference"
    {
        return Err(invalid(
            "多腿 CanonicalRunSpec 必须按主腿、对冲腿顺序提供两份数据身份",
        ));
    }
    let mut behavior = serde_json::to_value(spec)
        .map_err(|error| internal(format!("MultiLegBacktestSpec 转换失败: {error}")))?;
    let fields = behavior
        .as_object_mut()
        .ok_or_else(|| internal("多腿 behavior 根节点不是 object"))?;
    for locator in [
        "run_id",
        "primary_bars_path",
        "reference_bars_path",
        "output_dir",
        "primary_dataset_manifest_path",
        "reference_dataset_manifest_path",
    ] {
        fields.remove(locator);
    }
    normalize_values(&mut behavior, "behavior")?;

    let mut strategy_parameters = serde_json::to_value(&spec.strategy)
        .map_err(|error| internal(format!("多腿策略参数序列化失败: {error}")))?;
    normalize_values(&mut strategy_parameters, "strategies.parameters")?;
    let strategies = vec![CanonicalStrategyIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: strategy_source_sha256.map(str::to_string),
        parameters: strategy_parameters,
    }];

    let primary_market_json = serde_json::to_value(primary_execution_spec)
        .map_err(|error| internal(format!("主腿生效市场规格序列化失败: {error}")))?;
    let reference_market_json = serde_json::to_value(reference_execution_spec)
        .map_err(|error| internal(format!("对冲腿生效市场规格序列化失败: {error}")))?;
    let mut market_rules = BTreeMap::new();
    market_rules.insert(
        datasets[0].instrument.clone(),
        fingerprint_json(&primary_market_json)?,
    );
    market_rules.insert(
        datasets[1].instrument.clone(),
        fingerprint_json(&reference_market_json)?,
    );

    let mut extensions = BTreeMap::new();
    if let Some(quality) = dataset_quality {
        for leg in ["primary", "reference"] {
            let fingerprint = quality
                .get(leg)
                .and_then(|value| value.get("manifest_fingerprint"))
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(format!("多腿 {leg} 质量清单缺少 fingerprint")))?;
            extensions.insert(
                format!("dataset_quality:{leg}"),
                Value::String(fingerprint.to_string()),
            );
        }
    }
    if let Some(provenance) = funding_provider {
        if !provenance.is_object() {
            return Err(invalid("多腿资金费 Provider provenance 必须是对象"));
        }
        extensions.insert("funding_provider_provenance".into(), provenance.clone());
    }

    let mut execution_model = serde_json::json!({
        "runtime_version": runtime_version,
        "matching_kernel": MATCHING_KERNEL,
        "risk_model": "shared_account_no_margin_v1",
        "account_id": account_id,
        "settlement_currency": spec.settlement_currency,
        "primary_market_spec": primary_market_json,
        "reference_market_spec": reference_market_json,
        "primary_maker_fee_bps": spec.primary_maker_fee_bps.unwrap_or(spec.maker_fee_bps),
        "primary_taker_fee_bps": spec.primary_taker_fee_bps.unwrap_or(spec.taker_fee_bps),
        "reference_maker_fee_bps": spec.reference_maker_fee_bps.unwrap_or(spec.maker_fee_bps),
        "reference_taker_fee_bps": spec.reference_taker_fee_bps.unwrap_or(spec.taker_fee_bps),
        "fill_model": spec.fill_model,
        "funding_input_kind": super::funding::input_kind(spec),
        "funding_bps": spec.funding_bps,
        "funding_events": spec.funding_events,
        "funding_provider": funding_provider,
    });
    normalize_values(&mut execution_model, "execution_model")?;

    CanonicalRunSpec::new(
        "multi_leg_backtest",
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
    .map_err(|error| invalid(format!("多腿 CanonicalRunSpec 无效: {error}")))
}

pub(crate) fn multi_leg_input_components(
    datasets: &[CanonicalDatasetIdentity],
    dataset_quality: Option<&Value>,
    funding_provider: Option<&Value>,
) -> Result<BTreeMap<String, String>, AppError> {
    if datasets.len() != 2 {
        return Err(invalid("多腿 RunManifest 必须包含两份输入数据身份"));
    }
    let mut components = BTreeMap::from([
        (
            "primary_bars".into(),
            datasets[0].content_fingerprint.clone(),
        ),
        (
            "reference_bars".into(),
            datasets[1].content_fingerprint.clone(),
        ),
    ]);
    if let Some(quality) = dataset_quality {
        for leg in ["primary", "reference"] {
            let fingerprint = quality
                .get(leg)
                .and_then(|value| value.get("manifest_fingerprint"))
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(format!("多腿 {leg} 质量清单 fingerprint 无效")))?;
            components.insert(format!("{leg}_quality"), fingerprint.to_string());
        }
    }
    if let Some(provider) = funding_provider {
        for (field, name) in [
            ("source_hash", "funding_provider_source"),
            ("query_hash", "funding_provider_query"),
        ] {
            let value = provider
                .get(field)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid(format!("资金费 Provider provenance 缺少 {field}")))?;
            components.insert(name.into(), value.to_string());
        }
    }
    Ok(components)
}

pub(crate) fn effective_market_specs(
    spec: &MultiLegBacktestSpec,
    primary: &qx_core::InstrumentId,
    reference: &qx_core::InstrumentId,
) -> (TradingInstrumentSpec, TradingInstrumentSpec) {
    (
        super::execution_market_spec(
            spec.primary_market_spec.as_ref(),
            primary,
            &spec.settlement_currency,
        ),
        super::execution_market_spec(
            spec.reference_market_spec.as_ref(),
            reference,
            &spec.settlement_currency,
        ),
    )
}

fn fingerprint_json(value: &Value) -> Result<String, AppError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| internal(format!("多腿市场规则编码失败: {error}")))?;
    Ok(qx_strategy::sha256_hex(&bytes))
}

fn normalize_values(value: &mut Value, path: &str) -> Result<(), AppError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(number) if number.is_f64() => Err(invalid(format!(
            "{path} 含浮点值，CanonicalRunSpec 需要精确数值"
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
                    "{path} 含跨语言不安全整数，需使用十进制文本"
                )));
            }
            Ok(())
        }
        Value::Array(values) => {
            for (index, child) in values.iter_mut().enumerate() {
                normalize_values(child, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            for (name, child) in fields.iter_mut() {
                if (name.ends_with("_raw") || name == "seed") && child.is_number() {
                    *child = Value::String(child.to_string());
                } else {
                    normalize_values(child, &format!("{path}.{name}"))?;
                }
            }
            Ok(())
        }
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message.into())
}

fn internal(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message.into())
}
