use qx_core::{Fill, Ledger, Order, OrderPolicy, Price, TradingInstrumentSpec};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
struct Reservation {
    remaining_qty_raw: i128,
    remaining_cash_raw: i128,
    remaining_inventory_raw: i128,
}

#[derive(Clone, Copy)]
pub(super) struct ReservationUpdate {
    client_id: u64,
    reservation: Option<Reservation>,
}

#[derive(Default)]
pub(super) struct SpotCashReservations {
    by_order: BTreeMap<u64, Reservation>,
}

pub(super) enum FundingDecision {
    NotRequired,
    Accept {
        reserve_cash_raw: i128,
        reserve_inventory_raw: i128,
    },
    Reject(String),
}

/// Project initial margin for the filled position, every still-active order, and the
/// candidate. Order-book backtests use marked equity as the account ceiling; the
/// portfolio projection must therefore be supplied to the canonical risk gate.
#[allow(clippy::too_many_arguments)] // Keeps the account, mark, product, and OMS snapshot explicit at the risk boundary.
pub(super) fn projected_margin_and_fees_raw(
    candidate: &Order,
    candidate_policy: OrderPolicy,
    spec: &TradingInstrumentSpec,
    reference_price: Price,
    fee_bps: i128,
    ledger: &Ledger,
    account_id: &str,
    open_orders: &[Order],
) -> Result<(i128, i128), qx_core::QxError> {
    let mut projected = ledger.initial_margin_for_instrument(account_id, spec, reference_price)?;
    let mut projected_fees = 0_i128;
    for order in open_orders.iter().filter(|order| {
        order.client_id != candidate.client_id
            && order.account_id == account_id
            && order.instrument == candidate.instrument
            && !order.status.is_terminal()
            && order.remaining().raw() > 0
    }) {
        let policy = order.policy.unwrap_or_default();
        policy.validate_for(spec)?;
        let price = order.limit.unwrap_or(reference_price).max(reference_price);
        if !policy.reduce_only {
            let margin =
                spec.initial_margin(order.remaining().raw(), price.raw(), policy.leverage)?;
            projected = projected
                .checked_add(margin)
                .ok_or_else(|| qx_core::QxError::Invariant("在途订单保证金合计溢出".into()))?;
        }
        let notional = spec.notional(order.remaining().raw(), price.raw())?;
        projected_fees = projected_fees
            .checked_add(qx_core::bp_amount_checked(notional, fee_bps)?)
            .ok_or_else(|| qx_core::QxError::Invariant("在途订单手续费预留合计溢出".into()))?;
    }
    if !candidate_policy.reduce_only {
        let price = candidate
            .limit
            .unwrap_or(reference_price)
            .max(reference_price);
        let margin =
            spec.initial_margin(candidate.qty.raw(), price.raw(), candidate_policy.leverage)?;
        projected = projected
            .checked_add(margin)
            .ok_or_else(|| qx_core::QxError::Invariant("候选订单保证金合计溢出".into()))?;
        let notional = spec.notional(candidate.qty.raw(), price.raw())?;
        projected_fees = projected_fees
            .checked_add(qx_core::bp_amount_checked(notional, fee_bps)?)
            .ok_or_else(|| qx_core::QxError::Invariant("候选订单手续费预留溢出".into()))?;
    }
    if candidate_policy.reduce_only {
        let price = candidate
            .limit
            .unwrap_or(reference_price)
            .max(reference_price);
        let notional = spec.notional(candidate.qty.raw(), price.raw())?;
        projected_fees = projected_fees
            .checked_add(qx_core::bp_amount_checked(notional, fee_bps)?)
            .ok_or_else(|| qx_core::QxError::Invariant("候选订单手续费预留溢出".into()))?;
    }
    Ok((projected, projected_fees))
}

