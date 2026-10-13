//! The single authoritative strategy-fill reduction path for Bar execution.

use super::{append_virtual_fill, BarStrategy, PortfolioBudgetState};
use crate::cost::MarginRule;
use qx_core::{apply_fill_to_books, EventLog, Fill, Ledger, Order, QxError, TradingInstrumentSpec};
use qx_zhenlu::Oms;

pub(crate) struct FillApplicationInput<'a> {
    pub(crate) fill: Fill,
    pub(crate) instrument_spec: Option<&'a TradingInstrumentSpec>,
    pub(crate) cash_account: bool,
    pub(crate) multiplier: i128,
    pub(crate) strategy: &'a mut dyn BarStrategy,
    pub(crate) account_id: &'a str,
    pub(crate) currency: &'a str,
    pub(crate) ledger: &'a mut Ledger,
    pub(crate) oms: &'a mut Oms,
    pub(crate) portfolio_budget: &'a mut PortfolioBudgetState,
    pub(crate) margin: &'a dyn MarginRule,
    pub(crate) log: &'a mut EventLog,
    pub(crate) fills: &'a mut Vec<Fill>,
}

pub(crate) fn apply(input: FillApplicationInput<'_>) -> Result<(Order, Fill), QxError> {
    let mut fill = input.fill;
    let order = input
        .oms
        .get(fill.order_id)
        .cloned()
        .ok_or_else(|| QxError::ReconcileRequired("成交找不到订单".into()))?;
    if let Some(spec) = input.instrument_spec {
        spec.validate_fill(fill.qty.raw(), fill.price.raw())?;
    }
    order.trace_fill(&mut fill, None, None);
    input.portfolio_budget.validate_fill_budget(
        |strategy_id| input.strategy.strategy_budget_raw(strategy_id),
        &order,
        &fill,
        input.instrument_spec,
        input.multiplier,
        input.margin,
    )?;
    let terms = qx_core::FillTerms::resolve(input.instrument_spec, input.multiplier);
    let entry_ids = apply_fill_to_books(input.ledger, input.oms, input.currency, &fill, terms)?;
    input.portfolio_budget.apply_fill_if_owned(
        |strategy_id| input.strategy.strategy_budget_raw(strategy_id),
        &order,
        &fill,
    )?;
    append_virtual_fill(input.log, input.ledger, &fill, &entry_ids)?;
    let _ = input.strategy.refresh(
        input.ledger,
        input.account_id,
        input.currency,
        input.cash_account,
    );
    input.strategy.on_fill(&order, &fill)?;
    input.fills.push(fill.clone());
    Ok((order, fill))
}
