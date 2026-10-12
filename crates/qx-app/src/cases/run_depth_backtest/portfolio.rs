use crate::spec::{DepthBacktestSpec, PortfolioStrategySpec};
use crate::{AppError, AppErrorCategory};
use qx_core::{InstrumentId, Quantity};
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig, StrategyContext};
use qx_xingban::{DepthBarStrategy, OrderBookStrategySlot, PortfolioOrderBookStrategy};
use std::collections::BTreeMap;

pub(super) fn attach_portfolio_summary(
    summary: &mut serde_json::Value,
    spec: &DepthBacktestSpec,
    fills: &[qx_core::Fill],
) -> Result<(), AppError> {
    let Some(portfolio) = &spec.portfolio else {
        return Ok(());
    };
    let mut members = vec![(
        portfolio.primary_priority,
        spec.strategy.strategy_id.clone(),
    )];
    members.extend(
        portfolio
            .additional_strategies
            .iter()
            .map(|member| (member.priority, member.strategy.strategy_id.clone())),
    );
    members.sort();
    let mut fill_counts = members
        .iter()
        .map(|(_, strategy_id)| (strategy_id.clone(), 0_u64))
        .collect::<BTreeMap<_, _>>();
    for fill in fills {
        let strategy_id = fill.strategy_id.as_deref().ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "Tick/OrderBook 组合成交缺少 strategy_id 归属",
            )
        })?;
        let count = fill_counts.get_mut(strategy_id).ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("Tick/OrderBook 组合成交归属未知策略: {strategy_id}"),
            )
        })?;
        *count += 1;
    }
    let object = summary.as_object_mut().ok_or_else(|| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            "Tick/OrderBook 组合摘要不是 JSON 对象",
        )
    })?;
    object.insert(
        "strategy_ids".into(),
        serde_json::json!(members.iter().map(|(_, id)| id).collect::<Vec<_>>()),
    );
    object.insert("strategy_fills".into(), serde_json::json!(fill_counts));
    Ok(())
}

pub(super) fn build_portfolio_strategy(
    spec: &DepthBacktestSpec,
    instrument: &InstrumentId,
    account_id: &str,
    data_fingerprint: &str,
    as_of: u64,
) -> Result<PortfolioOrderBookStrategy, AppError> {
    let portfolio = spec.portfolio.as_ref().ok_or_else(|| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            "缺少 Tick/OrderBook 策略组合规格",
        )
    })?;
    let mut slots = Vec::with_capacity(portfolio.additional_strategies.len() + 1);
    let primary = &spec.strategy;
    let primary_strategy = build_member(
        primary,
        spec,
        instrument,
        account_id,
        data_fingerprint,
        as_of,
        portfolio.primary_reserved_capital_raw,
    )?;
    slots.push(OrderBookStrategySlot::new(
        portfolio.primary_priority,
        primary.strategy_id.clone(),
        portfolio.primary_reserved_capital_raw,
        primary_strategy,
    ));
    for member in &portfolio.additional_strategies {
        slots.push(build_slot(
            member,
            spec,
            instrument,
            account_id,
            data_fingerprint,
            as_of,
        )?);
    }
    PortfolioOrderBookStrategy::new(instrument.clone(), slots).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("Tick/OrderBook 策略组合无效: {error}"),
        )
    })
}

fn build_slot(
    member: &PortfolioStrategySpec,
    spec: &DepthBacktestSpec,
    instrument: &InstrumentId,
    account_id: &str,
    data_fingerprint: &str,
    as_of: u64,
) -> Result<OrderBookStrategySlot, AppError> {
    let strategy = build_member(
        &member.strategy,
        spec,
        instrument,
        account_id,
        data_fingerprint,
        as_of,
        member.reserved_capital_raw,
    )?;
    Ok(OrderBookStrategySlot::new(
        member.priority,
        member.strategy.strategy_id.clone(),
        member.reserved_capital_raw,
        strategy,
    ))
}

fn build_member(
    member: &crate::spec::BuiltinStrategySpec,
    spec: &DepthBacktestSpec,
    instrument: &InstrumentId,
    account_id: &str,
    data_fingerprint: &str,
    as_of: u64,
    reserved_capital_raw: i128,
) -> Result<DepthBarStrategy<BuiltinStrategy>, AppError> {
    let config = BuiltinStrategyConfig {
        fast_window: member.fast_window,
        slow_window: member.slow_window,
        period: member.period,
        threshold_bps: member.threshold_bps,
        target_notional_raw: member.target_notional_raw,
        primary_policy: spec.order_policy,
        ..BuiltinStrategyConfig::new(
            member.kind,
            member.strategy_id.clone(),
            instrument.clone(),
            Quantity::from_raw(member.quantity_raw),
        )
        .map_err(invalid_input)?
    };
    let strategy = BuiltinStrategy::new(config).map_err(invalid_input)?;
    let context = StrategyContext {
        strategy_id: member.strategy_id.clone(),
        strategy_version: format!("{}-v1", member.strategy_id),
        account_id: account_id.to_string(),
        venue_id: instrument.venue.to_string(),
        data_fingerprint: data_fingerprint.to_string(),
        as_of,
        positions: BTreeMap::new(),
        cash: BTreeMap::from([(spec.settlement_currency.clone(), reserved_capital_raw)]),
        available_margin_raw: Some(reserved_capital_raw),
        risk_state: "backtest".into(),
    };
    let mut adapter = DepthBarStrategy::new(strategy, context, instrument.clone());
    adapter.initialize().map_err(invalid_input)?;
    Ok(adapter)
}

fn invalid_input(error: impl std::fmt::Display) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, error.to_string())
}
