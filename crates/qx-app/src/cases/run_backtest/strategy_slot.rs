//! Assemble native built-ins as deterministic portfolio slots.

use super::invalid_strategy;
use crate::error::AppError;
use crate::spec::BuiltinStrategySpec;
use qx_core::{InstrumentId, Quantity};
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig, Strategy, StrategyContext};
use qx_xingban::{NativeBarStrategy, PortfolioStrategySlot};
use std::collections::BTreeMap;

pub(super) struct StrategySlotContext<'a> {
    pub priority: i32,
    pub reserved_capital_raw: i128,
    pub instrument: &'a InstrumentId,
    pub account_id: &'a str,
    pub currency: &'a str,
    pub data_fingerprint: &'a str,
    pub as_of: u64,
    pub strategy_version: Option<&'a str>,
}

struct BoxedStrategy(Box<dyn Strategy>);

impl Strategy for BoxedStrategy {
    fn on_init(&mut self, context: &StrategyContext) -> Result<(), String> {
        self.0.on_init(context)
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
}

pub(super) fn build_strategy_slot(
    spec: &BuiltinStrategySpec,
    context: StrategySlotContext<'_>,
    external: Option<Box<dyn Strategy>>,
) -> Result<PortfolioStrategySlot, AppError> {
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
    let strategy_context = StrategyContext {
        strategy_id: strategy_config.strategy_id.clone(),
        strategy_version: strategy_config.strategy_version.clone(),
        account_id: context.account_id.to_string(),
        venue_id: context.instrument.venue.to_string(),
        data_fingerprint: context.data_fingerprint.to_string(),
        as_of: context.as_of,
        positions: BTreeMap::new(),
        cash: BTreeMap::from([(context.currency.to_string(), context.reserved_capital_raw)]),
        available_margin_raw: Some(context.reserved_capital_raw),
        risk_state: "backtest".into(),
    };
    let strategy_id = strategy_config.strategy_id.clone();
    let strategy = match external {
        Some(strategy) => BoxedStrategy(strategy),
        None => BoxedStrategy(Box::new(
            BuiltinStrategy::new(strategy_config).map_err(invalid_strategy)?,
        )),
    };
    let mut native = NativeBarStrategy::new(strategy, strategy_context);
    native
        .initialize()
        .map_err(|error| AppError::from_qx_error(&error))?;
    Ok(PortfolioStrategySlot::native(
        context.priority,
        strategy_id,
        context.reserved_capital_raw,
        native,
    ))
}

pub(super) fn strategy_version(spec: &BuiltinStrategySpec) -> String {
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