/// Estimate strategy-visible free margin from this backtest's marked equity, settled
/// position margin, live-order margin, and reserved fees. This is a simulation estimate,
/// not a venue-reported free-margin snapshot.
pub(super) fn simulated_available_margin_raw(
    spec: &TradingInstrumentSpec,
    reference_price: Price,
    fee_bps: i128,
    ledger: &Ledger,
    account_id: &str,
    open_orders: &[Order],
) -> Result<i128, qx_core::QxError> {
    let mut used_margin =
        ledger.initial_margin_for_instrument(account_id, spec, reference_price)?;
    let mut reserved_fees = 0_i128;
    for order in open_orders.iter().filter(|order| {
        order.account_id == account_id
            && order.instrument == spec.instrument
            && !order.status.is_terminal()
            && order.remaining().raw() > 0
    }) {
        let policy = order.policy.ok_or_else(|| {
            qx_core::QxError::BusinessViolation(format!(
                "衍生品在途订单缺少保证金策略: client_id={}",
                order.client_id
            ))
        })?;
        policy.validate_for(spec)?;
        let price = order.limit.unwrap_or(reference_price).max(reference_price);
        if !policy.reduce_only {
            used_margin = used_margin
                .checked_add(spec.initial_margin(
                    order.remaining().raw(),
                    price.raw(),
                    policy.leverage,
                )?)
                .ok_or_else(|| qx_core::QxError::Invariant("在途订单保证金合计溢出".into()))?;
        }
        reserved_fees = reserved_fees
            .checked_add(qx_core::bp_amount_checked(
                spec.notional(order.remaining().raw(), price.raw())?,
                fee_bps,
            )?)
            .ok_or_else(|| qx_core::QxError::Invariant("在途订单手续费预留合计溢出".into()))?;
    }
    let marks = BTreeMap::from([(spec.instrument.clone(), reference_price)]);
    let valuation = qx_core::ValuationContext::spot(account_id, &marks, &spec.settlement_currency)
        .with_spec(Some(spec));
    ledger
        .valuate(&valuation)?
        .equity
        .raw()
        .checked_sub(used_margin)
        .and_then(|available| available.checked_sub(reserved_fees))
        .ok_or_else(|| qx_core::QxError::Invariant("衍生品可用保证金估算溢出".into()))
}

pub(super) struct FundingContext<'a> {
    pub(super) snapshot: &'a crate::OrderBookSnapshot,
    pub(super) reference_price: Option<Price>,
    pub(super) ledger: &'a Ledger,
    pub(super) account_id: &'a str,
    pub(super) currency: &'a str,
    pub(super) fee_bps: i128,
    pub(super) reservations: &'a SpotCashReservations,
    pub(super) total_reserved_cash_raw: Option<i128>,
}

pub(super) struct FillFundingContext<'a> {
    pub(super) spec: Option<&'a TradingInstrumentSpec>,
    pub(super) cash_before_fill: i128,
    pub(super) total_reserved_before_fill: i128,
    pub(super) inventory_before_fill: i128,
    pub(super) total_inventory_reserved_before_fill: i128,
}

