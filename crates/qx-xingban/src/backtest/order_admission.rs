//! Common account-level admission for Bar orders before matcher submission.

use super::{
    append_rejection, checked_abs, equity_for, notional_for, reduce_only_order_allowed,
    BarStrategy, PortfolioBudgetState,
};
use crate::{ashare::AshareRuleConfig, cost::MarginRule, venue::BarMatchingEngine};
use qx_core::{
    Event, EventKind, EventLog, InstrumentId, Ledger, Order, OrderStatus, PositionSide, Price,
    Priority, Side, TradingInstrumentSpec,
};
use qx_risk::OrderRiskPosition;
use qx_zhenlu::{Oms, RiskGate};
use std::collections::BTreeMap;

pub(crate) struct OrderAdmissionInput<'a> {
    pub(crate) orders: Vec<Order>,
    pub(crate) account_id: &'a str,
    pub(crate) instrument: &'a InstrumentId,
    pub(crate) currency: &'a str,
    pub(crate) ts: u64,
    pub(crate) position: i128,
    pub(crate) visible_close: i128,
    pub(crate) instrument_spec: Option<&'a TradingInstrumentSpec>,
    pub(crate) derivative_spec: Option<&'a TradingInstrumentSpec>,
    pub(crate) multiplier: i128,
    pub(crate) strategy: &'a dyn BarStrategy,
    pub(crate) ledger: &'a Ledger,
    pub(crate) oms: &'a mut Oms,
    pub(crate) portfolio_budget: &'a mut PortfolioBudgetState,
    pub(crate) margin: &'a dyn MarginRule,
    pub(crate) matcher: &'a BarMatchingEngine,
    /// Reservations held by other market matchers in a shared account.
    pub(crate) other_pending_funding_raw: i128,
    pub(crate) other_market_margin_raw: i128,
    /// Complete mark/spec snapshots for cross-market account risk valuation.
    pub(crate) portfolio_marks: Option<&'a BTreeMap<InstrumentId, Price>>,
    pub(crate) portfolio_specs: Option<&'a BTreeMap<InstrumentId, TradingInstrumentSpec>>,
    pub(crate) risk: &'a RiskGate,
    pub(crate) fx_rates: &'a BTreeMap<String, Price>,
    pub(crate) ashare_rules: Option<&'a AshareRuleConfig>,
    pub(crate) ashare_bought_today: i128,
    pub(crate) next_id: &'a mut u64,
    pub(crate) log: &'a mut EventLog,
}

pub(crate) struct AdmittedOrder {
    pub(crate) order: Order,
    pub(crate) funding_reservation_raw: i128,
}

