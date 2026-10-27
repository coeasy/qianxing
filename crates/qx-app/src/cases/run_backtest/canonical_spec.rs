//! Maps a resolved Bar backtest into the cross-run-type CanonicalRunSpec contract.

use super::{BacktestSpec, StrategyArtifactIdentity};
use crate::error::{AppError, AppErrorCategory};
use qx_datastruct::BarFrame;
use qx_spec::{
    CanonicalDatasetIdentity, CanonicalRunClock, CanonicalRunSpec, CanonicalStrategyIdentity,
};
use qx_xingban::BacktestReport;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

pub(super) fn build_bar_canonical_run_spec(
    spec: &BacktestSpec,
    strategies: &[StrategyArtifactIdentity],
    code_revision: &str,
    data_fingerprint: &str,
    frame: &BarFrame,
    report: &BacktestReport,
    runtime_version: &str,
    instrument_spec_version: &str,
    risk_rule_set_version: &str,
    effective_account_id: &str,
    extensions: BTreeMap<String, Value>,
) -> Result<CanonicalRunSpec, AppError> {
    let mut behavior = serde_json::to_value(spec).map_err(|error| {
        internal_error(format!("BacktestSpec 转换 CanonicalRunSpec 失败: {error}"))
    })?;
    let behavior_fields = behavior
        .as_object_mut()
        .ok_or_else(|| internal_error("BacktestSpec 序列化结果不是 JSON 对象".into()))?;
    for path_or_execution_id in ["run_id", "bars_path", "output_dir"] {
        behavior_fields.remove(path_or_execution_id);
    }
    normalize_cross_language_values(&mut behavior, "behavior")?;

    let strategy_artifacts = strategies
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
    let mut canonical_strategies = Vec::with_capacity(strategy_specs.len());
    for strategy_spec in strategy_specs {
        let identity = strategy_artifacts.get(strategy_spec.strategy_id.as_str());
        let version = identity
            .map(|identity| identity.strategy_version.clone())
            .unwrap_or_else(|| super::strategy_version(strategy_spec));
        let mut parameters = serde_json::to_value(strategy_spec).map_err(|error| {
            internal_error(format!("策略参数转换 CanonicalRunSpec 失败: {error}"))
        })?;
        normalize_cross_language_values(&mut parameters, "strategies.parameters")?;
        canonical_strategies.push(CanonicalStrategyIdentity {
            strategy_id: strategy_spec.strategy_id.clone(),
            strategy_version: version,
            source_sha256: identity.map(|identity| identity.source_sha256.clone()),
            parameters,
        });
    }

    let first =
        frame.ts.first().copied().ok_or_else(|| {
            internal_error("构建 CanonicalRunSpec 时 BarFrame 没有时间范围".into())
        })?;
    let last =
        frame.ts.last().copied().ok_or_else(|| {
            internal_error("构建 CanonicalRunSpec 时 BarFrame 没有结束时间".into())
        })?;
    let dataset = CanonicalDatasetIdentity {
        role: "bars".into(),
        instrument: spec.instrument.clone(),
        content_fingerprint: data_fingerprint.to_string(),
        start_ts_ms: first,
        end_ts_ms: last,
    };
    let clock = CanonicalRunClock {
        start_ts_ms: report.clock_start,
        end_ts_ms: report.clock_end,
        timestamp_unit: "unix_ms".into(),
        timezone: "UTC".into(),
    };
    let market_rules = BTreeMap::from([(spec.instrument.clone(), instrument_spec_version.into())]);
    let execution_model = serde_json::json!({
        "runtime_version": runtime_version,
        "matching_kernel": crate::cases::kernels::BAR_MATCHING_KERNEL,
        "risk_rule_set_version": risk_rule_set_version,
        "account_id": effective_account_id,
        "model_descriptors": report.model_descriptors,
        "fill_model": report.fill_model_descriptor,
    });
    CanonicalRunSpec::new(
        "bar_backtest",
        code_revision,
        canonical_strategies,
        vec![dataset],
        market_rules,
        clock,
        spec.seed,
        behavior,
        execution_model,
        extensions,
    )
    .map_err(|error| internal_error(format!("Bar CanonicalRunSpec 无效: {error}")))
}

fn normalize_cross_language_values(value: &mut Value, path: &str) -> Result<(), AppError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(number) if number.is_f64() => Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!("{path} 含浮点值；CanonicalRunSpec 的精确数值必须使用十进制文本"),
        )),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                if integer.unsigned_abs() > 9_007_199_254_740_991 {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        format!("{path} 含跨语言不安全整数；请在 RunSpec 中用十进制文本表示"),
                    ));
                }
            } else if let Some(integer) = number.as_u64() {
                if integer > 9_007_199_254_740_991 {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        format!("{path} 含跨语言不安全整数；请在 RunSpec 中用十进制文本表示"),
                    ));
                }
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

fn internal_error(message: String) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}
