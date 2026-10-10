//! Shared bounded Cartesian-grid rules for every experiment target.
//!
//! This module only handles parameter expansion and strategy-field mutation. Candidate runs and
//! matching continue through their existing application use cases.

use super::run_experiment::{ExperimentParameterSpace, MAX_EXPERIMENT_CANDIDATES};
use crate::{AppError, AppErrorCategory, BuiltinStrategySpec};
use qx_strategy::builtin_signal::BuiltinSignalKnob;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn normalized_strategy(
    strategy: &BuiltinStrategySpec,
    dimensions: &[ExperimentParameterSpace],
) -> BuiltinStrategySpec {
    let mut normalized = strategy.clone();
    for dimension in dimensions {
        match dimension.name.as_str() {
            "fast_window" => normalized.fast_window = 1,
            "slow_window" => normalized.slow_window = 2,
            "period" => normalized.period = 2,
            "threshold_bps" => normalized.threshold_bps = 0,
            "quantity_raw" => normalized.quantity_raw = 1,
            _ => {}
        }
    }
    normalized
}

pub(super) fn validate_experiment_id(experiment_id: &str) -> Result<(), AppError> {
    if experiment_id.is_empty()
        || experiment_id.len() > 96
        || matches!(experiment_id, "." | "..")
        || !experiment_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(invalid(
            "experiment_id 必须是最多 96 字符的 [A-Za-z0-9._-] 标识",
        ));
    }
    Ok(())
}

pub(super) fn validate_grid(
    strategy: &BuiltinStrategySpec,
    dimensions: &[ExperimentParameterSpace],
) -> Result<usize, AppError> {
    if dimensions.is_empty() {
        return Err(invalid("参数实验至少需要一个参数维度"));
    }
    let supported = [
        "fast_window",
        "slow_window",
        "period",
        "threshold_bps",
        "quantity_raw",
    ];
    let mut names = BTreeSet::new();
    let mut total = 1_usize;
    for dimension in dimensions {
        if !supported.contains(&dimension.name.as_str()) {
            return Err(invalid(format!("不支持的策略参数维度: {}", dimension.name)));
        }
        if !names.insert(dimension.name.as_str()) {
            return Err(invalid(format!("实验参数维度重复: {}", dimension.name)));
        }
        let applies = match dimension.name.as_str() {
            "fast_window" => strategy
                .kind
                .uses_signal_knob(BuiltinSignalKnob::FastWindow),
            "slow_window" => strategy
                .kind
                .uses_signal_knob(BuiltinSignalKnob::SlowWindow),
            "period" => strategy.kind.uses_signal_knob(BuiltinSignalKnob::Period),
            "threshold_bps" => strategy
                .kind
                .uses_signal_knob(BuiltinSignalKnob::ThresholdBps),
            "quantity_raw" => true,
            _ => false,
        };
        if !applies {
            return Err(invalid(format!(
                "参数维度 {} 不适用于策略 {}",
                dimension.name,
                strategy.kind.name()
            )));
        }
        if dimension.values.is_empty() {
            return Err(invalid(format!("参数维度 {} 没有候选值", dimension.name)));
        }
        if dimension.values.iter().collect::<BTreeSet<_>>().len() != dimension.values.len() {
            return Err(invalid(format!("参数维度 {} 含重复候选值", dimension.name)));
        }
        total = total
            .checked_mul(dimension.values.len())
            .ok_or_else(|| invalid("实验候选数溢出"))?;
        if total > MAX_EXPERIMENT_CANDIDATES {
            return Err(invalid(format!(
                "实验最多接受 {MAX_EXPERIMENT_CANDIDATES} 个候选组合"
            )));
        }
    }
    Ok(total)
}

pub(super) fn expand_grid(dimensions: &[ExperimentParameterSpace]) -> Vec<BTreeMap<String, i128>> {
    let mut sorted = dimensions.to_vec();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut combinations = vec![BTreeMap::new()];
    for dimension in &sorted {
        let mut expanded = Vec::with_capacity(combinations.len() * dimension.values.len());
        for combination in &combinations {
            for value in &dimension.values {
                let mut next = combination.clone();
                next.insert(dimension.name.clone(), *value);
                expanded.push(next);
            }
        }
        combinations = expanded;
    }
    combinations
}

pub(super) fn apply_parameters(
    strategy: &mut BuiltinStrategySpec,
    parameters: &BTreeMap<String, i128>,
) -> Result<(), AppError> {
    for (name, value) in parameters {
        match name.as_str() {
            "fast_window" => {
                strategy.fast_window =
                    usize::try_from(*value).map_err(|_| invalid_parameter(name, *value))?
            }
            "slow_window" => {
                strategy.slow_window =
                    usize::try_from(*value).map_err(|_| invalid_parameter(name, *value))?
            }
            "period" => {
                strategy.period =
                    usize::try_from(*value).map_err(|_| invalid_parameter(name, *value))?
            }
            "threshold_bps" => strategy.threshold_bps = *value,
            "quantity_raw" => strategy.quantity_raw = *value,
            _ => unreachable!("validate_grid closes parameter names"),
        }
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn invalid_parameter(name: &str, value: i128) -> AppError {
    invalid(format!("实验参数 {name}={value} 超出可表示范围"))
}
