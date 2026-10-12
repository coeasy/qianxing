//! Strategy-owned position and pending-order exposure for portfolio capital caps.

use crate::cost::MarginRule;
use qx_core::{Fill, Order, QxError, Side, TradingInstrumentSpec};
use std::collections::BTreeMap;

#[derive(Clone)]
struct PendingExposure {
    strategy_id: String,
    signed_qty: i128,
    reference_price: i128,
    leverage: u32,
}

#[derive(Default)]
pub(crate) struct PortfolioBudgetState {
    positions: BTreeMap<String, i128>,
    pending_orders: BTreeMap<u64, PendingExposure>,
}

impl PortfolioBudgetState {
    pub(crate) fn position(&self, strategy_id: &str) -> i128 {
        self.positions.get(strategy_id).copied().unwrap_or_default()
    }

    pub(crate) fn order_budget_check(
        &self,
        budget_for: impl Fn(&str) -> Option<i128>,
        order: &Order,
        reference_price: i128,
        instrument_spec: Option<&TradingInstrumentSpec>,
        multiplier: i128,
        margin: &dyn MarginRule,
    ) -> Result<Option<(i128, i128)>, QxError> {
        let Some(strategy_id) = strategy_id(order) else {
            return Ok(None);
        };
        let Some(budget) = budget_for(strategy_id) else {
            return Ok(None);
        };

        let leverage = order.policy.unwrap_or_default().leverage;
        let mut position = self.positions.get(strategy_id).copied().unwrap_or_default();
        let mut maximum_required = required_capital_raw(
            position,
            reference_price,
            instrument_spec,
            multiplier,
            leverage,
            margin,
        )?;
        for pending in self
            .pending_orders
            .values()
            .filter(|pending| pending.strategy_id == strategy_id)
        {
            position = position
                .checked_add(pending.signed_qty)
                .ok_or_else(|| QxError::Invariant("组合策略挂单预计持仓溢出".into()))?;
            maximum_required = maximum_required.max(required_capital_raw(
                position,
                pending.reference_price,
                instrument_spec,
                multiplier,
                pending.leverage,
                margin,
            )?);
        }
        position = position
            .checked_add(signed_qty(order.side, order.qty.raw())?)
            .ok_or_else(|| QxError::Invariant("组合策略预计持仓溢出".into()))?;
        maximum_required = maximum_required.max(required_capital_raw(
            position,
            reference_price,
            instrument_spec,
            multiplier,
            leverage,
            margin,
        )?);
        Ok(Some((budget, maximum_required)))
    }

    pub(crate) fn validate_fill_budget(
        &self,
        budget_for: impl Fn(&str) -> Option<i128>,
        order: &Order,
        fill: &Fill,
        instrument_spec: Option<&TradingInstrumentSpec>,
        multiplier: i128,
        margin: &dyn MarginRule,
    ) -> Result<(), QxError> {
        let Some(strategy_id) = strategy_id(order) else {
            return Ok(());
        };
        let Some(budget) = budget_for(strategy_id) else {
            return Ok(());
        };

        let leverage = order.policy.unwrap_or_default().leverage;
        let fill_delta = signed_qty(order.side, fill.qty.raw())?;
        let mut position = self
            .positions
            .get(strategy_id)
            .copied()
            .unwrap_or_default()
            .checked_add(fill_delta)
            .ok_or_else(|| QxError::Invariant("组合策略成交后持仓溢出".into()))?;
        let mut maximum_required = required_capital_raw(
            position,
            fill.price.raw(),
            instrument_spec,
            multiplier,
            leverage,
            margin,
        )?;

        for (order_id, pending) in &self.pending_orders {
            if pending.strategy_id != strategy_id {
                continue;
            }
            let remaining = if *order_id == order.client_id {
                pending
                    .signed_qty
                    .checked_sub(fill_delta)
                    .ok_or_else(|| QxError::Invariant("组合策略剩余挂单数量溢出".into()))?
            } else {
                pending.signed_qty
            };
            if remaining == 0 {
                continue;
            }
            position = position
                .checked_add(remaining)
                .ok_or_else(|| QxError::Invariant("组合策略剩余挂单持仓溢出".into()))?;
            maximum_required = maximum_required.max(required_capital_raw(
                position,
                pending.reference_price,
                instrument_spec,
                multiplier,
                pending.leverage,
                margin,
            )?);
        }
        if maximum_required > budget {
            return Err(QxError::BusinessViolation(format!(
                "成交价使策略持仓需求超过 reserved_capital_raw: required={maximum_required}, budget={budget}"
            )));
        }
        Ok(())
    }

