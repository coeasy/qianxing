//! Common account-level admission for Bar orders before matcher submission.

use super::{
    append_rejection, checked_abs, equity_for, notional_for, reduce_only_order_allowed,
    BarStrategy, PortfolioBudgetState,
};
use crate::backtest::portfolio_budget::PortfolioBudgetMarket;
use crate::{ashare::AshareRuleConfig, cost::MarginRule, venue::BarMatchingEngine};
use qx_core::{
    DecimalRounding, Event, EventContext, EventKind, EventLog, EventMetadata, ExactDecimal,
    InstrumentId, Ledger, Order, OrderRiskDecisionFact, OrderStatus, PositionSide, Price, Priority,
    Side, TradingInstrumentSpec,
};
use qx_risk::{ExactOrderRiskFacts, ExactOrderRiskPosition, OrderRiskContext, OrderRiskPosition};
use qx_strategy::StrategyOrderStatusUpdate;
use qx_zhenlu::{Oms, RiskGate};
use std::collections::BTreeMap;

pub(crate) struct OrderAdmissionInput<'a> {
    pub(crate) orders: &'a mut Vec<Order>,
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
    pub(crate) portfolio_budget_markets: Option<&'a BTreeMap<InstrumentId, PortfolioBudgetMarket>>,
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

pub(crate) struct OrderAdmissionResult {
    pub(crate) accepted_orders: Vec<AdmittedOrder>,
    pub(crate) rejected_updates: Vec<StrategyOrderStatusUpdate>,
}

fn raw_decimal(value: i128) -> ExactDecimal {
    ExactDecimal::from_parts(value, 9).expect("legacy scale is supported")
}

fn exact_to_raw(value: ExactDecimal, rounding: DecimalRounding) -> Result<i128, qx_core::QxError> {
    value
        .checked_div_at_scale(ExactDecimal::ONE, 9, rounding)
        .and_then(|projected| projected.to_raw(9).ok())
        .ok_or_else(|| qx_core::QxError::Invariant("精确值投影到 raw 失败".into()))
}

fn exact_abs(value: ExactDecimal) -> Result<ExactDecimal, qx_core::QxError> {
    if value.is_negative() {
        value
            .checked_neg()
            .ok_or_else(|| qx_core::QxError::Invariant("精确数量绝对值溢出".into()))
    } else {
        Ok(value)
    }
}

fn exact_gross_notional_for_market(
    ledger: &Ledger,
    oms: &Oms,
    account_id: &str,
    instrument: &InstrumentId,
    spec: &TradingInstrumentSpec,
    mark: ExactDecimal,
    candidate_order_id: u64,
) -> Result<ExactDecimal, qx_core::QxError> {
    let net = ledger
        .position_for_side_exact(account_id, instrument, PositionSide::Net)
        .quantity;
    let long = ledger
        .position_for_side_exact(account_id, instrument, PositionSide::Long)
        .quantity;
    let short = ledger
        .position_for_side_exact(account_id, instrument, PositionSide::Short)
        .quantity;
    let mut gross = ExactDecimal::ZERO;
    for quantity in [net, long, short] {
        if quantity.is_positive() || quantity.is_negative() {
            gross = gross
                .checked_add(spec.notional_exact(exact_abs(quantity)?, mark)?)
                .ok_or_else(|| qx_core::QxError::Invariant("精确组合持仓名义额溢出".into()))?;
        }
    }
    let pending = qx_risk::pending_order_gross_notional_exact_at_reference_mark(
        oms.all_orders_ref(),
        candidate_order_id,
        account_id,
        instrument,
        spec,
        Some(mark),
    )?;
    gross
        .checked_add(pending)
        .ok_or_else(|| qx_core::QxError::Invariant("精确组合在途名义额溢出".into()))
}