pub(super) fn preflight_order(
    order: &Order,
    policy: OrderPolicy,
    spec: Option<&TradingInstrumentSpec>,
    context: FundingContext<'_>,
) -> Result<FundingDecision, qx_core::QxError> {
    if let Some(spec) = spec {
        let spot = spec.product == qx_core::TradingProduct::Spot;
        let spot_buy = spot && order.side == qx_core::Side::Buy;
        let spot_sell = spot && order.side == qx_core::Side::Sell;
        if policy.reduce_only && !spot {
            return Ok(FundingDecision::NotRequired);
        }
        if spot_sell {
            let inventory = context
                .ledger
                .position_for(context.account_id, &order.instrument)
                .quantity
                .raw();
            if order.qty.raw() > unreserved_inventory(inventory, context.reservations)? {
                return Ok(FundingDecision::Reject(
                    "现货卖出数量超过扣除待成交卖单后的可用持仓".into(),
                ));
            }
        }
        let side_price = match order.side {
            qx_core::Side::Buy => context.snapshot.asks.first().map(|level| level.price),
            qx_core::Side::Sell => context.snapshot.bids.first().map(|level| level.price),
        };
        let price = order
            .limit
            .or(side_price)
            .or(context.reference_price)
            .ok_or_else(|| {
                qx_core::QxError::BusinessViolation("订单簿订单缺少保证金参考价".into())
            })?;
        let leveraged = spec.product.supports_leverage();
        // Derivative funding is evaluated once by the canonical account risk gate below,
        // using projected portfolio margin. Comparing an isolated order against equity
        // here can admit multiple individually affordable orders whose sum is not.
        if leveraged {
            return Ok(FundingDecision::NotRequired);
        }
        let available = unreserved_cash(
            context
                .ledger
                .cash_for(context.account_id, context.currency),
            context.reservations,
            context.total_reserved_cash_raw,
        )?;
        let required = spec.notional(order.qty.raw(), price.raw())?;
        let required_funding = if order.side == qx_core::Side::Buy {
            required
                .checked_add(qx_core::bp_amount_checked(
                    spec.notional(order.qty.raw(), price.raw())?,
                    context.fee_bps,
                )?)
                .ok_or_else(|| qx_core::QxError::Invariant("买入所需现金溢出".into()))?
        } else {
            required
        };
        if required_funding > available {
            return Ok(FundingDecision::Reject(
                "买入成本（含预估手续费）超过订单簿回测可用现金".into(),
            ));
        }
        return Ok(FundingDecision::Accept {
            reserve_cash_raw: if spot_buy { required_funding } else { 0 },
            reserve_inventory_raw: if spot_sell { order.qty.raw() } else { 0 },
        });
    }

    if order.side == qx_core::Side::Sell {
        let inventory = context
            .ledger
            .position_for(context.account_id, &order.instrument)
            .quantity
            .raw();
        if order.qty.raw() > unreserved_inventory(inventory, context.reservations)? {
            return Ok(FundingDecision::Reject(
                "现货卖出数量超过扣除待成交卖单后的可用持仓".into(),
            ));
        }
        return Ok(FundingDecision::Accept {
            reserve_cash_raw: 0,
            reserve_inventory_raw: order.qty.raw(),
        });
    }
    let price = order
        .limit
        .or_else(|| context.snapshot.asks.first().map(|level| level.price))
        .or(context.reference_price)
        .ok_or_else(|| qx_core::QxError::BusinessViolation("订单簿现货买入缺少参考价".into()))?;
    let notional = qx_core::fee::notional(order.qty.raw(), price.raw());
    let required = notional
        .checked_add(qx_core::bp_amount_checked(notional, context.fee_bps)?)
        .ok_or_else(|| qx_core::QxError::Invariant("买入所需现金溢出".into()))?;
    if required
        > unreserved_cash(
            context
                .ledger
                .cash_for(context.account_id, context.currency),
            context.reservations,
            context.total_reserved_cash_raw,
        )?
    {
        return Ok(FundingDecision::Reject(
            "买入成本（含预估手续费）超过订单簿回测可用现金".into(),
        ));
    }
    Ok(FundingDecision::Accept {
        reserve_cash_raw: required,
        reserve_inventory_raw: 0,
    })
}

fn unreserved_cash(
    cash_raw: i128,
    reservations: &SpotCashReservations,
    total_reserved_cash_raw: Option<i128>,
) -> Result<i128, qx_core::QxError> {
    cash_raw
        .checked_sub(match total_reserved_cash_raw {
            Some(total) => total,
            None => reservations.total()?,
        })
        .ok_or_else(|| qx_core::QxError::Invariant("现货挂单资金预留超过账户现金".into()))
}

fn unreserved_inventory(
    inventory_raw: i128,
    reservations: &SpotCashReservations,
) -> Result<i128, qx_core::QxError> {
    inventory_raw
        .checked_sub(reservations.total_inventory()?)
        .map(|available| available.max(0))
        .ok_or_else(|| qx_core::QxError::Invariant("现货挂单库存预留超过账面持仓".into()))
}

impl SpotCashReservations {
    pub(super) fn total(&self) -> Result<i128, qx_core::QxError> {
        self.by_order
            .values()
            .try_fold(0_i128, |total, reservation| {
                total
                    .checked_add(reservation.remaining_cash_raw)
                    .ok_or_else(|| qx_core::QxError::Invariant("现货挂单资金预留求和溢出".into()))
            })
    }

