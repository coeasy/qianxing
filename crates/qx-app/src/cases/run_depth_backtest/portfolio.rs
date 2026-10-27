use super::custom_strategy::{CallbackOrderBookStrategy, InjectedDepthStrategy};
use crate::spec::{DepthBacktestSpec, PortfolioStrategySpec};
use crate::{AppError, AppErrorCategory};
use qx_core::{InstrumentId, Quantity};
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig, StrategyContext};
use qx_xingban::{
    DepthBarStrategy, OrderBookStrategy, OrderBookStrategySlot, PortfolioOrderBookStrategy,
};
use std::collections::BTreeMap;

pub(super) fn strategy_specs(
    spec: &DepthBacktestSpec,
) -> impl Iterator<Item = &crate::BuiltinStrategySpec> {
    std::iter::once(&spec.strategy).chain(
        spec.portfolio
            .iter()
            .flat_map(|portfolio| portfolio.additional_strategies.iter())
            .map(|member| &member.strategy),
    )
}

pub(super) fn has_builtin_sizing(strategy: &crate::BuiltinStrategySpec) -> bool {
    strategy.target_notional_raw.is_some()
        || strategy.target_available_margin_bps.is_some()
        || strategy.target_volatility_bps.is_some()
        || strategy.target_kelly_fraction_bps.is_some()
}

struct DepthPortfolioBuildContext<'a> {
    spec: &'a DepthBacktestSpec,
    instrument: &'a InstrumentId,
    account_id: &'a str,
    data_fingerprint: &'a str,
    as_of: u64,
}

pub(super) fn build_portfolio_strategy(
    spec: &DepthBacktestSpec,
    instrument: &InstrumentId,
    account_id: &str,
    data_fingerprint: &str,
    as_of: u64,
    injected: Vec<InjectedDepthStrategy>,
) -> Result<PortfolioOrderBookStrategy, AppError> {
    let portfolio = spec.portfolio.as_ref().ok_or_else(|| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            "缺少 Tick/OrderBook 策略组合规格",
        )
    })?;
    let mut injected = injected
        .into_iter()
        .map(|item| (item.identity.strategy_id.clone(), item))
        .collect::<BTreeMap<_, _>>();
    let build_context = DepthPortfolioBuildContext {
        spec,
        instrument,
        account_id,
        data_fingerprint,
        as_of,
    };
    let mut slots = Vec::with_capacity(portfolio.additional_strategies.len() + 1);
    let primary = &spec.strategy;
    let primary_reserved_capital_exact = portfolio
        .primary_reserved_capital()
        .map_err(|message| AppError::new(AppErrorCategory::InvalidInput, message))?;
    let primary_strategy = build_member(
        primary,
        &build_context,
        portfolio.primary_reserved_capital_raw,
        primary_reserved_capital_exact,
        injected.remove(&primary.strategy_id),
    )?;
    slots.push(
        OrderBookStrategySlot::from_box(
            portfolio.primary_priority,
            primary.strategy_id.clone(),
            portfolio.primary_reserved_capital_raw,
            primary_strategy,
        )
        .with_reserved_capital_exact(primary_reserved_capital_exact),
    );
    for member in &portfolio.additional_strategies {
        slots.push(build_slot(member, &build_context, &mut injected)?);
    }
    if let Some((strategy_id, _)) = injected.into_iter().next() {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("未消费的 Tick/OrderBook 自定义策略槽: {strategy_id}"),
        ));
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
    context: &DepthPortfolioBuildContext<'_>,
    injected: &mut BTreeMap<String, InjectedDepthStrategy>,
) -> Result<OrderBookStrategySlot, AppError> {
    let reserved_capital_exact = member
        .reserved_capital()
        .map_err(|message| AppError::new(AppErrorCategory::InvalidInput, message))?;
    let strategy = build_member(
        &member.strategy,
        context,
        member.reserved_capital_raw,
        reserved_capital_exact,
        injected.remove(&member.strategy.strategy_id),
    )?;
    Ok(OrderBookStrategySlot::from_box(
        member.priority,
        member.strategy.strategy_id.clone(),
        member.reserved_capital_raw,
        strategy,
    )
    .with_reserved_capital_exact(reserved_capital_exact))
}

fn build_member(
    member: &crate::spec::BuiltinStrategySpec,
    build: &DepthPortfolioBuildContext<'_>,
    reserved_capital_raw: i128,
    reserved_capital_exact: qx_core::ExactDecimal,
    injected: Option<InjectedDepthStrategy>,
) -> Result<Box<dyn OrderBookStrategy>, AppError> {
    let cash = BTreeMap::from([(build.spec.settlement_currency.clone(), reserved_capital_raw)]);
    let context = StrategyContext {
        strategy_id: member.strategy_id.clone(),
        strategy_version: injected
            .as_ref()
            .map(|item| item.identity.strategy_version.clone())
            .unwrap_or_else(|| format!("{}-v1", member.strategy_id)),
        account_id: build.account_id.to_string(),
        venue_id: build.instrument.venue.to_string(),
        data_fingerprint: build.data_fingerprint.to_string(),
        as_of: build.as_of,
        positions: BTreeMap::new(),
        available_margin_raw: crate::cases::strategy_available_margin_raw(
            &cash,
            &build.spec.settlement_currency,
        ),
        cash,
        risk_state: "backtest".into(),
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::from([(
            build.spec.settlement_currency.clone(),
            reserved_capital_exact,
        )]),
        available_margin_exact: Some(reserved_capital_exact),
    };
    if let Some(injected) = injected {
        return CallbackOrderBookStrategy::new(
            injected.strategy,
            context,
            build.instrument.clone(),
            build.spec.tier.clone(),
        )
        .map(|strategy| Box::new(strategy) as Box<dyn OrderBookStrategy>)
        .map_err(invalid_input);
    }
    let config = BuiltinStrategyConfig {
        fast_window: member.fast_window,
        slow_window: member.slow_window,
        period: member.period,
        threshold_bps: member.threshold_bps,
        target_notional_raw: member.target_notional_raw,
        target_available_margin_bps: member.target_available_margin_bps,
        target_volatility_bps: member.target_volatility_bps,
        target_kelly_fraction_bps: member.target_kelly_fraction_bps,
        kelly_lookback_bars: member.kelly_lookback_bars,
        primary_policy: build.spec.order_policy,
        ..BuiltinStrategyConfig::new(
            member.kind,
            member.strategy_id.clone(),
            build.instrument.clone(),
            Quantity::from_raw(member.quantity_raw),
        )
        .map_err(invalid_input)?
    };
    let strategy = BuiltinStrategy::new(config).map_err(invalid_input)?;
    let mut adapter = DepthBarStrategy::new(strategy, context, build.instrument.clone());
    adapter.initialize().map_err(invalid_input)?;
    Ok(Box::new(adapter))
}

fn invalid_input(error: impl std::fmt::Display) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, error.to_string())
}