    pub(crate) fn accept_if_owned(
        &mut self,
        budget_for: impl Fn(&str) -> Option<i128>,
        order: &Order,
        reference_price: i128,
    ) -> Result<(), QxError> {
        if let Some(owner) =
            strategy_id(order).filter(|strategy_id| budget_for(strategy_id).is_some())
        {
            let owner = owner.to_string();
            if self.pending_orders.contains_key(&order.client_id) {
                return Err(QxError::Invariant("组合预算重复登记同一挂单 ID".into()));
            }
            self.pending_orders.insert(
                order.client_id,
                PendingExposure {
                    strategy_id: owner,
                    signed_qty: signed_qty(order.side, order.qty.raw())?,
                    reference_price,
                    leverage: order.policy.unwrap_or_default().leverage,
                },
            );
        }
        Ok(())
    }

    pub(crate) fn apply_fill_if_owned(
        &mut self,
        budget_for: impl Fn(&str) -> Option<i128>,
        order: &Order,
        fill: &Fill,
    ) -> Result<(), QxError> {
        if strategy_id(order).is_some_and(|strategy_id| budget_for(strategy_id).is_some()) {
            self.apply_fill(order, fill)?;
        }
        Ok(())
    }

    pub(crate) fn cancel_if_owned(
        &mut self,
        budget_for: impl Fn(&str) -> Option<i128>,
        client_order_id: u64,
    ) -> Result<(), QxError> {
        if let Some(pending) = self.pending_orders.get(&client_order_id) {
            if budget_for(&pending.strategy_id).is_some() {
                self.pending_orders.remove(&client_order_id);
            }
        }
        Ok(())
    }

    fn apply_fill(&mut self, order: &Order, fill: &Fill) -> Result<(), QxError> {
        let Some(owner) = strategy_id(order) else {
            return Ok(());
        };
        let fill_delta = signed_qty(order.side, fill.qty.raw())?;
        let pending = self
            .pending_orders
            .get(&order.client_id)
            .ok_or_else(|| QxError::ReconcileRequired("组合策略成交没有对应的预算挂单".into()))?;
        if pending.strategy_id != owner || pending.signed_qty.signum() != fill_delta.signum() {
            return Err(QxError::ReconcileRequired(
                "组合策略预算挂单归属或方向与成交不一致".into(),
            ));
        }
        let new_position = self
            .positions
            .get(owner)
            .copied()
            .unwrap_or_default()
            .checked_add(fill_delta)
            .ok_or_else(|| QxError::Invariant("组合策略成交持仓溢出".into()))?;
        self.positions.insert(owner.to_string(), new_position);
        let pending = self
            .pending_orders
            .get_mut(&order.client_id)
            .ok_or_else(|| QxError::ReconcileRequired("组合策略预算挂单在状态更新前消失".into()))?;
        pending.signed_qty = pending
            .signed_qty
            .checked_sub(fill_delta)
            .ok_or_else(|| QxError::Invariant("组合策略剩余挂单数量溢出".into()))?;
        if pending.signed_qty == 0 {
            self.pending_orders.remove(&order.client_id);
        }
        Ok(())
    }
}

fn strategy_id(order: &Order) -> Option<&str> {
    order
        .trace
        .as_ref()
        .and_then(|trace| trace.strategy_id.as_deref())
}

fn required_capital_raw(
    position: i128,
    reference_price: i128,
    instrument_spec: Option<&TradingInstrumentSpec>,
    multiplier: i128,
    leverage: u32,
    margin: &dyn MarginRule,
) -> Result<i128, QxError> {
    let qty = position
        .checked_abs()
        .ok_or_else(|| QxError::Invariant("组合策略持仓绝对值溢出".into()))?;
    let price = reference_price
        .checked_abs()
        .ok_or_else(|| QxError::Invariant("组合策略参考价格绝对值溢出".into()))?;
    if qty == 0 {
        return Ok(0);
    }
    if let Some(spec) = instrument_spec.filter(|spec| spec.product.supports_leverage()) {
        margin.instrument_initial_margin(spec, qty, price, leverage)
    } else if let Some(spec) = instrument_spec {
        spec.notional(qty, price)
    } else {
        Ok(qx_core::notional(qty, price).saturating_mul(multiplier))
    }
}

fn signed_qty(side: Side, qty: i128) -> Result<i128, QxError> {
    if qty <= 0 {
        return Err(QxError::BusinessViolation(
            "组合策略订单与成交数量必须为正".into(),
        ));
    }
    match side {
        Side::Buy => Ok(qty),
        Side::Sell => qty
            .checked_neg()
            .ok_or_else(|| QxError::Invariant("组合策略卖出数量溢出".into())),
    }
}