pub(crate) fn admit_orders(
    input: OrderAdmissionInput<'_>,
) -> Result<Vec<AdmittedOrder>, qx_core::QxError> {
    let OrderAdmissionInput {
        orders,
        account_id,
        instrument,
        currency,
        ts,
        position,
        visible_close,
        instrument_spec,
        derivative_spec,
        multiplier,
        strategy,
        ledger,
        oms,
        portfolio_budget,
        margin,
        matcher,
        other_pending_funding_raw,
        other_market_margin_raw,
        portfolio_marks,
        portfolio_specs,
        risk,
        fx_rates,
        ashare_rules,
        ashare_bought_today,
        next_id,
        log,
    } = input;
    let mut reserved_funding_raw = 0_i128;
    let mut accepted_orders = Vec::new();
    for mut order in orders {
        if order.client_id == 0 || oms.get(order.client_id).is_some() {
            while *next_id == 0 || oms.get(*next_id).is_some() {
                *next_id = next_id
                    .checked_add(1)
                    .ok_or_else(|| qx_core::QxError::Invariant("回测订单序号溢出".into()))?;
            }
            order.client_id = *next_id;
            *next_id = next_id
                .checked_add(1)
                .ok_or_else(|| qx_core::QxError::Invariant("回测订单序号溢出".into()))?;
        } else if order.client_id >= *next_id {
            *next_id = order
                .client_id
                .checked_add(1)
                .ok_or_else(|| qx_core::QxError::Invariant("回测订单序号溢出".into()))?;
        }
        if order.account_id.is_empty() {
            order.account_id = account_id.to_string();
        }
        let invalid_reason = if order.account_id != account_id {
            Some("订单账户与回测账户不一致")
        } else if order.instrument != *instrument {
            Some("订单标的与回测标的不一致")
        } else {
            None
        };
        let ashare_reason = ashare_rules.and_then(|rules| {
            rules
                .validate_order(&order, position, ashare_bought_today, ts)
                .err()
        });
        if let Some(reason) = invalid_reason {
            append_rejection(log, ts, order.client_id, reason);
            continue;
        }
        if let Some(reason) = ashare_reason {
            append_rejection(log, ts, order.client_id, &reason);
            continue;
        }

        let product_policy = order.policy.unwrap_or_default();
        let leverage = if let Some(spec) = derivative_spec {
            product_policy.validate_for(spec)?;
            product_policy.leverage
        } else {
            1
        };
        // Decisions at Bar(t) only observe data through Bar(t-1), so risk and
        // funding use the last visible close rather than the current bar.
        let reference_price = order
            .limit
            .map(|price| price.raw())
            .unwrap_or(visible_close);
        let order_qty = checked_abs(order.qty.raw())?;
        let strategy_budget_check = portfolio_budget.order_budget_check(
            |strategy_id| strategy.strategy_budget_raw(strategy_id),
            &order,
            reference_price,
            instrument_spec,
            multiplier,
            margin,
        )?;
        let reduce_only_allowed = !product_policy.reduce_only
            || reduce_only_order_allowed(ledger, account_id, instrument, &order, order_qty);
        let required_margin = if let Some(spec) = derivative_spec {
            let signed_delta = match order.side {
                Side::Buy => order_qty,
                Side::Sell => -order_qty,
            };
            let projected_position = if product_policy.position_mode == qx_core::PositionMode::Hedge
            {
                ledger
                    .position_for_side(account_id, instrument, product_policy.position_side)
                    .quantity
                    .raw()
                    .checked_add(signed_delta)
                    .ok_or_else(|| {
                        qx_core::QxError::Invariant("回测 projected hedge position 溢出".into())
                    })?
            } else {
                position.checked_add(signed_delta).ok_or_else(|| {
                    qx_core::QxError::Invariant("回测 projected position 溢出".into())
                })?
            };
            let margin_qty = if product_policy.position_mode == qx_core::PositionMode::Hedge {
                let current_long = ledger
                    .position_for_side(account_id, instrument, PositionSide::Long)
                    .quantity
                    .raw();
                let current_short = ledger
                    .position_for_side(account_id, instrument, PositionSide::Short)
                    .quantity
                    .raw();
                let long = if product_policy.position_side == PositionSide::Long {
                    projected_position
                } else {
                    current_long
                };
                let short = if product_policy.position_side == PositionSide::Short {
                    projected_position
                } else {
                    current_short
                };
                checked_abs(long)?.saturating_add(checked_abs(short)?)
            } else {
                checked_abs(projected_position)?
            };
            margin.instrument_initial_margin(
                spec,
                margin_qty,
                checked_abs(reference_price)?,
                leverage,
            )?
        } else {
            margin.initial_margin(notional_for(
                derivative_spec,
                order_qty,
                checked_abs(reference_price)?,
                multiplier,
            )?)
        };
        let marks = BTreeMap::from([(instrument.clone(), Price::from_raw(visible_close))]);
        let equity = match (portfolio_marks, portfolio_specs) {
            (Some(marks), Some(specs)) => ledger
                .valuate(
                    &qx_core::ValuationContext::spot(account_id, marks, currency)
                        .with_market_specs(specs)
                        .with_fx(fx_rates),
                )?
                .equity
                .raw(),
            (None, None) => equity_for(
                ledger,
                account_id,
                &marks,
                currency,
                multiplier,
                derivative_spec,
                fx_rates,
            )?,
            _ => {
                return Err(qx_core::QxError::Invariant(
                    "共享账户风控估值必须同时提供 market marks 与 specs".into(),
                ));
            }
        };
        let cash_funded_spot_buy =
            derivative_spec.is_none() && order.side == Side::Buy && !product_policy.reduce_only;
        let required_funding = if cash_funded_spot_buy {
            let notional = notional_for(
                derivative_spec,
                order_qty,
                checked_abs(reference_price)?,
                multiplier,
            )?;
            notional
                .checked_add(matcher.estimate_fee(order_qty, checked_abs(reference_price)?, &order))
                .ok_or_else(|| qx_core::QxError::Invariant("买入所需现金溢出".into()))?
                .max(required_margin)
        } else {
            required_margin
        };
        let available_funding = if cash_funded_spot_buy {
            ledger.cash_for(account_id, currency)
        } else {
            equity
                .checked_sub(other_market_margin_raw)
                .unwrap_or(i128::MIN)
        };
        let total_reserved_funding = reserved_funding_raw
            .checked_add(matcher.pending_funding_reservation_raw())
            .and_then(|value| value.checked_add(other_pending_funding_raw))
            .unwrap_or(i128::MAX);
        let unreserved_funding = available_funding
            .checked_sub(total_reserved_funding)
            .unwrap_or(i128::MIN);
        if !reduce_only_allowed {
            append_rejection(
                log,
                ts,
                order.client_id,
                "reduce-only order would open or exceed the existing position",
            );
            continue;
        }
        if strategy_budget_check.is_some_and(|(budget, required)| required > budget) {
            append_rejection(
                log,
                ts,
                order.client_id,
                "订单会使策略持仓需求超过 reserved_capital_raw",
            );
            continue;
        }
        if required_funding > unreserved_funding {
            append_rejection(
                log,
                ts,
                order.client_id,
                if cash_funded_spot_buy {
                    "买入成本（含预估手续费）超过账户可用现金"
                } else {
                    "initial margin exceeds equity"
                },
            );
            continue;
        }

        let long_qty = ledger
            .position_for_side(account_id, instrument, PositionSide::Long)
            .quantity
            .raw();
        let short_qty = ledger
            .position_for_side(account_id, instrument, PositionSide::Short)
            .quantity
            .raw();
        let one_way_qty = position
            .checked_sub(long_qty)
            .and_then(|value| value.checked_sub(short_qty))
            .ok_or_else(|| {
                qx_core::QxError::Invariant("拆分回测 one-way/hedge 持仓数量溢出".into())
            })?;
        let mark_abs = checked_abs(visible_close)?;
        let one_way_notional = notional_for(
            derivative_spec,
            checked_abs(one_way_qty)?,
            mark_abs,
            multiplier,
        )?;
        let long_notional = notional_for(
            derivative_spec,
            checked_abs(long_qty)?,
            mark_abs,
            multiplier,
        )?;
        let short_notional = notional_for(
            derivative_spec,
            checked_abs(short_qty)?,
            mark_abs,
            multiplier,
        )?;
        let filled_gross_notional = one_way_notional
            .checked_add(long_notional)
            .and_then(|value| value.checked_add(short_notional))
            .ok_or_else(|| qx_core::QxError::Invariant("回测 hedge gross notional 溢出".into()))?;
        let pending_gross_notional = if risk.rule_set().has_rule("MaxNotional") {
            if let Some(spec) = instrument_spec {
                qx_risk::pending_order_gross_notional(
                    &oms.all_orders(),
                    order.client_id,
                    account_id,
                    instrument,
                    spec,
                    &marks,
                )?
            } else {
                0
            }
        } else {
            0
        };
        let gross_notional = filled_gross_notional
            .checked_add(pending_gross_notional)
            .ok_or_else(|| {
                qx_core::QxError::Invariant("回测持仓与在途订单名义额合计溢出".into())
            })?;
        if matches!(order.status, OrderStatus::PendingSubmit) {
            order.status = OrderStatus::Submitted;
        }
        let risk_position =
            OrderRiskPosition::new_with_multiplier(one_way_qty, gross_notional, multiplier)
                .with_hedge_legs(long_qty, short_qty);
        if let Err(error) = risk.check_with_spec(
            &order,
            &risk_position,
            Some(Price::from_raw(reference_price)),
            instrument_spec,
        ) {
            append_rejection(log, ts, order.client_id, &error.to_string());
            continue;
        }
        if let Err(error) = oms.submit(order.clone()) {
            append_rejection(log, ts, order.client_id, &error.to_string());
            continue;
        }
        if let Err(error) = oms.accept(order.client_id) {
            append_rejection(log, ts, order.client_id, &error.to_string());
            continue;
        }
        portfolio_budget.accept_if_owned(
            |strategy_id| strategy.strategy_budget_raw(strategy_id),
            &order,
            reference_price,
        )?;
        reserved_funding_raw = reserved_funding_raw
            .checked_add(required_funding)
            .ok_or_else(|| qx_core::QxError::Invariant("同 Bar 组合资金预留溢出".into()))?;
        let submit_seq = log.alloc_seq();
        log.append(Event::new(
            submit_seq,
            ts,
            Priority::COMMAND,
            EventKind::Submit {
                client_order_id: order.client_id,
            },
        ));
        accepted_orders.push(AdmittedOrder {
            order,
            funding_reservation_raw: required_funding,
        });
    }
    Ok(accepted_orders)
}