    pub(super) fn total_inventory(&self) -> Result<i128, qx_core::QxError> {
        self.by_order
            .values()
            .try_fold(0_i128, |total, reservation| {
                total
                    .checked_add(reservation.remaining_inventory_raw)
                    .ok_or_else(|| qx_core::QxError::Invariant("现货挂单库存预留求和溢出".into()))
            })
    }

    pub(super) fn reserve(
        &mut self,
        client_id: u64,
        qty_raw: i128,
        cash_raw: i128,
        inventory_raw: i128,
    ) -> Result<(), qx_core::QxError> {
        if qty_raw <= 0
            || cash_raw < 0
            || inventory_raw < 0
            || (cash_raw == 0 && inventory_raw == 0)
            || self.by_order.contains_key(&client_id)
        {
            return Err(qx_core::QxError::Invariant(format!(
                "现货挂单资金预留身份或金额非法: client_id={client_id}"
            )));
        }
        self.by_order.insert(
            client_id,
            Reservation {
                remaining_qty_raw: qty_raw,
                remaining_cash_raw: cash_raw,
                remaining_inventory_raw: inventory_raw,
            },
        );
        Ok(())
    }

    pub(super) fn release(&mut self, client_id: u64) {
        self.by_order.remove(&client_id);
    }

    pub(super) fn plan_fill(
        &self,
        order: &Order,
        fill: &Fill,
        context: FillFundingContext<'_>,
    ) -> Result<Option<ReservationUpdate>, qx_core::QxError> {
        let Some(reservation) = self.by_order.get(&order.client_id).copied() else {
            return Ok(None);
        };
        if fill.qty.raw() <= 0 || fill.qty.raw() > reservation.remaining_qty_raw {
            return Err(qx_core::QxError::ReconcileRequired(format!(
                "现货挂单成交数量超出资金预留: client_id={}",
                order.client_id
            )));
        }
        let remaining_qty = reservation.remaining_qty_raw - fill.qty.raw();
        let remaining_cash = if remaining_qty == 0 {
            0
        } else {
            mul_div_ceil(
                reservation.remaining_cash_raw,
                remaining_qty,
                reservation.remaining_qty_raw,
            )?
        };
        let remaining_inventory = if reservation.remaining_inventory_raw == 0 {
            0
        } else {
            remaining_qty
        };
        let other_inventory_reserved = context
            .total_inventory_reserved_before_fill
            .checked_sub(reservation.remaining_inventory_raw)
            .ok_or_else(|| {
                qx_core::QxError::Invariant("现货订单库存预留总额小于单笔预留".into())
            })?;
        if reservation.remaining_inventory_raw > 0 {
            let inventory_after_fill = context
                .inventory_before_fill
                .checked_sub(fill.qty.raw())
                .ok_or_else(|| qx_core::QxError::Invariant("现货成交后持仓计算溢出".into()))?;
            let remaining_reserved = other_inventory_reserved
                .checked_add(remaining_inventory)
                .ok_or_else(|| qx_core::QxError::Invariant("现货剩余库存预留溢出".into()))?;
            if inventory_after_fill < remaining_reserved {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "现货卖出成交后持仓不足以覆盖其他待成交卖单: client_id={}, inventory_after={inventory_after_fill}, reserved_after={remaining_reserved}",
                    order.client_id,
                )));
            }
        }
        if reservation.remaining_cash_raw > 0 {
            let notional = match context.spec {
                Some(spec) => spec.notional(fill.qty.raw(), fill.price.raw())?,
                None => qx_core::fee::notional(fill.qty.raw(), fill.price.raw()),
            };
            let actual_cost = notional
                .checked_add(fill.fee.raw().max(0))
                .ok_or_else(|| qx_core::QxError::Invariant("现货成交成本溢出".into()))?;
            let other_orders_reserved = context
                .total_reserved_before_fill
                .checked_sub(reservation.remaining_cash_raw)
                .ok_or_else(|| {
                    qx_core::QxError::Invariant("现货订单资金预留总额小于单笔预留".into())
                })?;
            let cash_after_fill = context
                .cash_before_fill
                .checked_sub(actual_cost)
                .ok_or_else(|| {
                    qx_core::QxError::BusinessViolation("现货成交成本超过账户可用现金".into())
                })?;
            let remaining_reserved = other_orders_reserved
                .checked_add(remaining_cash)
                .ok_or_else(|| qx_core::QxError::Invariant("现货剩余资金预留溢出".into()))?;
            if cash_after_fill < remaining_reserved {
                return Err(qx_core::QxError::BusinessViolation(format!(
                "现货成交后账户现金不足以覆盖其他挂单与本单剩余预留: client_id={}, cash_after={cash_after_fill}, reserved_after={remaining_reserved}",
                order.client_id,
            )));
            }
        }
        Ok(Some(ReservationUpdate {
            client_id: order.client_id,
            reservation: (remaining_qty > 0).then_some(Reservation {
                remaining_qty_raw: remaining_qty,
                remaining_cash_raw: remaining_cash,
                remaining_inventory_raw: remaining_inventory,
            }),
        }))
    }

    pub(super) fn commit_fill(&mut self, update: Option<ReservationUpdate>) {
        let Some(update) = update else {
            return;
        };
        match update.reservation {
            Some(reservation) => {
                self.by_order.insert(update.client_id, reservation);
            }
            None => {
                self.by_order.remove(&update.client_id);
            }
        }
    }
}

