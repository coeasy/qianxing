//! The single authoritative strategy-fill reduction path for Bar execution.

use super::{
    append_virtual_fill, strategy_order_status_update_from_order, BarStrategy, PortfolioBudgetState,
};
use crate::backtest::portfolio_budget::PortfolioBudgetMarket;
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
    pub(crate) portfolio_budget_markets:
        Option<&'a std::collections::BTreeMap<qx_core::InstrumentId, PortfolioBudgetMarket>>,
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
        fill.validate_exact_amounts()
            .map_err(QxError::BusinessViolation)?;
        if fill.exact_amounts.is_some() {
            spec.validate_exact_fill(fill.quantity_decimal(), fill.price_decimal())?;
        } else {
            spec.validate_fill(fill.qty.raw(), fill.price.raw())?;
        }
    }
    order.trace_fill(&mut fill, None, None);
    input
        .portfolio_budget
        .validate_fill_budget_with_exact_budget(
            |strategy_id| input.strategy.strategy_budget_exact(strategy_id),
            &order,
            &fill,
            input.instrument_spec,
            input.multiplier,
            input.margin,
            input.portfolio_budget_markets,
            input.currency,
        )?;
    let terms = qx_core::FillTerms::resolve(input.instrument_spec, input.multiplier);
    let entry_ids = apply_fill_to_books(input.ledger, input.oms, input.currency, &fill, terms)?;
    input
        .portfolio_budget
        .apply_fill_if_owned_with_exact_budget(
            |strategy_id| input.strategy.strategy_budget_exact(strategy_id),
            &order,
            &fill,
        )?;
    let fill_event_seq = append_virtual_fill(
        input.log,
        input.ledger,
        &fill,
        &entry_ids,
        qx_core::Priority::APPLY,
    )?;
    let _ = input.strategy.refresh(
        input.ledger,
        input.account_id,
        input.currency,
        input.cash_account,
    );
    input
        .strategy
        .on_fill_with_event_seq(&order, &fill, fill_event_seq)?;
    let committed_order = input
        .oms
        .get(order.client_id)
        .cloned()
        .ok_or_else(|| QxError::Invariant("成交后 OMS 订单消失".into()))?;
    if let Some(update) = strategy_order_status_update_from_order(
        &committed_order,
        fill_event_seq,
        fill.ts,
        committed_order.status,
        None,
    )? {
        input.strategy.on_order_status(&update)?;
    }
    input.fills.push(fill.clone());
    Ok((order, fill))
}
