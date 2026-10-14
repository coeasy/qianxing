use qx_core::{Fill, Ledger, Order, OrderPolicy, Price, TradingInstrumentSpec, ValuationContext};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
struct Reservation {
    remaining_qty_raw: i128,
    remaining_cash_raw: i128,
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
    Accept { reserve_cash_raw: i128 },
    Reject(String),
}

pub(super) struct FundingContext<'a> {
    pub(super) snapshot: &'a crate::OrderBookSnapshot,
    pub(super) reference_price: Option<Price>,
    pub(super) ledger: &'a Ledger,
    pub(super) account_id: &'a str,
    pub(super) currency: &'a str,
    pub(super) fee_bps: i128,
    pub(super) reservations: &'a SpotCashReservations,
}

pub(super) fn preflight_order(
    order: &Order,
    policy: OrderPolicy,
    spec: Option<&TradingInstrumentSpec>,
    context: FundingContext<'_>,
) -> Result<FundingDecision, qx_core::QxError> {
    if let Some(spec) = spec {
        let spot_buy =
            spec.product == qx_core::TradingProduct::Spot && order.side == qx_core::Side::Buy;
        if policy.reduce_only && !spot_buy {
            return Ok(FundingDecision::NotRequired);
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
        let available = if leveraged {
            let marks = BTreeMap::from([(order.instrument.clone(), price)]);
            let valuation = ValuationContext::spot(context.account_id, &marks, context.currency)
                .with_spec(Some(spec));
            context.ledger.valuate(&valuation)?.equity.raw()
        } else {
            unreserved_cash(
                context
                    .ledger
                    .cash_for(context.account_id, context.currency),
                context.reservations,
            )?
        };
        let required = spec.initial_margin(order.qty.raw(), price.raw(), policy.leverage)?;
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
            return Ok(FundingDecision::Reject(if leveraged {
                "订单初始保证金超过订单簿回测权益".into()
            } else {
                "买入成本（含预估手续费）超过订单簿回测可用现金".into()
            }));
        }
        return Ok(FundingDecision::Accept {
            reserve_cash_raw: if spot_buy { required_funding } else { 0 },
        });
    }

    if order.side != qx_core::Side::Buy {
        return Ok(FundingDecision::NotRequired);
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
        )?
    {
        return Ok(FundingDecision::Reject(
            "买入成本（含预估手续费）超过订单簿回测可用现金".into(),
        ));
    }
    Ok(FundingDecision::Accept {
        reserve_cash_raw: required,
    })
}

fn unreserved_cash(
    cash_raw: i128,
    reservations: &SpotCashReservations,
) -> Result<i128, qx_core::QxError> {
    cash_raw
        .checked_sub(reservations.total()?)
        .ok_or_else(|| qx_core::QxError::Invariant("现货挂单资金预留超过账户现金".into()))
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

    pub(super) fn reserve(
        &mut self,
        client_id: u64,
        qty_raw: i128,
        cash_raw: i128,
    ) -> Result<(), qx_core::QxError> {
        if qty_raw <= 0 || cash_raw <= 0 || self.by_order.contains_key(&client_id) {
            return Err(qx_core::QxError::Invariant(format!(
                "现货挂单资金预留身份或金额非法: client_id={client_id}"
            )));
        }
        self.by_order.insert(
            client_id,
            Reservation {
                remaining_qty_raw: qty_raw,
                remaining_cash_raw: cash_raw,
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
        spec: Option<&TradingInstrumentSpec>,
        cash_before_fill: i128,
        total_reserved_before_fill: i128,
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
        let notional = match spec {
            Some(spec) => spec.notional(fill.qty.raw(), fill.price.raw())?,
            None => qx_core::fee::notional(fill.qty.raw(), fill.price.raw()),
        };
        let actual_cost = notional
            .checked_add(fill.fee.raw().max(0))
            .ok_or_else(|| qx_core::QxError::Invariant("现货成交成本溢出".into()))?;
        let other_orders_reserved = total_reserved_before_fill
            .checked_sub(reservation.remaining_cash_raw)
            .ok_or_else(|| {
                qx_core::QxError::Invariant("现货订单资金预留总额小于单笔预留".into())
            })?;
        let cash_after_fill = cash_before_fill.checked_sub(actual_cost).ok_or_else(|| {
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
        Ok(Some(ReservationUpdate {
            client_id: order.client_id,
            reservation: (remaining_qty > 0).then_some(Reservation {
                remaining_qty_raw: remaining_qty,
                remaining_cash_raw: remaining_cash,
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
