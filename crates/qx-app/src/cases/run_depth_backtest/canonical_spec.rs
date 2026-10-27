//! Maps a resolved L1/L2 run into the shared, path-independent run identity.

use super::super::StrategyArtifactIdentity;
use crate::error::{AppError, AppErrorCategory};
use crate::DepthBacktestSpec;
use qx_spec::{
    CanonicalDatasetIdentity, CanonicalRunClock, CanonicalRunSpec, CanonicalStrategyIdentity,
};
use qx_xingban::{DepthFrame, OrderBookBacktestReport};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

pub(super) fn build_depth_canonical_run_spec(
    spec: &DepthBacktestSpec,
    custom_strategies: &[StrategyArtifactIdentity],
    code_revision: &str,
    frame: &DepthFrame,
    report: &OrderBookBacktestReport,
    runtime_version: &str,
    instrument_spec_version: &str,
    risk_rule_set_version: &str,
    effective_account_id: &str,
    effective_fee_bps: i128,
    quality_manifest_fingerprint: Option<&str>,
) -> Result<CanonicalRunSpec, AppError> {
    let mut behavior = serde_json::to_value(spec)
        .map_err(|error| internal_error(format!("DepthBacktestSpec 转换失败: {error}")))?;
    let behavior_fields = behavior
        .as_object_mut()
        .ok_or_else(|| internal_error("DepthBacktestSpec 序列化结果不是 JSON 对象".into()))?;
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
        behavior_fields.remove(locator_or_provenance);
    }
    normalize_cross_language_values(&mut behavior, "behavior")?;

    let custom_by_id = custom_strategies
        .iter()
        .map(|identity| (identity.strategy_id.as_str(), identity))
        .collect::<HashMap<_, _>>();
    let mut strategy_specs = vec![&spec.strategy];
    if let Some(portfolio) = &spec.portfolio {
        strategy_specs.extend(
            portfolio
                .additional_strategies
                .iter()
                .map(|member| &member.strategy),
        );
    }
    let mut strategies = Vec::with_capacity(strategy_specs.len());
    for (index, strategy_spec) in strategy_specs.into_iter().enumerate() {
        let custom = custom_by_id.get(strategy_spec.strategy_id.as_str());
        let strategy_version = custom
            .map(|identity| identity.strategy_version.clone())
            .unwrap_or_else(|| {
                if index == 0 {
                    format!("builtin-{}-v1", strategy_spec.kind.name())
                } else {
                    format!("{}-v1", strategy_spec.strategy_id)
                }
            });
        let mut parameters = serde_json::to_value(strategy_spec).map_err(|error| {
            internal_error(format!("深度策略参数转换 CanonicalRunSpec 失败: {error}"))
        })?;
        normalize_cross_language_values(&mut parameters, "strategies.parameters")?;
        strategies.push(CanonicalStrategyIdentity {
            strategy_id: strategy_spec.strategy_id.clone(),
            strategy_version,
            source_sha256: custom.map(|identity| identity.source_sha256.clone()),
            parameters,
        });
    }

    let first = frame
        .snapshots
        .first()
        .ok_or_else(|| internal_error("构建 CanonicalRunSpec 时 DepthFrame 为空".into()))?;
    let last = frame
        .snapshots
        .last()
        .ok_or_else(|| internal_error("构建 CanonicalRunSpec 时 DepthFrame 没有结束时间".into()))?;
    let dataset = CanonicalDatasetIdentity {
        role: "depth_snapshots".into(),
        instrument: frame.instrument.to_string(),
        // Keep source labels (which may contain a host-local locator) out of the canonical JSON.
        // The summary still retains the legacy data_fingerprint; the content identity here is
        // bound to the same verified frame hash through `input.fingerprint` on read-back.
        content_fingerprint: format!("depth-frame-fnv1a64:{:016x}", frame.input_hash()),
        start_ts_ms: first.ts,
        end_ts_ms: last.ts,
    };
    let clock = CanonicalRunClock {
        start_ts_ms: report.clock_start,
        end_ts_ms: report.clock_end,
        timestamp_unit: "unix_ms".into(),
        timezone: "UTC".into(),
    };
    let market_rules =
        BTreeMap::from([(frame.instrument.to_string(), instrument_spec_version.into())]);
    let extensions = quality_manifest_fingerprint
        .map(|fingerprint| {
            BTreeMap::from([(
                "dataset_quality_fingerprint".into(),
                Value::String(fingerprint.into()),
            )])
        })
        .unwrap_or_default();
    let execution_model = serde_json::json!({
        "runtime_version": runtime_version,
        "matching_kernel": super::artifacts::matching_kernel(&spec.tier),
        "risk_rule_set_version": risk_rule_set_version,
        "account_id": effective_account_id,
        "effective_fee_bps": effective_fee_bps.to_string(),
        "active_strategy_kind": if spec.market_making.is_some() { "market_making" } else if spec.portfolio.is_some() { "portfolio" } else { "single_strategy" },
        "model_descriptors": report.model_descriptors,
        "execution_model_descriptor": report.execution_model_descriptor,
    });
    CanonicalRunSpec::new(
        "depth_backtest",
        code_revision,
        strategies,
        vec![dataset],
        market_rules,
        clock,
        0,
        behavior,
        execution_model,
        extensions,
    )
    .map_err(|error| internal_error(format!("Depth CanonicalRunSpec 无效: {error}")))
}