fn mul_div_ceil(value: i128, numerator: i128, denominator: i128) -> Result<i128, qx_core::QxError> {
    let product = value
        .checked_mul(numerator)
        .ok_or_else(|| qx_core::QxError::Invariant("现货挂单资金按成交量分摊溢出".into()))?;
    let adjusted = product
        .checked_add(denominator - 1)
        .ok_or_else(|| qx_core::QxError::Invariant("现货挂单资金向上取整溢出".into()))?;
    Ok(adjusted / denominator)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{
        InstrumentId, MarginMode, OrderStatus, PositionMode, PositionSide, Quantity, Side,
        TradingProduct, SCALE,
    };

    fn derivative_spec(instrument: InstrumentId) -> TradingInstrumentSpec {
        TradingInstrumentSpec {
            instrument,
            product: TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 10,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
        }
    }

    fn derivative_order(id: u64, instrument: &InstrumentId, qty: i64) -> Order {
        Order {
            client_id: id,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(qty),
            limit: None,
            status: OrderStatus::Accepted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: Some(OrderPolicy {
                reduce_only: false,
                position_side: PositionSide::Net,
                margin_mode: MarginMode::Cross,
                position_mode: PositionMode::OneWay,
                leverage: 2,
                post_only: false,
            }),
        }
    }

    #[test]
    fn derivative_margin_projection_includes_open_orders_and_candidate() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let spec = derivative_spec(instrument.clone());
        let open = derivative_order(1, &instrument, 6);
        let candidate = derivative_order(2, &instrument, 6);
        let ledger = Ledger::new();
        let (projected, _) = projected_margin_and_fees_raw(
            &candidate,
            candidate.policy.unwrap(),
            &spec,
            Price::from_i64(10),
            0,
            &ledger,
            "main",
            std::slice::from_ref(&open),
        )
        .unwrap();
        let expected = spec
            .initial_margin(Quantity::from_i64(12).raw(), Price::from_i64(10).raw(), 2)
            .unwrap();
        assert_eq!(projected, expected);
        let context = qx_risk::OrderRiskContext {
            available_margin_raw: Some(expected - 1),
            projected_initial_margin_raw: Some(projected),
            reference_price: Some(Price::from_i64(10)),
            instrument_spec: Some(spec),
            ..qx_risk::OrderRiskContext::default()
        };
        assert!(context.validate_order(&candidate).is_err());
    }

    #[test]
    fn derivative_margin_projection_uses_each_open_positions_persisted_leverage() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let spec = derivative_spec(instrument.clone());
        let mut ledger = Ledger::new();
        let mut opened = derivative_order(10, &instrument, 1);
        opened.policy = Some(OrderPolicy {
            leverage: 10,
            ..opened.policy.unwrap()
        });
        ledger
            .apply_fill_with_spec(
                &opened,
                &Fill {
                    order_id: opened.client_id,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(10),
                    ts: 1,
                    account_id: "main".into(),
                    ..Fill::default()
                },
                "USDT",
                &spec,
            )
            .unwrap();

        let candidate = derivative_order(11, &instrument, 1);
        let candidate_policy = candidate.policy.unwrap();
        let (projected, _) = projected_margin_and_fees_raw(
            &candidate,
            candidate_policy,
            &spec,
            Price::from_i64(10),
            0,
            &ledger,
            "main",
            &[],
        )
        .unwrap();
        let expected_existing = spec
            .initial_margin(Quantity::from_i64(1).raw(), Price::from_i64(10).raw(), 10)
            .unwrap();
        let expected_candidate = spec
            .initial_margin(
                Quantity::from_i64(1).raw(),
                Price::from_i64(10).raw(),
                candidate_policy.leverage,
            )
            .unwrap();
        assert_eq!(projected, expected_existing + expected_candidate);
    }

    #[test]
    fn simulated_available_margin_reserves_position_orders_and_fees() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let spec = derivative_spec(instrument.clone());
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "USDT", qx_core::Money::from_i64(10_000), 1)
            .unwrap();
        let mut opened = derivative_order(10, &instrument, 1);
        opened.policy = Some(OrderPolicy {
            leverage: 10,
            ..opened.policy.unwrap()
        });
        ledger
            .apply_fill_with_spec(
                &opened,
                &Fill {
                    order_id: opened.client_id,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(10),
                    ts: 2,
                    account_id: "main".into(),
                    ..Fill::default()
                },
                "USDT",
                &spec,
            )
            .unwrap();
        let mut pending = derivative_order(11, &instrument, 2);
        pending.limit = Some(Price::from_i64(12));
        let open_orders = [pending];

        let available = simulated_available_margin_raw(
            &spec,
            Price::from_i64(10),
            100,
            &ledger,
            "main",
            &open_orders,
        )
        .unwrap();
        let marks = BTreeMap::from([(instrument.clone(), Price::from_i64(10))]);
        let equity = ledger
            .valuate(
                &qx_core::ValuationContext::spot("main", &marks, "USDT").with_spec(Some(&spec)),
            )
            .unwrap()
            .equity
            .raw();
        let position_margin = spec
            .initial_margin(Quantity::from_i64(1).raw(), Price::from_i64(10).raw(), 10)
            .unwrap();
        let pending_margin = spec
            .initial_margin(Quantity::from_i64(2).raw(), Price::from_i64(12).raw(), 2)
            .unwrap();
        let pending_fee = qx_core::bp_amount_checked(
            spec.notional(Quantity::from_i64(2).raw(), Price::from_i64(12).raw())
                .unwrap(),
            100,
        )
        .unwrap();
        assert_eq!(
            available,
            equity - position_margin - pending_margin - pending_fee
        );
        assert!(available < equity, "持仓和在途单必须占用策略可用保证金");
    }

    #[test]
    fn derivative_margin_projection_refuses_a_legacy_position_without_leverage() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let spec = derivative_spec(instrument.clone());
        let mut ledger = Ledger::new();
        ledger
            .apply_entry(qx_core::LedgerEntry {
                id: 0,
                account_id: "main".into(),
                currency: "USDT".into(),
                kind: qx_core::LedgerEntryKind::TradePosition,
                amount: qx_core::Money::ZERO,
                instrument: Some(instrument.clone()),
                quantity: Quantity::from_i64(1),
                price: Some(Price::from_i64(10)),
                order_id: None,
                ts: 1,
                multiplier: 1,
                position_side: None,
                leverage: None,
            })
            .unwrap();
        let candidate = derivative_order(12, &instrument, 1);
        let result = projected_margin_and_fees_raw(
            &candidate,
            candidate.policy.unwrap(),
            &spec,
            Price::from_i64(10),
            0,
            &ledger,
            "main",
            &[],
        );
        assert!(matches!(
            result,
            Err(qx_core::QxError::ReconcileRequired(message))
                if message.contains("缺少可回放的开仓杠杆")
        ));
    }
}
