//! Path-free CanonicalRunSpec mapping for shared-account multi-market Bar runs.

use super::super::run_backtest::strategy_slot::strategy_version;
use super::super::run_multi_market_manifest::DataIdentity;
use super::super::MultiMarketExecutionWindow;
use crate::error::{AppError, AppErrorCategory};
use crate::{MultiMarketBacktestSpec, StrategyArtifactIdentity};
use qx_core::{OrderPolicy, TradingInstrumentSpec};
use qx_spec::{
    CanonicalDatasetIdentity, CanonicalRunClock, CanonicalRunSpec, CanonicalStrategyIdentity,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

pub(crate) fn build_multi_market_canonical_run_spec(
    spec: &MultiMarketBacktestSpec,
    strategy_identities: &[StrategyArtifactIdentity],
    data_identities: &[DataIdentity],
    code_revision: &str,
    runtime_version: &str,
    clock_start: u64,
    clock_end: u64,
    execution_window: Option<MultiMarketExecutionWindow>,
    experiment_parameters: Option<&BTreeMap<String, i128>>,
) -> Result<CanonicalRunSpec, AppError> {
    if spec.markets.len() != data_identities.len() {
        return Err(invalid(
            "多市场 CanonicalRunSpec 输入数据数量与市场数不一致",
        ));
    }
    let custom_by_id = strategy_identities
        .iter()
        .map(|identity| (identity.strategy_id.as_str(), identity))
        .collect::<HashMap<_, _>>();
    let data_by_instrument = data_identities
        .iter()
        .map(|identity| (identity.instrument.as_str(), identity))
        .collect::<HashMap<_, _>>();

    let mut behavior = serde_json::to_value(spec)
        .map_err(|error| internal_error(format!("MultiMarketBacktestSpec 转换失败: {error}")))?;
    let behavior_fields = behavior
        .as_object_mut()
        .ok_or_else(|| internal_error("MultiMarketBacktestSpec 序列化结果不是对象".into()))?;
    behavior_fields.remove("run_id");
    behavior_fields.remove("output_dir");
    let market_values = behavior_fields
        .get_mut("markets")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| internal_error("MultiMarketBacktestSpec.markets 不是数组".into()))?;

    let mut datasets = Vec::with_capacity(spec.markets.len());
    let mut market_rules = BTreeMap::new();
    let mut effective_markets = Vec::with_capacity(spec.markets.len());
    let mut strategies = Vec::new();
    let mut extensions = BTreeMap::new();

    for (market, market_value) in spec.markets.iter().zip(market_values) {
        let data = data_by_instrument
            .get(market.instrument.as_str())
            .ok_or_else(|| {
                invalid(format!(
                    "CanonicalRunSpec 缺少 {} 的 BarFrame",
                    market.instrument
                ))
            })?;
        let instrument = qx_core::InstrumentId::parse(&market.instrument)
            .ok_or_else(|| invalid(format!("CanonicalRunSpec 标的无效: {}", market.instrument)))?;
        let contract = market
            .market_spec
            .as_ref()
            .map(|snapshot| crate::market_spec_from_value(&instrument, snapshot))
            .transpose()?
            .unwrap_or_else(|| {
                TradingInstrumentSpec::spot_default(&instrument, &spec.settlement_currency)
            });
        let market_spec_json = serde_json::to_value(&contract)
            .map_err(|error| internal_error(format!("TradingInstrumentSpec 转换失败: {error}")))?;
        let market_fields = market_value
            .as_object_mut()
            .ok_or_else(|| internal_error("多市场行为项不是对象".into()))?;
        market_fields.remove("bars_path");
        market_fields.remove("dataset_manifest_path");
        market_fields.insert("market_spec".into(), market_spec_json.clone());
        let spec_bytes = serde_json::to_vec(&market_spec_json)
            .map_err(|error| internal_error(format!("市场规则编码失败: {error}")))?;
        market_rules.insert(
            market.instrument.clone(),
            qx_strategy::sha256_hex(&spec_bytes),
        );

        let default_policy = OrderPolicy {
            margin_mode: contract.margin_mode_default(),
            ..OrderPolicy::default()
        };
        let order_policy = market.order_policy.unwrap_or(default_policy);
        let fee_schedule = market.fee_schedule.unwrap_or_default();
        let derived_margin_model = market
            .market_spec
            .as_ref()
            .filter(|snapshot| {
                snapshot
                    .get("leverage_tiers")
                    .and_then(Value::as_array)
                    .is_some_and(|tiers| !tiers.is_empty())
            })
            .map(crate::backtest_margin_model_from_market);
        let margin_model = market
            .margin_model
            .as_ref()
            .or(derived_margin_model.as_ref())
            .cloned()
            .unwrap_or_else(|| {
                if contract.product.supports_leverage() {
                    crate::BacktestMarginConfig::Leverage {
                        leverage: order_policy.leverage,
                        maintenance_bp: i64::from(contract.maintenance_margin_bps),
                    }
                } else {
                    crate::BacktestMarginConfig::NoMargin
                }
            });
        effective_markets.push(serde_json::json!({
            "instrument": market.instrument,
            "market_spec": market_spec_json,
            "order_policy": order_policy,
            "fee_schedule": fee_schedule,
            "margin_model": margin_model,
            "funding_bps": market.funding_bps,
            "funding_events": market.funding_events,
        }));

        datasets.push(CanonicalDatasetIdentity {
            role: "bars".into(),
            instrument: market.instrument.clone(),
            content_fingerprint: data.content_hash.clone(),
            start_ts_ms: data.start_ts_ms,
            end_ts_ms: data.end_ts_ms,
        });
        if market.dataset_manifest_path.is_some() {
            let fingerprint = data
                .quality_fingerprint
                .as_deref()
                .ok_or_else(|| invalid(format!("{} 缺少质量清单指纹", market.instrument)))?;
            extensions.insert(
                format!("dataset_quality:{}", market.instrument),
                Value::String(fingerprint.to_string()),
            );
        }
        for member in &market.strategies {
            let strategy = &member.strategy;
            let custom = custom_by_id.get(strategy.strategy_id.as_str());
            let mut parameters = serde_json::to_value(strategy)
                .map_err(|error| internal_error(format!("多市场策略参数转换失败: {error}")))?;
            normalize_cross_language_values(&mut parameters, "strategies.parameters")?;
            strategies.push(CanonicalStrategyIdentity {
                strategy_id: strategy.strategy_id.clone(),
                strategy_version: custom
                    .map(|identity| identity.strategy_version.clone())
                    .unwrap_or_else(|| strategy_version(strategy)),
                source_sha256: custom.map(|identity| identity.source_sha256.clone()),
                parameters,
            });
        }
    }
    normalize_cross_language_values(&mut behavior, "behavior")?;
    if let Some(window) = execution_window {
        behavior["execution_window"] = serde_json::to_value(window)
            .map_err(|error| internal_error(format!("执行窗口转换失败: {error}")))?;
    }
    if let Some(parameters) = experiment_parameters {
        behavior["experiment_parameters"] = Value::Object(
            parameters
                .iter()
                .map(|(key, value)| (key.clone(), Value::String(value.to_string())))
                .collect(),
        );
    }
    let mut execution_model = serde_json::json!({
        "runtime_version": runtime_version,
        "matching_kernel": "portfolio-bar-engine-v1",
        "risk_model": "conservative-default-v1",
        "fill_model": "next-bar-open-v1",
        "latency_model": "zero-latency-v1",
        "account_id": spec.account_id,
        "settlement_currency": spec.settlement_currency,
        "effective_markets": effective_markets,
    });
    normalize_cross_language_values(&mut execution_model, "execution_model")?;
    CanonicalRunSpec::new(
        "multi_market_backtest",
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
    .map_err(|error| internal_error(format!("Multi-market CanonicalRunSpec 无效: {error}")))
}

fn normalize_cross_language_values(value: &mut Value, path: &str) -> Result<(), AppError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(number) if number.is_f64() => Err(invalid(format!(
            "{path} 含浮点值；CanonicalRunSpec 的精确数值必须使用十进制文本"
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
                if (name.ends_with("_raw")
                    || matches!(
                        name.as_str(),
                        "seed" | "contract_size" | "price_tick" | "qty_step" | "min_qty"
                    ))
                    && child.is_number()
                {
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