fn normalize_cross_language_values(value: &mut Value, path: &str) -> Result<(), AppError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(number) if number.is_f64() => Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!("{path} 含浮点值；CanonicalRunSpec 的精确数值必须使用十进制文本"),
        )),
        Value::Number(number) => {
            if number
                .as_i64()
                .is_some_and(|integer| integer.unsigned_abs() > 9_007_199_254_740_991)
                || number
                    .as_u64()
                    .is_some_and(|integer| integer > 9_007_199_254_740_991)
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("{path} 含跨语言不安全整数；请在 CanonicalRunSpec 中用十进制文本表示"),
                ));
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
                // Fixed-point quantities use i128 at the Rust boundary and may exceed the exact
                // JSON integer range even when their decimal representation is small.
                if is_exact_integer_field(name) && child.is_number() {
                    *child = Value::String(child.to_string());
                    continue;
                }
                normalize_cross_language_values(child, &format!("{path}.{name}"))?;
            }
            Ok(())
        }
    }
}

fn is_exact_integer_field(name: &str) -> bool {
    name.ends_with("_raw")
        || matches!(
            name,
            "quantity_raw"
                | "contract_size"
                | "price_tick"
                | "qty_step"
                | "min_qty"
                | "min_notional_raw"
                | "quote_qty_raw"
                | "max_inventory_raw"
                | "tick_size_raw"
                | "reserved_capital_raw"
                | "initial_cash_raw"
                | "seed"
        )
}

fn internal_error(message: String) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_market_and_quantity_integers_use_decimal_text() {
        let mut value = serde_json::json!({
            "contract_size": 9_007_199_254_740_992_i128,
            "price_tick": 1_000_000_000_000_000_000_i128,
            "quantity_raw": 1_000_000_000_i128,
            "valid_from": 1_700_000_000_000_u64,
            "max_leverage": 20_u32
        });
        normalize_cross_language_values(&mut value, "test").unwrap();
        assert_eq!(value["contract_size"], "9007199254740992");
        assert_eq!(value["price_tick"], "1000000000000000000");
        assert_eq!(value["quantity_raw"], "1000000000");
        assert_eq!(value["valid_from"], 1_700_000_000_000_u64);
        assert_eq!(value["max_leverage"], 20);
    }

    #[test]
    fn canonical_behavior_rejects_floating_point_values() {
        let error =
            normalize_cross_language_values(&mut serde_json::json!({"ratio": 0.25}), "test")
                .unwrap_err();
        assert!(error.to_string().contains("浮点值"));
    }
}
