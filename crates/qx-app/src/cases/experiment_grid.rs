//! Shared bounded Cartesian-grid rules for every experiment target.
//!
//! This module only handles parameter expansion and strategy-field mutation. Candidate runs and
//! matching continue through their existing application use cases.

use super::run_experiment::{ExperimentParameterSpace, MAX_EXPERIMENT_CANDIDATES};
use crate::{AppError, AppErrorCategory, BuiltinStrategySpec};
use qx_strategy::builtin_signal::BuiltinSignalKnob;
use qx_xingban::MarketMakingConfig;
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
            "target_kelly_fraction_bps" => normalized.target_kelly_fraction_bps = Some(1),
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
        "target_kelly_fraction_bps",
    ];
    let mut names = BTreeSet::new();
    let mut total = 1_usize;
    for dimension in dimensions {
        if dimension.name == "quantity_raw"
            && (strategy.target_notional_raw.is_some()
                || strategy.target_available_margin_bps.is_some()
                || strategy.target_volatility_bps.is_some()
                || strategy.target_kelly_fraction_bps.is_some())
        {
            return Err(invalid(
                "quantity_raw 参数维度与策略 sizing 模式冲突：该策略的下单数量由 sizing 模式计算；请移除此维度或改用兼容的 sizing 参数",
            ));
        }
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
            "target_kelly_fraction_bps" => true,
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

pub(super) fn validate_depth_grid(
    strategy: &BuiltinStrategySpec,
    market_making: Option<MarketMakingConfig>,
    dimensions: &[ExperimentParameterSpace],
) -> Result<usize, AppError> {
    const MAKER_PARAMETERS: &[&str] = &[
        "market_making_spread_bps",
        "market_making_quote_qty_raw",
        "market_making_max_inventory_raw",
        "market_making_tick_size_raw",
        "market_making_inventory_skew_bps",
    ];
    let has_maker_parameter = dimensions
        .iter()
        .any(|dimension| MAKER_PARAMETERS.contains(&dimension.name.as_str()));
    if !has_maker_parameter {
        if market_making.is_some() {
            return Err(invalid(
                "做市深度实验必须至少包含一个 market_making_* 参数维度",
            ));
        }
        return validate_grid(strategy, dimensions);
    }
    if market_making.is_none() {
        return Err(invalid(
            "market_making_* 参数维度需要 base.market_making 配置",
        ));
    }
    let mut names = BTreeSet::new();
    let mut total = 1_usize;
    for dimension in dimensions {
        if !MAKER_PARAMETERS.contains(&dimension.name.as_str()) {
            return Err(invalid(
                "做市参数实验只接受 market_making_* 维度，不能与内置信号参数混用",
            ));
        }
        if !names.insert(dimension.name.as_str()) {
            return Err(invalid(format!("实验参数维度重复: {}", dimension.name)));
        }
        if dimension.values.is_empty()
            || dimension.values.iter().collect::<BTreeSet<_>>().len() != dimension.values.len()
        {
            return Err(invalid(format!(
                "实验参数维度 {} 候选不能为空且不能重复",
                dimension.name
            )));
        }
        for value in &dimension.values {
            let valid = match dimension.name.as_str() {
                "market_making_spread_bps" => (2..=10_000).contains(value),
                "market_making_inventory_skew_bps" => (0..=10_000).contains(value),
                _ => *value > 0,
            };
            if !valid {
                return Err(invalid(format!(
                    "实验参数 {}={} 超出做市参数范围",
                    dimension.name, value
                )));
            }
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
            "target_kelly_fraction_bps" => {
                strategy.target_kelly_fraction_bps =
                    Some(u32::try_from(*value).map_err(|_| invalid_parameter(name, *value))?)
            }
            _ => unreachable!("validate_grid closes parameter names"),
        }
    }
    Ok(())
}

pub(super) fn apply_depth_parameters(
    strategy: &mut BuiltinStrategySpec,
    market_making: &mut Option<MarketMakingConfig>,
    parameters: &BTreeMap<String, i128>,
) -> Result<(), AppError> {
    if parameters
        .keys()
        .any(|name| name.starts_with("market_making_"))
    {
        let config = market_making
            .as_mut()
            .ok_or_else(|| invalid("做市参数实验缺少 market_making 配置"))?;
        for (name, value) in parameters {
            match name.as_str() {
                "market_making_spread_bps" => {
                    config.spread_bps =
                        u32::try_from(*value).map_err(|_| invalid_parameter(name, *value))?
                }
                "market_making_quote_qty_raw" => config.quote_qty_raw = *value,
                "market_making_max_inventory_raw" => config.max_inventory_raw = *value,
                "market_making_tick_size_raw" => config.tick_size_raw = *value,
                "market_making_inventory_skew_bps" => {
                    config.inventory_skew_bps =
                        u32::try_from(*value).map_err(|_| invalid_parameter(name, *value))?
                }
                _ => unreachable!("validate_depth_grid closes market-making parameter names"),
            }
        }
        return Ok(());
    }
    apply_parameters(strategy, parameters)
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn invalid_parameter(name: &str, value: i128) -> AppError {
    invalid(format!("实验参数 {name}={value} 超出可表示范围"))
}
