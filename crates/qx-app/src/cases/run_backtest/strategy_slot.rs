//! Assemble native built-ins as deterministic portfolio slots.

use super::invalid_strategy;
use crate::cases::strategy_available_margin_raw;
use crate::error::AppError;
use crate::spec::BuiltinStrategySpec;
use qx_core::{InstrumentId, Quantity};
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig, Strategy, StrategyContext};
use qx_xingban::{NativeBarStrategy, PortfolioStrategySlot};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(in crate::cases) struct StrategySlotContext<'a> {
    pub priority: i32,
    pub reserved_capital_raw: i128,
    pub reserved_capital_exact: qx_core::ExactDecimal,
    pub instrument: &'a InstrumentId,
    pub account_id: &'a str,
    pub currency: &'a str,
    pub data_fingerprint: &'a str,
    pub as_of: u64,
    pub strategy_version: Option<&'a str>,
}

pub(in crate::cases) struct BoxedStrategy(Box<dyn Strategy>);

impl Strategy for BoxedStrategy {
    fn on_init(&mut self, context: &StrategyContext) -> Result<(), String> {
        self.0.on_init(context)
    }

    fn on_start(&mut self, context: &StrategyContext) -> Result<(), String> {
        self.0.on_start(context)
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &qx_strategy::MarketEvent,
    ) -> Result<qx_strategy::StrategyDecision, String> {
        self.0.on_event(context, event)
    }

    fn on_order_update(
        &mut self,
        context: &StrategyContext,
        update: &str,
    ) -> Result<Option<qx_strategy::StrategyDecision>, String> {
        self.0.on_order_update(context, update)
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.0.on_stop()
    }

    fn on_stop_with_context(
        &mut self,
        stop: &qx_strategy::StrategyStopContextV1,
    ) -> Result<(), String> {
        self.0.on_stop_with_context(stop)
    }

    fn diagnostic_state(&self) -> Option<serde_json::Value> {
        self.0.diagnostic_state()
    }
}

pub(in crate::cases) fn build_strategy_slot(
    spec: &BuiltinStrategySpec,
    context: StrategySlotContext<'_>,
    external: Option<Box<dyn Strategy>>,
) -> Result<PortfolioStrategySlot, AppError> {
    let (strategy_id, native) = build_native_strategy(spec, context, external)?;
    let slot = PortfolioStrategySlot::native(
        context.priority,
        strategy_id,
        context.reserved_capital_raw,
        native,
    );
    Ok(slot.with_reserved_capital_exact(context.reserved_capital_exact))
}

pub(in crate::cases) fn build_single_native_strategy(
    spec: &BuiltinStrategySpec,
    context: StrategySlotContext<'_>,
) -> Result<NativeBarStrategy<BoxedStrategy>, AppError> {
    build_native_strategy(spec, context, None).map(|(_, native)| native)
}

fn build_native_strategy(
    spec: &BuiltinStrategySpec,
    context: StrategySlotContext<'_>,
    external: Option<Box<dyn Strategy>>,
) -> Result<(String, NativeBarStrategy<BoxedStrategy>), AppError> {
    let strategy_config = BuiltinStrategyConfig::new(
        spec.kind,
        spec.strategy_id.clone(),
        context.instrument.clone(),
        Quantity::from_raw(spec.quantity_raw),
    )
    .map_err(invalid_strategy)?;
    let mut strategy_config = BuiltinStrategyConfig {
        fast_window: spec.fast_window,
        slow_window: spec.slow_window,
        period: spec.period,
        threshold_bps: spec.threshold_bps,
        ..strategy_config
    };
    strategy_config.target_notional_raw = spec.target_notional_raw;
    strategy_config.target_available_margin_bps = spec.target_available_margin_bps;
    strategy_config.target_volatility_bps = spec.target_volatility_bps;
    strategy_config.target_kelly_fraction_bps = spec.target_kelly_fraction_bps;
    strategy_config.kelly_lookback_bars = spec.kelly_lookback_bars;
    strategy_config.strategy_version = context
        .strategy_version
        .map(str::to_string)
        .unwrap_or_else(|| strategy_version(spec));
    strategy_config.validate().map_err(invalid_strategy)?;
    let budget_exact = context.reserved_capital_exact;
    let cash = BTreeMap::from([(context.currency.to_string(), context.reserved_capital_raw)]);
    let strategy_context = StrategyContext {
        strategy_id: strategy_config.strategy_id.clone(),
        strategy_version: strategy_config.strategy_version.clone(),
        account_id: context.account_id.to_string(),
        venue_id: context.instrument.venue.to_string(),
        data_fingerprint: context.data_fingerprint.to_string(),
        as_of: context.as_of,
        positions_exact: std::collections::BTreeMap::new(),
        cash_exact: BTreeMap::from([(context.currency.to_string(), budget_exact)]),
        available_margin_exact: Some(budget_exact),
        positions: BTreeMap::new(),
        available_margin_raw: strategy_available_margin_raw(&cash, context.currency),
        cash,
        risk_state: "backtest".into(),
    };
    let strategy_id = strategy_config.strategy_id.clone();
    let strategy = match external {
        Some(strategy) => BoxedStrategy(strategy),
        None => BoxedStrategy(Box::new(
            BuiltinStrategy::new(strategy_config).map_err(invalid_strategy)?,
        )),
    };
    let native = NativeBarStrategy::new(strategy, strategy_context);
    Ok((strategy_id, native))
}

pub(in crate::cases) fn strategy_version(spec: &BuiltinStrategySpec) -> String {
    if spec.target_kelly_fraction_bps.is_some() {
        format!("builtin-{}-kelly-v1", spec.kind.name())
    } else if spec.target_volatility_bps.is_some() {
        format!("builtin-{}-atr-budget-v1", spec.kind.name())
    } else if spec.target_available_margin_bps.is_some() {
        format!("builtin-{}-cash-fraction-v1", spec.kind.name())
    } else if spec.target_notional_raw.is_some() {
        format!("builtin-{}-notional-v1", spec.kind.name())
    } else {
        format!("builtin-{}-v1", spec.kind.name())
    }
}