fn reject_order(
    log: &mut EventLog,
    ts: u64,
    order: &Order,
    reason: &str,
    rejected_updates: &mut Vec<StrategyOrderStatusUpdate>,
) -> Result<(), qx_core::QxError> {
    let event_seq = append_rejection(log, ts, order.client_id, reason);
    let Some(strategy_id) = order
        .trace
        .as_ref()
        .and_then(|trace| trace.strategy_id.as_deref())
        .filter(|strategy_id| !strategy_id.trim().is_empty())
    else {
        return Ok(());
    };
    let mut rejected_order = order.clone();
    rejected_order.status = OrderStatus::Rejected;
    rejected_updates.push(
        StrategyOrderStatusUpdate::from_order(
            &rejected_order,
            strategy_id,
            event_seq,
            ts,
            OrderStatus::Rejected,
            Some(reason),
        )
        .map_err(qx_core::QxError::BusinessViolation)?,
    );
    Ok(())
}

pub(crate) fn admit_orders(
    input: OrderAdmissionInput<'_>,
) -> Result<OrderAdmissionResult, qx_core::QxError> {
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
        portfolio_budget_markets,
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
    let mut reserved_cash_spot_sell_qty = if derivative_spec.is_none() {
        oms.all_orders_ref()
            .filter(|existing| {
                existing.account_id == account_id
                    && existing.instrument == *instrument
                    && existing.side == Side::Sell
                    && !existing.status.is_terminal()
            })
            .fold(0_i128, |reserved, existing| {
                reserved.saturating_add(existing.remaining().raw())
            })
    } else {
        0
    };
    let mut accepted_orders = Vec::new();
    let mut rejected_updates = Vec::new();
    for mut order in orders.drain(..) {
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
        let exact_amounts = order.exact_amounts;
        let quote_budget = exact_amounts.and_then(|amounts| amounts.quote_budget);
        if let Some(budget) = quote_budget {
            let Some(spec) = instrument_spec else {
                reject_order(
                    log,
                    ts,
                    &order,
                    "Bar quote_budget 订单缺少冻结 TradingInstrumentSpec",
                    &mut rejected_updates,
                )?;
                continue;
            };
            if order.side != Side::Buy || derivative_spec.is_some() {
                reject_order(
                    log,
                    ts,
                    &order,
                    "Bar quote_budget 只支持现货市价买入",
                    &mut rejected_updates,
                )?;
                continue;
            }
            if let Err(error) = spec.validate_exact_quote_budget(budget) {
                reject_order(log, ts, &order, &error.to_string(), &mut rejected_updates)?;
                continue;
            }
        }
        let spot_sell_reason =
            if ashare_rules.is_none() && derivative_spec.is_none() && order.side == Side::Sell {
                if let Some(amounts) = exact_amounts {
                    let exact_position = ledger
                        .position_for_side_exact(account_id, instrument, PositionSide::Net)
                        .quantity;
                    let reserved = raw_decimal(reserved_cash_spot_sell_qty);
                    let available = exact_position
                        .checked_sub(reserved)
                        .unwrap_or(ExactDecimal::ZERO);
                    amounts
                        .quantity
                        .checked_cmp(available)
                        .is_none_or(|ordering| ordering.is_gt())
                        .then_some("现货卖出数量超过可用持仓")
                } else if order.qty.raw() > position.saturating_sub(reserved_cash_spot_sell_qty) {
                    Some("现货卖出数量超过可用持仓")
                } else {
                    None
                }
            } else {
                None
            };
        let ashare_reason = ashare_rules.and_then(|rules| {
            rules
                .validate_order(
                    &order,
                    position,
                    ashare_bought_today.saturating_add(reserved_cash_spot_sell_qty),
                    ts,
                )
                .err()
        });
        if let Some(reason) = invalid_reason {
            reject_order(log, ts, &order, reason, &mut rejected_updates)?;
            continue;
        }
        if let Some(reason) = spot_sell_reason {
            reject_order(log, ts, &order, reason, &mut rejected_updates)?;
            continue;
        }
        if let Some(reason) = ashare_reason {
            reject_order(log, ts, &order, &reason, &mut rejected_updates)?;
            continue;
        }

        let product_policy = order.policy.unwrap_or_default();
        if let Some(spec) = instrument_spec {
            if let Err(error) = product_policy.validate_for(spec) {
                reject_order(log, ts, &order, &error.to_string(), &mut rejected_updates)?;
                continue;
            }
        }
        let leverage = if derivative_spec.is_some() {
            product_policy.leverage
        } else {
            1
        };
        // Decisions at Bar(t) only observe data through Bar(t-1), so risk and
        // funding use the last visible close rather than the current bar.
        let reference_price_exact = order
            .limit_price_decimal()
            .unwrap_or_else(|| raw_decimal(visible_close));
        let reference_price = exact_to_raw(reference_price_exact, DecimalRounding::Ceiling)?;
        if quote_budget.is_some() {
            if let Some(amounts) = order.exact_amounts.as_mut() {
                amounts.market_reference_price = Some(reference_price_exact);
            }
        }
        let quantity_exact = if let Some(budget) = quote_budget {
            instrument_spec.and_then(|spec| {
                spec.base_quantity_for_quote_budget(budget, reference_price_exact)
                    .ok()
            })
        } else {
            exact_amounts.map_or_else(
                || order.qty.raw().checked_abs().map(raw_decimal),
                |amounts| Some(amounts.quantity),
            )
        };
        let Some(quantity_exact) = quantity_exact.filter(|qty| qty.is_positive()) else {
            reject_order(
                log,
                ts,
                &order,
                "订单数量必须是可表示的正数",
                &mut rejected_updates,
            )?;
            continue;
        };
        let order_qty = exact_to_raw(quantity_exact, DecimalRounding::Ceiling)?;
        if order_qty <= 0 {
            reject_order(
                log,
                ts,
                &order,
                "订单数量必须是可表示的正数",
                &mut rejected_updates,
            )?;
            continue;
        }
        if let Some(spec) = instrument_spec {
            let validation = if exact_amounts.is_some() {
                if let Some(budget) = quote_budget {
                    spec.validate_exact_quote_budget(budget)
                        .and_then(|()| spec.validate_exact_order_precision(quantity_exact, None))
                } else {
                    spec.validate_exact_order_with_reference(
                        quantity_exact,
                        order.limit_price_decimal(),
                        Some(reference_price_exact),
                    )
                }
            } else {
                spec.validate_order_with_reference(
                    order_qty,
                    order.limit.map(|price| price.raw()),
                    Some(reference_price),
                )
            };
            if let Err(error) = validation {
                reject_order(log, ts, &order, &error.to_string(), &mut rejected_updates)?;
                continue;
            }
        } else if exact_amounts.is_some() {
            reject_order(
                log,
                ts,
                &order,
                "精确数量订单缺少 TradingInstrumentSpec",
                &mut rejected_updates,
            )?;
            continue;
        }
        let strategy_budget_check = match portfolio_budget.order_budget_check_with_exact_budget(
            |strategy_id| strategy.strategy_budget_exact(strategy_id),
            &order,
            quantity_exact,
            reference_price_exact,
            raw_decimal(visible_close),
            instrument_spec,
            multiplier,
            margin,
            portfolio_budget_markets,
            currency,
        ) {
            Ok(check) => check,
            Err(qx_core::QxError::BusinessViolation(reason)) => {
                reject_order(log, ts, &order, &reason, &mut rejected_updates)?;
                continue;
            }
            Err(error) => return Err(error),
        };
        let reduce_only_allowed = !product_policy.reduce_only
            || if exact_amounts.is_some() {
                let current = ledger
                    .position_for_side_exact(account_id, instrument, product_policy.position_side)
                    .quantity;
                let direction_matches = match order.side {
                    Side::Buy => current.is_negative(),
                    Side::Sell => current.is_positive(),
                };
                direction_matches
                    && quantity_exact
                        .checked_cmp(exact_abs(current)?)
                        .is_some_and(|ordering| !ordering.is_gt())
            } else {
                reduce_only_order_allowed(ledger, account_id, instrument, &order, order_qty)
            };
        let required_margin = if let Some(budget) = quote_budget {
            exact_to_raw(budget, DecimalRounding::Ceiling)?
        } else if let Some(spec) = derivative_spec {
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
            )?)?
        };
        let equity = match (portfolio_marks, portfolio_specs) {
            (Some(marks), Some(specs)) => ledger
                .valuate(
                    &qx_core::ValuationContext::spot(account_id, marks, currency)
                        .with_market_specs(specs)
                        .with_fx(fx_rates),
                )?
                .equity
                .raw(),
            (None, None) => {
                let marks = BTreeMap::from([(instrument.clone(), Price::from_raw(visible_close))]);
                equity_for(
                    ledger,
                    account_id,
                    &marks,
                    currency,
                    multiplier,
                    derivative_spec,
                    fx_rates,
                )?
            }
            _ => {
                return Err(qx_core::QxError::Invariant(
                    "共享账户风控估值必须同时提供 market marks 与 specs".into(),
                ));
            }
        };
        let cash_funded_spot_buy =
            derivative_spec.is_none() && order.side == Side::Buy && !product_policy.reduce_only;
        let projected_margin_exact = quote_budget.unwrap_or_else(|| raw_decimal(required_margin));
        let required_funding_exact = if exact_amounts.is_some() {
            if cash_funded_spot_buy {
                let spec = instrument_spec.ok_or_else(|| {
                    qx_core::QxError::BusinessViolation(
                        "精确现货买单缺少 TradingInstrumentSpec".into(),
                    )
                })?;
                let quote_cost = if let Some(budget) = quote_budget {
                    budget
                } else {
                    spec.quote_cash_exact(
                        quantity_exact,
                        reference_price_exact,
                        DecimalRounding::AwayFromZero,
                    )?
                };
                let fee_quantity = if quote_budget.is_some() {
                    quantity_exact
                        .checked_add(spec.quantity_step_exact()?)
                        .ok_or_else(|| {
                            qx_core::QxError::Invariant("quote_budget 手续费数量溢出".into())
                        })?
                } else {
                    quantity_exact
                };
                let fee = matcher
                    .estimate_fee_exact(fee_quantity, reference_price_exact, &order)
                    .map_err(qx_core::QxError::BusinessViolation)?;
                let cash_required = quote_cost
                    .checked_add(fee)
                    .ok_or_else(|| qx_core::QxError::Invariant("精确买入资金需求溢出".into()))?;
                Some(
                    if cash_required
                        .checked_cmp(projected_margin_exact)
                        .is_some_and(|ordering| ordering.is_gt())
                    {
                        cash_required
                    } else {
                        projected_margin_exact
                    },
                )
            } else {
                Some(projected_margin_exact)
            }
        } else {
            None
        };
        let required_funding = if let Some(exact) = required_funding_exact {
            exact_to_raw(exact, DecimalRounding::Ceiling)?
        } else if cash_funded_spot_buy {
            let notional = notional_for(
                derivative_spec,
                order_qty,
                checked_abs(reference_price)?,
                multiplier,
            )?;
            notional
                .checked_add(matcher.estimate_fee(
                    order_qty,
                    checked_abs(reference_price)?,
                    &order,
                )?)
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
        let unreserved_funding_exact = if exact_amounts.is_some() {
            let available_exact = if cash_funded_spot_buy {
                ledger.cash_for_exact(account_id, currency)
            } else {
                raw_decimal(available_funding)
            };
            Some(
                available_exact
                    .checked_sub(raw_decimal(total_reserved_funding))
                    .ok_or_else(|| qx_core::QxError::Invariant("精确可用资金计算溢出".into()))?,
            )
        } else {
            None
        };
        let funding_sufficient = match (required_funding_exact, unreserved_funding_exact) {
            (Some(required), Some(available)) => required
                .checked_cmp(available)
                .is_some_and(|ordering| !ordering.is_gt()),
            _ => required_funding <= unreserved_funding,
        };
        if !reduce_only_allowed {
            reject_order(
                log,
                ts,
                &order,
                "reduce-only order would open or exceed the existing position",
                &mut rejected_updates,
            )?;
            continue;
        }
        if let Some((budget, required)) = strategy_budget_check {
            if required
                .checked_cmp(budget)
                .ok_or_else(|| qx_core::QxError::Invariant("组合策略预算比较失败".into()))?
                .is_gt()
            {
                reject_order(
                    log,
                    ts,
                    &order,
                    "订单会使策略持仓需求超过 reserved_capital_raw",
                    &mut rejected_updates,
                )?;
                continue;
            }
        }
        if !funding_sufficient && (cash_funded_spot_buy || instrument_spec.is_none()) {
            reject_order(
                log,
                ts,
                &order,
                if cash_funded_spot_buy {
                    "买入成本（含预估手续费）超过账户可用现金"
                } else {
                    "initial margin exceeds equity"
                },
                &mut rejected_updates,
            )?;
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
                if exact_amounts.is_some() {
                    let pending = qx_risk::pending_order_gross_notional_exact_at_reference_mark(
                        oms.all_orders_ref(),
                        order.client_id,
                        account_id,
                        instrument,
                        spec,
                        Some(raw_decimal(visible_close)),
                    )?;
                    exact_to_raw(pending, DecimalRounding::Ceiling)?
                } else {
                    qx_risk::pending_order_gross_notional_at_reference_mark(
                        oms.all_orders_ref(),
                        order.client_id,
                        account_id,
                        instrument,
                        spec,
                        Some(Price::from_raw(visible_close)),
                    )?
                }
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
        let exact_risk = if exact_amounts.is_some() {
            let Some(spec) = instrument_spec else {
                reject_order(
                    log,
                    ts,
                    &order,
                    "精确数量订单缺少 TradingInstrumentSpec",
                    &mut rejected_updates,
                )?;
                continue;
            };
            let mut exact_gross = ExactDecimal::ZERO;
            match (portfolio_marks, portfolio_specs) {
                (Some(marks), Some(specs)) => {
                    for (market_instrument, market_spec) in specs {
                        let mark = marks
                            .get(market_instrument)
                            .map(|price| raw_decimal(price.raw().saturating_abs()))
                            .or_else(|| {
                                (market_instrument == instrument).then_some(reference_price_exact)
                            });
                        let Some(mark) = mark else {
                            let net = ledger
                                .position_for_side_exact(
                                    account_id,
                                    market_instrument,
                                    PositionSide::Net,
                                )
                                .quantity;
                            let long = ledger
                                .position_for_side_exact(
                                    account_id,
                                    market_instrument,
                                    PositionSide::Long,
                                )
                                .quantity;
                            let short = ledger
                                .position_for_side_exact(
                                    account_id,
                                    market_instrument,
                                    PositionSide::Short,
                                )
                                .quantity;
                            if net.is_positive()
                                || net.is_negative()
                                || long.is_positive()
                                || long.is_negative()
                                || short.is_positive()
                                || short.is_negative()
                            {
                                return Err(qx_core::QxError::BusinessViolation(format!(
                                    "精确组合风控缺少持仓标的 {market_instrument} 的参考价"
                                )));
                            }
                            continue;
                        };
                        exact_gross = exact_gross
                            .checked_add(exact_gross_notional_for_market(
                                ledger,
                                oms,
                                account_id,
                                market_instrument,
                                market_spec,
                                mark,
                                order.client_id,
                            )?)
                            .ok_or_else(|| {
                                qx_core::QxError::Invariant("精确组合名义额合计溢出".into())
                            })?;
                    }
                }
                (None, None) => {
                    exact_gross = exact_gross_notional_for_market(
                        ledger,
                        oms,
                        account_id,
                        instrument,
                        spec,
                        reference_price_exact,
                        order.client_id,
                    )?;
                }
                _ => {
                    return Err(qx_core::QxError::Invariant(
                        "精确共享账户风控估值必须同时提供 market marks 与 specs".into(),
                    ));
                }
            }
            let exact_net = ledger
                .position_for_side_exact(account_id, instrument, PositionSide::Net)
                .quantity;
            let exact_long = ledger
                .position_for_side_exact(account_id, instrument, PositionSide::Long)
                .quantity;
            let exact_short = ledger
                .position_for_side_exact(account_id, instrument, PositionSide::Short)
                .quantity;
            Some(ExactOrderRiskFacts {
                available_margin: unreserved_funding_exact,
                projected_initial_margin: instrument_spec.map(|_| projected_margin_exact),
                reference_price: Some(reference_price_exact),
                max_order_notional: None,
                max_position_notional: None,
                position: ExactOrderRiskPosition {
                    net_qty: exact_net,
                    gross_notional: exact_gross,
                    long_qty: exact_long,
                    short_qty: exact_short,
                },
            })
        } else {
            None
        };
        let risk_context = OrderRiskContext {
            available_margin_raw: instrument_spec.map(|_| unreserved_funding),
            projected_initial_margin_raw: instrument_spec.map(|_| required_margin),
            reference_price: Some(Price::from_raw(reference_price)),
            instrument_spec: instrument_spec.cloned(),
            position: risk_position,
            exact: exact_risk,
            ..OrderRiskContext::default()
        };
        let decision = risk.rule_set().evaluate(&risk_context, &order);
        let trace = order.trace.as_ref();
        let strategy_id = trace.and_then(|trace| trace.strategy_id.clone());
        let signal_id = trace.and_then(|trace| trace.signal_id);
        let intent_id = trace.and_then(|trace| trace.intent_id);
        let metadata = EventMetadata {
            source_id: "qx-zhenlu::RiskGate".into(),
            source_kind: "risk".into(),
            rule_version: decision.rule_set_version.clone(),
            context: EventContext {
                account_id: account_id.into(),
                strategy_id: strategy_id.clone().unwrap_or_default(),
                signal_id: signal_id.map(|value| value.to_string()).unwrap_or_default(),
                intent_id: intent_id.map(|value| value.to_string()).unwrap_or_default(),
                ..EventContext::default()
            },
            ..EventMetadata::default()
        };
        let risk_fact = OrderRiskDecisionFact {
            client_order_id: order.client_id,
            instrument: order.instrument.clone(),
            strategy_id,
            signal_id,
            intent_id,
            allowed: decision.allowed,
            violations: decision.violations.clone(),
            projected_position_raw: decision.projected_position_raw,
            projected_margin_raw: decision.projected_margin_raw,
            reference_price_raw: decision.reference_price_raw,
            rule_set_version: decision.rule_set_version,
        };
        let seq = log.alloc_seq();
        log.append_checked(
            Event::new(
                seq,
                ts,
                Priority::COMMAND,
                EventKind::OrderRiskDecision {
                    decision: risk_fact,
                },
            )
            .correlated(order.client_id.to_string())
            .with_metadata(metadata),
        )?;
        if !decision.allowed {
            reject_order(
                log,
                ts,
                &order,
                &decision.violations.join("; "),
                &mut rejected_updates,
            )?;
            continue;
        }
        if let Err(error) = oms.submit(order.clone()) {
            reject_order(log, ts, &order, &error.to_string(), &mut rejected_updates)?;
            continue;
        }
        if let Err(error) = oms.accept(order.client_id) {
            reject_order(log, ts, &order, &error.to_string(), &mut rejected_updates)?;
            continue;
        }
        portfolio_budget.accept_if_owned_with_exact_budget(
            |strategy_id| strategy.strategy_budget_exact(strategy_id),
            &order,
            quantity_exact,
            reference_price_exact,
        )?;
        if derivative_spec.is_none() && order.side == Side::Sell {
            let sell_reservation = if exact_amounts.is_some() {
                exact_to_raw(
                    order.remaining_decimal().unwrap_or(ExactDecimal::ZERO),
                    DecimalRounding::Ceiling,
                )?
            } else {
                order.remaining().raw()
            };
            reserved_cash_spot_sell_qty =
                reserved_cash_spot_sell_qty.saturating_add(sell_reservation);
        }
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
            order: oms.get(order.client_id).cloned().unwrap_or(order),
            funding_reservation_raw: required_funding,
        });
    }
    Ok(OrderAdmissionResult {
        accepted_orders,
        rejected_updates,
    })
}
