//! Path-free CanonicalRunSpec mapping for shared-account multi-market Depth runs.

use super::super::run_backtest::strategy_slot::strategy_version;
use crate::error::{AppError, AppErrorCategory};
use crate::{
    multi_market_depth_input_set_identity_v1, MultiMarketDepthBacktestSpec,
    MultiMarketDepthInputIdentityV1, StrategyArtifactIdentity,
};
use qx_spec::{
    CanonicalDatasetIdentity, CanonicalRunClock, CanonicalRunSpec, CanonicalStrategyIdentity,
};
use qx_xingban::OrderBookExecutionDescriptor;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

pub(crate) fn build_multi_market_depth_canonical_run_spec(
    spec: &MultiMarketDepthBacktestSpec,
    strategy_identities: &[StrategyArtifactIdentity],
    data_identities: &[MultiMarketDepthInputIdentityV1],
    execution_models: &BTreeMap<String, OrderBookExecutionDescriptor>,
    code_revision: &str,
    runtime_version: &str,
    clock_start: u64,
    clock_end: u64,
) -> Result<CanonicalRunSpec, AppError> {
    spec.validate()?;
    multi_market_depth_input_set_identity_v1(spec, data_identities)?;
    let input_clock_start = data_identities
        .iter()
        .map(|identity| identity.start_ts_ms)
        .min()
        .ok_or_else(|| invalid("多市场深度 CanonicalRunSpec 缺少输入开始时间"))?;
    let input_clock_end = data_identities
        .iter()
        .map(|identity| identity.end_ts_ms)
        .max()
        .ok_or_else(|| invalid("多市场深度 CanonicalRunSpec 缺少输入结束时间"))?;
    if clock_start != input_clock_start || clock_end != input_clock_end {
        return Err(invalid("多市场深度运行时钟必须覆盖全部输入帧的首末时间"));
    }
    let custom_by_id = strategy_identities
        .iter()
        .map(|identity| (identity.strategy_id.as_str(), identity))
        .collect::<HashMap<_, _>>();
    if custom_by_id.len() != strategy_identities.len() {
        return Err(invalid("多市场深度 CanonicalRunSpec 策略身份重复"));
    }
    let data_by_instrument = data_identities
        .iter()
        .map(|identity| (identity.instrument.as_str(), identity))
        .collect::<HashMap<_, _>>();
    if execution_models.len() != spec.markets.len() {
        return Err(invalid(
            "多市场深度 CanonicalRunSpec 撮合模型数量与市场数不一致",
        ));
    }

    let mut behavior = serde_json::to_value(spec).map_err(|error| {
        internal_error(format!("MultiMarketDepthBacktestSpec 转换失败: {error}"))
    })?;
    let behavior_fields = behavior
        .as_object_mut()
        .ok_or_else(|| internal_error("MultiMarketDepthBacktestSpec 序列化结果不是对象".into()))?;
    behavior_fields.remove("run_id");
    behavior_fields.remove("output_dir");
    behavior_fields.insert("seed".into(), Value::String("0".into()));
    let market_values = behavior_fields
        .get_mut("markets")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| internal_error("MultiMarketDepthBacktestSpec.markets 不是数组".into()))?;

    let mut datasets = Vec::with_capacity(spec.markets.len());
    let mut market_rules = BTreeMap::new();
    let mut strategies = Vec::with_capacity(spec.markets.len());
    let mut extensions = BTreeMap::new();
    let mut expected_execution_models = BTreeMap::new();

    for (market, market_value) in spec.markets.iter().zip(market_values) {
        let data = data_by_instrument
            .get(market.instrument.as_str())
            .ok_or_else(|| {
                invalid(format!(
                    "CanonicalRunSpec 缺少 {} 的 DepthFrame",
                    market.instrument
                ))
            })?;
        let data_tier = market.data_tier.as_deref().unwrap_or("l2");
        let content_fingerprint = format!("depth-frame-fnv1a64:{:016x}", data.input_hash);
        datasets.push(CanonicalDatasetIdentity {
            role: "depth_snapshots".into(),
            instrument: market.instrument.clone(),
            content_fingerprint,
            start_ts_ms: data.start_ts_ms,
            end_ts_ms: data.end_ts_ms,
        });

        let market_fields = market_value
            .as_object_mut()
            .ok_or_else(|| internal_error("多市场深度行为项不是对象".into()))?;
        market_fields.remove("depth_path");
        market_fields.remove("dataset_manifest_path");
        market_fields.insert("data_tier".into(), Value::String(data_tier.to_owned()));
        let effective_order_policy = market.order_policy.unwrap_or_default();
        market_fields.insert(
            "order_policy".into(),
            serde_json::to_value(effective_order_policy)
                .map_err(|error| internal_error(format!("有效订单策略转换失败: {error}")))?,
        );

        let mut market_rule = serde_json::to_value(&market.market_spec)
            .map_err(|error| internal_error(format!("市场规则转换失败: {error}")))?;
        normalize_cross_language_values(&mut market_rule, "market_rules")?;
        let market_rule_bytes = serde_json::to_vec(&market_rule)
            .map_err(|error| internal_error(format!("市场规则编码失败: {error}")))?;
        market_rules.insert(
            market.instrument.clone(),
            qx_strategy::sha256_hex(&market_rule_bytes),
        );

        let strategy = &market.strategy;
        let custom = custom_by_id.get(strategy.strategy_id.as_str());
        if let Some(identity) = custom {
            identity.validate_for_slot(&strategy.strategy_id, strategy)?;
        }
        let mut parameters = serde_json::to_value(strategy)
            .map_err(|error| internal_error(format!("多市场深度策略参数转换失败: {error}")))?;
        normalize_cross_language_values(&mut parameters, "strategies.parameters")?;
        strategies.push(CanonicalStrategyIdentity {
            strategy_id: strategy.strategy_id.clone(),
            strategy_version: custom
                .map(|identity| identity.strategy_version.clone())
                .unwrap_or_else(|| strategy_version(strategy)),
            source_sha256: custom.map(|identity| identity.source_sha256.clone()),
            parameters,
        });

        let descriptor = execution_models
            .get(&market.instrument)
            .ok_or_else(|| invalid(format!("{} 缺少生效撮合模型", market.instrument)))?;
        let expected = market.execution_model()?.audit_descriptor();
        if descriptor != &expected {
            return Err(invalid(format!(
                "{} 生效撮合模型与输入规格不一致",
                market.instrument
            )));
        }
        expected_execution_models.insert(market.instrument.clone(), descriptor.clone());

        if market.dataset_manifest_path.is_some() {
            let fingerprint = data
                .quality_fingerprint
                .as_deref()
                .ok_or_else(|| invalid(format!("{} 缺少质量清单指纹", market.instrument)))?;
            extensions.insert(
                format!("dataset_quality:{}", market.instrument),
                Value::String(fingerprint.to_owned()),
            );
        }
    }
    if custom_by_id.keys().any(|strategy_id| {
        !spec
            .markets
            .iter()
            .any(|market| market.strategy.strategy_id == *strategy_id)
    }) {
        return Err(invalid(
            "多市场深度 CanonicalRunSpec 含有 spec 之外的策略身份",
        ));
    }
    normalize_cross_language_values(&mut behavior, "behavior")?;

    let mut execution_model = serde_json::json!({
        "runtime_version": runtime_version,
        "matching_kernel": "portfolio-orderbook-engine-v1",
        "risk_model": "conservative-default-v1",
        "account_id": spec.account_id,
        "settlement_currency": spec.settlement_currency,
        "execution_models": expected_execution_models,
    });
    normalize_cross_language_values(&mut execution_model, "execution_model")?;
    CanonicalRunSpec::new(
        "multi_market_depth_backtest",
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
    .map_err(|error| internal_error(format!("Multi-market depth CanonicalRunSpec 无效: {error}")))
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
                        "contract_size" | "price_tick" | "qty_step" | "min_qty"
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
