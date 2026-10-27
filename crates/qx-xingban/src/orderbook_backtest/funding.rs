use qx_core::{
    DecimalRounding, ExactDecimal, ExactValuationContext, Fill, Ledger, Order, OrderPolicy, Price,
    TradingInstrumentSpec, ValuationContext,
};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
struct Reservation {
    remaining_qty_raw: i128,
    remaining_cash_raw: i128,
    remaining_inventory_raw: i128,
    exact: Option<ExactReservation>,
}

#[derive(Clone, Copy)]
struct ExactReservation {
    remaining_qty: ExactDecimal,
    remaining_cash: ExactDecimal,
    remaining_inventory: ExactDecimal,
    remaining_quote_budget: Option<ExactDecimal>,
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
    AcceptExact {
        reserve_quantity: ExactDecimal,
        reserve_cash: ExactDecimal,
        reserve_inventory: ExactDecimal,
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

/// Exact derivative counterpart to `projected_margin_and_fees_raw`. It keeps sub-nano
/// order quantities, prices, notional, margin and fee reserves in their original scale.
#[allow(clippy::too_many_arguments)] // Mirrors the raw projection boundary so both paths use identical account facts.
pub(super) fn projected_margin_and_fees_exact(
    candidate: &Order,
    candidate_policy: OrderPolicy,
    spec: &TradingInstrumentSpec,
    reference_price: ExactDecimal,
    fee_bps: i128,
    ledger: &Ledger,
    account_id: &str,
    open_orders: &[Order],
) -> Result<(ExactDecimal, ExactDecimal), qx_core::QxError> {
    if !spec.product.is_derivative() || !reference_price.is_positive() {
        return Err(qx_core::QxError::BusinessViolation(
            "精确组合保证金投影要求永续/期货规格和正参考价".into(),
        ));
    }
    let mut projected = ExactDecimal::ZERO;
    for side in [
        qx_core::PositionSide::Net,
        qx_core::PositionSide::Long,
        qx_core::PositionSide::Short,
    ] {
        let exact_position = ledger.position_for_side_exact(account_id, &spec.instrument, side);
        if exact_position.quantity.is_zero() {
            continue;
        }
        let position = ledger.position_for_side(account_id, &spec.instrument, side);
        let margin_ratio_raw = position.margin_ratio_raw.ok_or_else(|| {
            qx_core::QxError::ReconcileRequired(format!(
                "精确保证金投影的持仓 {} 缺少开仓杠杆事实",
                spec.instrument
            ))
        })?;
        if !(1..=qx_core::SCALE).contains(&margin_ratio_raw) {
            return Err(qx_core::QxError::Invariant(format!(
                "精确保证金投影的持仓 {} 保证金比例非法: {margin_ratio_raw}",
                spec.instrument
            )));
        }
        // Ledger's compatibility ratio is stored at 1e-9 and may be a floor or a
        // half-even weighted average. Add one raw unit for non-100% ratios so the exact
        // risk path cannot understate a repeating reciprocal leverage such as 1/3.
        let conservative_margin_ratio_raw = if margin_ratio_raw < qx_core::SCALE {
            margin_ratio_raw
                .checked_add(1)
                .ok_or_else(|| qx_core::QxError::Invariant("保证金比例保守取整溢出".into()))?
        } else {
            margin_ratio_raw
        };
        let margin_ratio = ExactDecimal::from_parts(conservative_margin_ratio_raw, 9)
            .map_err(|error| qx_core::QxError::Invariant(error.into()))?;
        let notional = spec.notional_exact(exact_abs(exact_position.quantity)?, reference_price)?;
        let margin = notional
            .checked_mul_at_scale(
                margin_ratio,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::AwayFromZero,
            )
            .ok_or_else(|| qx_core::QxError::Invariant("精确持仓保证金计算溢出".into()))?;
        projected = projected
            .checked_add(margin)
            .ok_or_else(|| qx_core::QxError::Invariant("精确持仓保证金合计溢出".into()))?;
    }

    let mut projected_fees = ExactDecimal::ZERO;
    for order in open_orders.iter().filter(|order| {
        order.client_id != candidate.client_id
            && order.account_id == account_id
            && order.instrument == candidate.instrument
            && !order.status.is_terminal()
            && order.has_remaining_amount()
    }) {
        let policy = order.policy.unwrap_or_default();
        policy.validate_for(spec)?;
        let remaining = order.remaining_decimal().ok_or_else(|| {
            qx_core::QxError::Invariant(format!(
                "精确在途订单剩余量溢出: client_id={}",
                order.client_id
            ))
        })?;
        if !remaining.is_positive() {
            continue;
        }
        let price =
            conservative_derivative_price(spec, order.limit_price_decimal(), reference_price)?;
        if !policy.reduce_only {
            projected = projected
                .checked_add(spec.initial_margin_exact(remaining, price, policy.leverage)?)
                .ok_or_else(|| qx_core::QxError::Invariant("精确在途保证金合计溢出".into()))?;
        }
        let notional = spec.notional_exact(remaining, price)?;
        projected_fees = projected_fees
            .checked_add(exact_fee_reserve(notional, fee_bps)?)
            .ok_or_else(|| qx_core::QxError::Invariant("精确在途手续费预留合计溢出".into()))?;
    }

    let candidate_amounts = candidate.exact_amounts.ok_or_else(|| {
        qx_core::QxError::BusinessViolation("精确衍生候选订单缺少 exact_amounts".into())
    })?;
    let candidate_price =
        conservative_derivative_price(spec, candidate_amounts.limit_price, reference_price)?;
    if !candidate_policy.reduce_only {
        projected = projected
            .checked_add(spec.initial_margin_exact(
                candidate_amounts.quantity,
                candidate_price,
                candidate_policy.leverage,
            )?)
            .ok_or_else(|| qx_core::QxError::Invariant("精确候选保证金合计溢出".into()))?;
    }
    projected_fees = projected_fees
        .checked_add(exact_fee_reserve(
            spec.notional_exact(candidate_amounts.quantity, candidate_price)?,
            fee_bps,
        )?)
        .ok_or_else(|| qx_core::QxError::Invariant("精确候选手续费预留溢出".into()))?;
    Ok((projected, projected_fees))
}

pub(super) fn marked_equity_exact(
    ledger: &Ledger,
    account_id: &str,
    spec: &TradingInstrumentSpec,
    mark: ExactDecimal,
) -> Result<ExactDecimal, qx_core::QxError> {
    if !mark.is_positive() {
        return Err(qx_core::QxError::BusinessViolation(
            "精确衍生权益估值需要正标记价".into(),
        ));
    }
    let raw_mark = Price::from_raw(exact_to_raw(mark, DecimalRounding::TowardZero)?);
    let marks = BTreeMap::from([(spec.instrument.clone(), raw_mark)]);
    let exact_marks = BTreeMap::from([(spec.instrument.clone(), mark)]);
    let valuation =
        ValuationContext::spot(account_id, &marks, &spec.settlement_currency).with_spec(Some(spec));
    ledger.valuate_exact(&ExactValuationContext::new(valuation).with_exact_marks(&exact_marks))
}

/// Exact shared-account margin projection across every derivative market. The portfolio
/// caller restricts the account to one settlement currency until cross-currency collateral
/// haircuts and FX risk are modeled.
#[allow(clippy::too_many_arguments)]
pub(super) fn projected_multi_market_margin_and_fees_exact(
    candidate: Option<&Order>,
    specs: &BTreeMap<qx_core::InstrumentId, TradingInstrumentSpec>,
    fee_bps_by_instrument: &BTreeMap<qx_core::InstrumentId, i128>,
    marks: &BTreeMap<qx_core::InstrumentId, ExactDecimal>,
    ledger: &Ledger,
    account_id: &str,
    open_orders: &[Order],
) -> Result<(ExactDecimal, ExactDecimal), qx_core::QxError> {
    if specs
        .values()
        .any(|spec| !spec.product.is_derivative() || spec.exact_rules.is_none())
    {
        return Err(qx_core::QxError::BusinessViolation(
            "共享精确组合保证金只接受带精确规则的永续/期货市场".into(),
        ));
    }
    let mut projected_margin = ExactDecimal::ZERO;
    for spec in specs.values() {
        let Some(mark) = marks.get(&spec.instrument).copied() else {
            if [
                qx_core::PositionSide::Net,
                qx_core::PositionSide::Long,
                qx_core::PositionSide::Short,
            ]
            .into_iter()
            .any(|side| {
                !ledger
                    .position_for_side_exact(account_id, &spec.instrument, side)
                    .quantity
                    .is_zero()
            }) {
                return Err(qx_core::QxError::ReconcileRequired(format!(
                    "共享精确保证金缺少持仓标记价: {}",
                    spec.instrument
                )));
            }
            continue;
        };
        if !mark.is_positive() {
            return Err(qx_core::QxError::BusinessViolation(format!(
                "共享精确保证金标记价必须为正: {}",
                spec.instrument
            )));
        }
        for side in [
            qx_core::PositionSide::Net,
            qx_core::PositionSide::Long,
            qx_core::PositionSide::Short,
        ] {
            let position_exact = ledger.position_for_side_exact(account_id, &spec.instrument, side);
            if position_exact.quantity.is_zero() {
                continue;
            }
            let position = ledger.position_for_side(account_id, &spec.instrument, side);
            let margin_ratio_raw = position.margin_ratio_raw.ok_or_else(|| {
                qx_core::QxError::ReconcileRequired(format!(
                    "共享精确保证金持仓 {} 缺少开仓杠杆事实",
                    spec.instrument
                ))
            })?;
            if !(1..=qx_core::SCALE).contains(&margin_ratio_raw) {
                return Err(qx_core::QxError::Invariant(format!(
                    "共享精确保证金比例非法: {} {margin_ratio_raw}",
                    spec.instrument
                )));
            }
            let conservative_ratio_raw = if margin_ratio_raw < qx_core::SCALE {
                margin_ratio_raw.checked_add(1).ok_or_else(|| {
                    qx_core::QxError::Invariant("共享保证金比例保守取整溢出".into())
                })?
            } else {
                margin_ratio_raw
            };
            let margin_ratio = ExactDecimal::from_parts(conservative_ratio_raw, 9)
                .map_err(|error| qx_core::QxError::Invariant(error.into()))?;
            let notional = spec.notional_exact(exact_abs(position_exact.quantity)?, mark)?;
            let margin = notional
                .checked_mul_at_scale(
                    margin_ratio,
                    ExactDecimal::MAX_SCALE,
                    DecimalRounding::AwayFromZero,
                )
                .ok_or_else(|| qx_core::QxError::Invariant("共享精确持仓保证金计算溢出".into()))?;
            projected_margin = projected_margin
                .checked_add(margin)
                .ok_or_else(|| qx_core::QxError::Invariant("共享精确持仓保证金合计溢出".into()))?;
        }
    }

    let mut projected_fees = ExactDecimal::ZERO;
    for order in open_orders.iter().filter(|order| {
        candidate.is_none_or(|candidate| order.client_id != candidate.client_id)
            && order.account_id == account_id
            && !order.status.is_terminal()
            && order.has_remaining_amount()
    }) {
        let Some(spec) = specs.get(&order.instrument) else {
            return Err(qx_core::QxError::ReconcileRequired(format!(
                "共享衍生在途订单缺少市场规格: {}",
                order.instrument
            )));
        };
        let amounts = order.exact_amounts.ok_or_else(|| {
            qx_core::QxError::BusinessViolation(format!(
                "共享衍生在途订单缺少 exact_amounts: {}",
                order.client_id
            ))
        })?;
        if amounts.quote_budget.is_some() {
            return Err(qx_core::QxError::BusinessViolation(
                "共享衍生在途订单不支持 quote-budget".into(),
            ));
        }
        let policy = order.policy.ok_or_else(|| {
            qx_core::QxError::BusinessViolation(format!(
                "共享衍生在途订单缺少 OrderPolicy: {}",
                order.client_id
            ))
        })?;
        policy.validate_for(spec)?;
        let mark = marks.get(&order.instrument).copied().ok_or_else(|| {
            qx_core::QxError::ReconcileRequired(format!(
                "共享衍生在途订单缺少精确标记价: {}",
                order.instrument
            ))
        })?;
        let remaining = order.remaining_decimal().ok_or_else(|| {
            qx_core::QxError::Invariant(format!("共享精确在途订单剩余量溢出: {}", order.client_id))
        })?;
        if !remaining.is_positive() {
            continue;
        }
        let price = conservative_derivative_price(spec, order.limit_price_decimal(), mark)?;
        if !policy.reduce_only {
            projected_margin = projected_margin
                .checked_add(spec.initial_margin_exact(remaining, price, policy.leverage)?)
                .ok_or_else(|| qx_core::QxError::Invariant("共享精确在途保证金合计溢出".into()))?;
        }
        let fee_bps = fee_bps_by_instrument
            .get(&order.instrument)
            .copied()
            .ok_or_else(|| {
                qx_core::QxError::ReconcileRequired(format!(
                    "共享衍生在途订单缺少手续费规则: {}",
                    order.instrument
                ))
            })?;
        projected_fees = projected_fees
            .checked_add(exact_fee_reserve(
                spec.notional_exact(remaining, price)?,
                fee_bps,
            )?)
            .ok_or_else(|| qx_core::QxError::Invariant("共享精确手续费合计溢出".into()))?;
    }

    if let Some(candidate) = candidate {
        let spec = specs.get(&candidate.instrument).ok_or_else(|| {
            qx_core::QxError::BusinessViolation(format!(
                "共享精确候选订单缺少市场规格: {}",
                candidate.instrument
            ))
        })?;
        let amounts = candidate.exact_amounts.ok_or_else(|| {
            qx_core::QxError::BusinessViolation("共享精确衍生候选订单缺少 exact_amounts".into())
        })?;
        if amounts.quote_budget.is_some() {
            return Err(qx_core::QxError::BusinessViolation(
                "共享精确衍生候选订单不支持 quote-budget".into(),
            ));
        }
        let policy = candidate.policy.ok_or_else(|| {
            qx_core::QxError::BusinessViolation("共享精确衍生候选订单缺少 OrderPolicy".into())
        })?;
        policy.validate_for(spec)?;
        let mark = marks.get(&candidate.instrument).copied().ok_or_else(|| {
            qx_core::QxError::ReconcileRequired(format!(
                "共享精确候选订单缺少精确标记价: {}",
                candidate.instrument
            ))
        })?;
        let quantity = candidate
            .remaining_decimal()
            .ok_or_else(|| qx_core::QxError::Invariant("共享精确候选订单剩余量溢出".into()))?;
        if !quantity.is_positive() {
            return Err(qx_core::QxError::BusinessViolation(
                "共享精确候选订单必须保留正数量".into(),
            ));
        }
        let price = conservative_derivative_price(spec, candidate.limit_price_decimal(), mark)?;
        if !policy.reduce_only {
            projected_margin = projected_margin
                .checked_add(spec.initial_margin_exact(quantity, price, policy.leverage)?)
                .ok_or_else(|| qx_core::QxError::Invariant("共享精确候选保证金合计溢出".into()))?;
        }
        let fee_bps = fee_bps_by_instrument
            .get(&candidate.instrument)
            .copied()
            .ok_or_else(|| {
                qx_core::QxError::ReconcileRequired(format!(
                    "共享精确候选订单缺少手续费规则: {}",
                    candidate.instrument
                ))
            })?;
        projected_fees = projected_fees
            .checked_add(exact_fee_reserve(
                spec.notional_exact(quantity, price)?,
                fee_bps,
            )?)
            .ok_or_else(|| qx_core::QxError::Invariant("共享精确候选手续费溢出".into()))?;
    }
    Ok((projected_margin, projected_fees))
}

pub(super) fn marked_multi_market_equity_exact(
    ledger: &Ledger,
    account_id: &str,
    reporting_currency: &str,
    specs: &BTreeMap<qx_core::InstrumentId, TradingInstrumentSpec>,
    marks: &BTreeMap<qx_core::InstrumentId, ExactDecimal>,
) -> Result<ExactDecimal, qx_core::QxError> {
    let raw_marks = marks
        .iter()
        .map(|(instrument, mark)| {
            Ok((
                instrument.clone(),
                Price::from_raw(exact_to_raw(*mark, DecimalRounding::TowardZero)?),
            ))
        })
        .collect::<Result<BTreeMap<_, _>, qx_core::QxError>>()?;
    let valuation =
        ValuationContext::spot(account_id, &raw_marks, reporting_currency).with_market_specs(specs);
    ledger.valuate_exact(&ExactValuationContext::new(valuation).with_exact_marks(marks))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn validate_multi_market_exact_margin_after_fill(
    order_after_fill: &Order,
    specs: &BTreeMap<qx_core::InstrumentId, TradingInstrumentSpec>,
    fee_bps_by_instrument: &BTreeMap<qx_core::InstrumentId, i128>,
    marks: &BTreeMap<qx_core::InstrumentId, ExactDecimal>,
    ledger: &Ledger,
    account_id: &str,
    reporting_currency: &str,
    open_orders: &[Order],
) -> Result<(), qx_core::QxError> {
    let candidate = order_after_fill
        .remaining_decimal()
        .filter(|remaining| remaining.is_positive())
        .map(|_| order_after_fill);
    let (required_margin, reserved_fees) = projected_multi_market_margin_and_fees_exact(
        candidate,
        specs,
        fee_bps_by_instrument,
        marks,
        ledger,
        account_id,
        open_orders,
    )?;
    let equity =
        marked_multi_market_equity_exact(ledger, account_id, reporting_currency, specs, marks)?;
    let available = equity
        .checked_sub(reserved_fees)
        .ok_or_else(|| qx_core::QxError::Invariant("共享成交后精确衍生可用权益计算溢出".into()))?;
    let margin_exceeds_available = required_margin
        .checked_cmp(available)
        .is_none_or(std::cmp::Ordering::is_gt);
    if available.is_negative() || margin_exceeds_available {
        return Err(qx_core::QxError::BusinessViolation(format!(
            "共享成交后精确组合保证金不足: 所需 {required_margin}，扣除手续费预留后权益 {available}"
        )));
    }
    Ok(())
}

/// Recheck an exact derivative account after a fill has been booked. Market orders can
/// execute at a materially different price from their submission reference, so admission
/// alone is not enough to guarantee that the resulting position remains funded.
pub(super) fn validate_exact_derivative_margin_after_fill(
    order_after_fill: &Order,
    spec: &TradingInstrumentSpec,
    mark: ExactDecimal,
    fee_bps: i128,
    ledger: &Ledger,
    account_id: &str,
    open_orders: &[Order],
) -> Result<(), qx_core::QxError> {
    if !spec.product.is_derivative() || order_after_fill.exact_amounts.is_none() {
        return Err(qx_core::QxError::BusinessViolation(
            "成交后精确保证金复核要求精确衍生订单".into(),
        ));
    }
    let remaining = order_after_fill
        .remaining_decimal()
        .ok_or_else(|| qx_core::QxError::Invariant("成交后精确订单剩余量溢出".into()))?;
    let mut candidate = order_after_fill.clone();
    let amounts = candidate
        .exact_amounts
        .as_mut()
        .expect("exact derivative order was checked above");
    amounts.quantity = remaining;
    amounts.filled = ExactDecimal::ZERO;
    amounts.quote_budget = None;
    amounts.quote_filled = ExactDecimal::ZERO;
    if amounts.limit_price.is_none() {
        amounts.market_reference_price = Some(mark);
    }

    let (required_margin, reserved_fees) = projected_margin_and_fees_exact(
        &candidate,
        candidate.policy.unwrap_or_default(),
        spec,
        mark,
        fee_bps,
        ledger,
        account_id,
        open_orders,
    )?;
    let equity = marked_equity_exact(ledger, account_id, spec, mark)?;
    let available = equity
        .checked_sub(reserved_fees)
        .ok_or_else(|| qx_core::QxError::Invariant("成交后精确衍生可用权益计算溢出".into()))?;
    let margin_exceeds_available = required_margin
        .checked_cmp(available)
        .is_none_or(std::cmp::Ordering::is_gt);
    if available.is_negative() || margin_exceeds_available {
        return Err(qx_core::QxError::BusinessViolation(format!(
            "成交后精确组合保证金不足: 所需 {required_margin}，扣除手续费预留后权益 {available}"
        )));
    }
    Ok(())
}

pub(super) fn conservative_derivative_price(
    spec: &TradingInstrumentSpec,
    limit_price: Option<ExactDecimal>,
    reference_price: ExactDecimal,
) -> Result<ExactDecimal, qx_core::QxError> {
    let Some(limit_price) = limit_price else {
        return Ok(reference_price);
    };
    let ordering = limit_price
        .checked_cmp(reference_price)
        .ok_or_else(|| qx_core::QxError::Invariant("精确衍生风险价格比较失败".into()))?;
    if spec.inverse {
        Ok(if ordering.is_lt() {
            limit_price
        } else {
            reference_price
        })
    } else {
        Ok(if ordering.is_gt() {
            limit_price
        } else {
            reference_price
        })
    }
}

fn exact_abs(value: ExactDecimal) -> Result<ExactDecimal, qx_core::QxError> {
    if value.is_negative() {
        value
            .checked_neg()
            .ok_or_else(|| qx_core::QxError::Invariant("精确持仓绝对值溢出".into()))
    } else {
        Ok(value)
    }
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
    pub(super) reference_price_exact: Option<ExactDecimal>,
    pub(super) ledger: &'a Ledger,
    pub(super) account_id: &'a str,
    pub(super) currency: &'a str,
    pub(super) fee_bps: i128,
    pub(super) reservations: &'a SpotCashReservations,
    pub(super) total_reserved_cash_raw: Option<i128>,
    /// Shared-account exact cash reservations across all markets. `None` means the
    /// single-market caller should use this market's reservations only.
    pub(super) total_reserved_cash_exact: Option<ExactDecimal>,
}

pub(super) struct FillFundingContext<'a> {
    pub(super) spec: Option<&'a TradingInstrumentSpec>,
    pub(super) cash_before_fill: i128,
    pub(super) total_reserved_before_fill: i128,
    pub(super) inventory_before_fill: i128,
    pub(super) total_inventory_reserved_before_fill: i128,
    pub(super) cash_before_fill_exact: ExactDecimal,
    pub(super) total_reserved_cash_exact_before_fill: ExactDecimal,
    pub(super) inventory_before_fill_exact: ExactDecimal,
}

pub(super) fn preflight_order(
    order: &Order,
    policy: OrderPolicy,
    spec: Option<&TradingInstrumentSpec>,
    context: FundingContext<'_>,
) -> Result<FundingDecision, qx_core::QxError> {
    if let Some(amounts) = order.exact_amounts {
        if spec.is_some_and(|spec| spec.product.is_derivative()) {
            return preflight_exact_derivative_order(order, amounts, policy, spec, context);
        }
        return preflight_exact_spot_order(order, amounts, policy, spec, context);
    }
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
    let notional = qx_core::fee::notional(order.qty.raw(), price.raw())?;
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

fn preflight_exact_derivative_order(
    order: &Order,
    amounts: qx_core::ExactOrderAmounts,
    policy: OrderPolicy,
    spec: Option<&TradingInstrumentSpec>,
    context: FundingContext<'_>,
) -> Result<FundingDecision, qx_core::QxError> {
    if let Err(error) = order.validate() {
        return Ok(FundingDecision::Reject(error));
    }
    let Some(spec) = spec else {
        return Ok(FundingDecision::Reject(
            "精确衍生订单簿订单缺少冻结 TradingInstrumentSpec".into(),
        ));
    };
    if !spec.product.is_derivative() || spec.exact_rules.is_none() {
        return Ok(FundingDecision::Reject(
            "精确衍生订单簿资金校验需要带精确规则的永续或期货规格".into(),
        ));
    }
    if amounts.quote_budget.is_some()
        || !amounts.filled.is_zero()
        || !amounts.quote_filled.is_zero()
    {
        return Ok(FundingDecision::Reject(
            "精确衍生深度订单只支持未成交的基础数量订单".into(),
        ));
    }
    if let Err(error) = policy.validate_for(spec) {
        return Ok(FundingDecision::Reject(error.to_string()));
    }
    let Some(reference_price) = context.reference_price_exact else {
        return Ok(FundingDecision::Reject(
            "精确衍生订单簿资金校验缺少精确参考价".into(),
        ));
    };
    let price = amounts.limit_price.unwrap_or(reference_price);
    if let Err(error) =
        spec.validate_exact_order_with_reference(amounts.quantity, amounts.limit_price, Some(price))
    {
        return Ok(FundingDecision::Reject(error.to_string()));
    }

    // The canonical risk gate computes aggregate exact margin and fee exposure for the
    // filled portfolio, all accepted orders, and this candidate. Do not reserve derivative
    // notional as spot cash or compare this order in isolation.
    Ok(FundingDecision::NotRequired)
}

fn preflight_exact_spot_order(
    order: &Order,
    amounts: qx_core::ExactOrderAmounts,
    policy: OrderPolicy,
    spec: Option<&TradingInstrumentSpec>,
    context: FundingContext<'_>,
) -> Result<FundingDecision, qx_core::QxError> {
    if let Err(error) = order.validate() {
        return Ok(FundingDecision::Reject(error));
    }
    let Some(spec) = spec else {
        return Ok(FundingDecision::Reject(
            "精确订单簿订单缺少冻结 TradingInstrumentSpec".into(),
        ));
    };
    if spec.product != qx_core::TradingProduct::Spot {
        return Ok(FundingDecision::Reject(
            "订单簿精确资金预留当前只支持现货".into(),
        ));
    }
    if spec.exact_rules.is_none() {
        return Ok(FundingDecision::Reject(
            "精确订单簿资金预留需要冻结 tick/step 规则".into(),
        ));
    }
    if !amounts.filled.is_zero() || !amounts.quote_filled.is_zero() {
        return Ok(FundingDecision::Reject(
            "深度回测只允许提交尚未成交的精确新订单".into(),
        ));
    }
    if order.side == qx_core::Side::Buy && policy.reduce_only {
        return Ok(FundingDecision::Reject(
            "现货精确买单不能标记 reduce_only".into(),
        ));
    }
    if let Err(error) = policy.validate_for(spec) {
        return Ok(FundingDecision::Reject(error.to_string()));
    }
    let Some(reference_price) = context.reference_price_exact else {
        return Ok(FundingDecision::Reject(
            "精确订单簿资金预留缺少精确参考价".into(),
        ));
    };
    let side_price = match order.side {
        qx_core::Side::Buy => context
            .snapshot
            .asks
            .first()
            .map(|level| level.price_decimal()),
        qx_core::Side::Sell => context
            .snapshot
            .bids
            .first()
            .map(|level| level.price_decimal()),
    };
    let price = amounts
        .limit_price
        .or(side_price)
        .or(amounts.market_reference_price)
        .unwrap_or(reference_price);
    let reserve_quantity = if let Some(budget) = amounts.quote_budget {
        if order.side != qx_core::Side::Buy || policy.reduce_only {
            return Ok(FundingDecision::Reject(
                "计价币预算订单只支持非 reduce_only 的现货买入".into(),
            ));
        }
        if let Err(error) = spec.validate_exact_quote_budget(budget) {
            return Ok(FundingDecision::Reject(error.to_string()));
        }
        let reference = amounts
            .market_reference_price
            .or(side_price)
            .unwrap_or(price);
        let target = match spec.base_quantity_for_quote_budget(budget, reference) {
            Ok(target) => target,
            Err(error) => return Ok(FundingDecision::Reject(error.to_string())),
        };
        if let Err(error) = spec.validate_exact_order_with_reference(target, None, Some(reference))
        {
            return Ok(FundingDecision::Reject(error.to_string()));
        }
        ExactDecimal::ZERO
    } else {
        if let Err(error) = spec.validate_exact_order_with_reference(
            amounts.quantity,
            amounts.limit_price,
            Some(price),
        ) {
            return Ok(FundingDecision::Reject(error.to_string()));
        }
        amounts.quantity
    };

    let reserve_inventory = if order.side == qx_core::Side::Sell {
        let inventory = context
            .ledger
            .position_for_side_exact(
                context.account_id,
                &order.instrument,
                qx_core::PositionSide::Net,
            )
            .quantity;
        let available = inventory
            .checked_sub(context.reservations.total_inventory_exact()?)
            .ok_or_else(|| qx_core::QxError::Invariant("精确现货可用库存计算溢出".into()))?;
        if amounts
            .quantity
            .checked_cmp(available)
            .is_none_or(|ordering| ordering.is_gt())
        {
            return Ok(FundingDecision::Reject(
                "精确现货卖出数量超过扣除待成交卖单后的可用持仓".into(),
            ));
        }
        amounts.quantity
    } else {
        ExactDecimal::ZERO
    };

    let reserve_cash = if order.side == qx_core::Side::Buy && !policy.reduce_only {
        let notional = if let Some(budget) = amounts.quote_budget {
            budget
        } else {
            spec.notional_exact(amounts.quantity, price)?
        };
        let quote_cost = if let Some(budget) = amounts.quote_budget {
            budget
        } else {
            spec.quote_cash_exact(amounts.quantity, price, DecimalRounding::AwayFromZero)?
        };
        let fee = exact_fee_reserve(notional, context.fee_bps)?;
        let required = quote_cost
            .checked_add(fee)
            .ok_or_else(|| qx_core::QxError::Invariant("精确买入资金预留溢出".into()))?;
        let available = context
            .ledger
            .cash_for_exact(context.account_id, context.currency)
            .checked_sub(match context.total_reserved_cash_exact {
                Some(total) => total,
                None => context.reservations.total_cash_exact()?,
            })
            .ok_or_else(|| qx_core::QxError::Invariant("精确可用现金计算溢出".into()))?;
        if required
            .checked_cmp(available)
            .is_none_or(|ordering| ordering.is_gt())
        {
            return Ok(FundingDecision::Reject(
                "精确买入成本（含预估手续费）超过订单簿回测可用现金".into(),
            ));
        }
        required
    } else {
        ExactDecimal::ZERO
    };

    Ok(FundingDecision::AcceptExact {
        reserve_quantity,
        reserve_cash,
        reserve_inventory,
    })
}

fn exact_fee_reserve(
    notional: ExactDecimal,
    fee_bps: i128,
) -> Result<ExactDecimal, qx_core::QxError> {
    let bps = ExactDecimal::from_parts(fee_bps, 0)
        .map_err(|error| qx_core::QxError::Invariant(error.into()))?;
    let denominator = ExactDecimal::from_parts(10_000, 0)
        .map_err(|error| qx_core::QxError::Invariant(error.into()))?;
    notional
        .checked_mul_at_scale(bps, ExactDecimal::MAX_SCALE, DecimalRounding::AwayFromZero)
        .and_then(|value| {
            value.checked_div_at_scale(
                denominator,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::AwayFromZero,
            )
        })
        .ok_or_else(|| qx_core::QxError::Invariant("精确手续费预留计算溢出".into()))
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

    pub(super) fn total_cash_exact(&self) -> Result<ExactDecimal, qx_core::QxError> {
        self.by_order
            .values()
            .try_fold(ExactDecimal::ZERO, |total, reservation| {
                let amount = match reservation.exact {
                    Some(exact) => exact.remaining_cash,
                    None => exact_from_raw(reservation.remaining_cash_raw)?,
                };
                total
                    .checked_add(amount)
                    .ok_or_else(|| qx_core::QxError::Invariant("精确现货现金预留求和溢出".into()))
            })
    }

    pub(super) fn total_inventory_exact(&self) -> Result<ExactDecimal, qx_core::QxError> {
        self.by_order
            .values()
            .try_fold(ExactDecimal::ZERO, |total, reservation| {
                let amount = match reservation.exact {
                    Some(exact) => exact.remaining_inventory,
                    None => exact_from_raw(reservation.remaining_inventory_raw)?,
                };
                total
                    .checked_add(amount)
                    .ok_or_else(|| qx_core::QxError::Invariant("精确现货库存预留求和溢出".into()))
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
                exact: None,
            },
        );
        Ok(())
    }

    pub(super) fn reserve_exact(
        &mut self,
        client_id: u64,
        quantity: ExactDecimal,
        cash: ExactDecimal,
        inventory: ExactDecimal,
        quote_budget: Option<ExactDecimal>,
    ) -> Result<(), qx_core::QxError> {
        if (quote_budget.is_some() && !quantity.is_zero())
            || (quote_budget.is_none() && !quantity.is_positive())
            || quote_budget.is_some_and(|budget| !budget.is_positive())
            || cash.is_negative()
            || inventory.is_negative()
            || (!cash.is_positive() && !inventory.is_positive())
            || self.by_order.contains_key(&client_id)
        {
            return Err(qx_core::QxError::Invariant(format!(
                "精确现货挂单资金预留身份或金额非法: client_id={client_id}"
            )));
        }
        self.by_order.insert(
            client_id,
            Reservation {
                remaining_qty_raw: exact_to_raw(quantity, DecimalRounding::Ceiling)?,
                remaining_cash_raw: exact_to_raw(cash, DecimalRounding::Ceiling)?,
                remaining_inventory_raw: exact_to_raw(inventory, DecimalRounding::Ceiling)?,
                exact: Some(ExactReservation {
                    remaining_qty: quantity,
                    remaining_cash: cash,
                    remaining_inventory: inventory,
                    remaining_quote_budget: quote_budget,
                }),
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
        if let Some(fill_amounts) = fill.exact_amounts {
            let Some(mut exact_reservation) = reservation.exact else {
                return Err(qx_core::QxError::ReconcileRequired(format!(
                    "精确盘口成交缺少精确资金预留: client_id={}",
                    order.client_id
                )));
            };
            let quote_budget_order = exact_reservation.remaining_quote_budget.is_some();
            if !fill_amounts.quantity.is_positive()
                || (!quote_budget_order
                    && fill_amounts
                        .quantity
                        .checked_cmp(exact_reservation.remaining_qty)
                        .is_none_or(|ordering| ordering.is_gt()))
            {
                return Err(qx_core::QxError::ReconcileRequired(format!(
                    "精确现货成交数量超出预留: client_id={}",
                    order.client_id
                )));
            }
            let remaining_qty = if quote_budget_order {
                let budget = exact_reservation
                    .remaining_quote_budget
                    .expect("quote budget reservation checked above");
                let quote_cost = fill_amounts.quote_cost.ok_or_else(|| {
                    qx_core::QxError::ReconcileRequired(
                        "计价币预算订单成交缺少精确计价币金额".into(),
                    )
                })?;
                if quote_cost
                    .checked_cmp(budget)
                    .is_none_or(|ordering| ordering.is_gt())
                {
                    return Err(qx_core::QxError::BusinessViolation(
                        "精确成交计价币金额超过订单预算".into(),
                    ));
                }
                exact_reservation.remaining_quote_budget =
                    Some(budget.checked_sub(quote_cost).ok_or_else(|| {
                        qx_core::QxError::Invariant("精确剩余计价币预算计算溢出".into())
                    })?);
                ExactDecimal::ZERO
            } else {
                exact_reservation
                    .remaining_qty
                    .checked_sub(fill_amounts.quantity)
                    .ok_or_else(|| qx_core::QxError::Invariant("精确剩余订单数量计算溢出".into()))?
            };

            if exact_reservation.remaining_cash.is_positive() {
                let spec = context.spec.ok_or_else(|| {
                    qx_core::QxError::BusinessViolation(
                        "精确现货成交资金校验缺少 TradingInstrumentSpec".into(),
                    )
                })?;
                let quote_cost = match fill_amounts.quote_cost {
                    Some(cost) => cost,
                    None => spec.quote_cash_exact(
                        fill_amounts.quantity,
                        fill_amounts.price,
                        DecimalRounding::AwayFromZero,
                    )?,
                };
                let mut fee = ExactDecimal::ZERO;
                if !fill.exact_fees.is_empty() {
                    for charge in &fill.exact_fees {
                        if charge.currency.as_deref().is_none_or(|currency| {
                            !currency.eq_ignore_ascii_case(&spec.quote_currency)
                        }) {
                            return Err(qx_core::QxError::BusinessViolation(
                                "订单簿精确买入资金预留只支持报价币手续费".into(),
                            ));
                        }
                        fee = fee.checked_add(charge.amount).ok_or_else(|| {
                            qx_core::QxError::Invariant("精确手续费求和溢出".into())
                        })?;
                    }
                } else if fill.fee.raw() > 0 {
                    fee = exact_from_raw(fill.fee.raw())?;
                }
                let actual_cost = quote_cost
                    .checked_add(fee)
                    .ok_or_else(|| qx_core::QxError::Invariant("精确成交成本溢出".into()))?;
                exact_reservation.remaining_cash = exact_reservation
                    .remaining_cash
                    .checked_sub(actual_cost)
                    .filter(|remaining| !remaining.is_negative())
                    .ok_or_else(|| {
                        qx_core::QxError::BusinessViolation(
                            "精确成交成本超过该订单的资金预留".into(),
                        )
                    })?;
                let original = reservation.exact.expect("exact reservation exists");
                let other_reserved = context
                    .total_reserved_cash_exact_before_fill
                    .checked_sub(original.remaining_cash)
                    .ok_or_else(|| {
                        qx_core::QxError::Invariant("精确单笔现金预留超过共享总预留".into())
                    })?;
                let cash_after = context
                    .cash_before_fill_exact
                    .checked_sub(actual_cost)
                    .ok_or_else(|| qx_core::QxError::Invariant("成交后精确现金计算溢出".into()))?;
                let reserved_after = other_reserved
                    .checked_add(exact_reservation.remaining_cash)
                    .ok_or_else(|| qx_core::QxError::Invariant("成交后精确预留求和溢出".into()))?;
                if cash_after
                    .checked_cmp(reserved_after)
                    .is_none_or(|ordering| ordering.is_lt())
                {
                    return Err(qx_core::QxError::BusinessViolation(
                        "精确成交后现金不足以覆盖其他挂单及本单剩余预留".into(),
                    ));
                }
            }

            if exact_reservation.remaining_inventory.is_positive() {
                let original = reservation.exact.expect("exact reservation exists");
                exact_reservation.remaining_inventory = original
                    .remaining_inventory
                    .checked_sub(fill_amounts.quantity)
                    .filter(|remaining| !remaining.is_negative())
                    .ok_or_else(|| {
                        qx_core::QxError::BusinessViolation("精确现货卖出成交超过库存预留".into())
                    })?;
                let other_reserved = self
                    .total_inventory_exact()?
                    .checked_sub(original.remaining_inventory)
                    .ok_or_else(|| {
                        qx_core::QxError::Invariant("精确单笔库存预留超过总预留".into())
                    })?;
                let inventory_after = context
                    .inventory_before_fill_exact
                    .checked_sub(fill_amounts.quantity)
                    .ok_or_else(|| qx_core::QxError::Invariant("成交后精确库存计算溢出".into()))?;
                let reserved_after = other_reserved
                    .checked_add(exact_reservation.remaining_inventory)
                    .ok_or_else(|| {
                        qx_core::QxError::Invariant("成交后精确库存预留求和溢出".into())
                    })?;
                if inventory_after
                    .checked_cmp(reserved_after)
                    .is_none_or(|ordering| ordering.is_lt())
                {
                    return Err(qx_core::QxError::BusinessViolation(
                        "精确卖出成交后持仓不足以覆盖其他待成交卖单".into(),
                    ));
                }
            }

            exact_reservation.remaining_qty = remaining_qty;
            let has_remaining_quote_budget = exact_reservation
                .remaining_quote_budget
                .is_some_and(ExactDecimal::is_positive);
            let retain_reservation = if quote_budget_order {
                has_remaining_quote_budget && !fill_amounts.quote_budget_complete
            } else {
                remaining_qty.is_positive()
            };
            let reservation = if retain_reservation {
                Some(Reservation {
                    remaining_qty_raw: exact_to_raw(remaining_qty, DecimalRounding::Ceiling)?,
                    remaining_cash_raw: exact_to_raw(
                        exact_reservation.remaining_cash,
                        DecimalRounding::Ceiling,
                    )?,
                    remaining_inventory_raw: exact_to_raw(
                        exact_reservation.remaining_inventory,
                        DecimalRounding::Ceiling,
                    )?,
                    exact: Some(exact_reservation),
                })
            } else {
                None
            };
            return Ok(Some(ReservationUpdate {
                client_id: order.client_id,
                reservation,
            }));
        }
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
                None => qx_core::fee::notional(fill.qty.raw(), fill.price.raw())?,
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
                exact: None,
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

fn exact_from_raw(value: i128) -> Result<ExactDecimal, qx_core::QxError> {
    ExactDecimal::from_parts(value, 9).map_err(|error| qx_core::QxError::Invariant(error.into()))
}

pub(super) fn exact_to_raw(
    value: ExactDecimal,
    rounding: DecimalRounding,
) -> Result<i128, qx_core::QxError> {
    value
        .checked_mul_at_scale(ExactDecimal::ONE, 9, rounding)
        .and_then(|projected| projected.to_raw(9).ok())
        .ok_or_else(|| qx_core::QxError::Invariant("精确预留投影到 raw 失败".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{
        ExactOrderAmounts, ExactTradingRules, InstrumentId, MarginMode, OrderStatus, PositionMode,
        PositionSide, Quantity, Side, TradingProduct, SCALE,
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
            min_notional_raw: 0,
            max_leverage: 10,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
            exact_rules: None,
        }
    }

    #[test]
    fn exact_derivative_risk_uses_adverse_limit_side_for_linear_and_inverse_contracts() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let linear = derivative_spec(instrument.clone());
        let inverse = TradingInstrumentSpec {
            linear: false,
            inverse: true,
            settlement_currency: "BTC".into(),
            ..derivative_spec(instrument)
        };
        let reference = ExactDecimal::parse("100").unwrap();
        let lower = ExactDecimal::parse("90").unwrap();
        let higher = ExactDecimal::parse("110").unwrap();

        assert_eq!(
            conservative_derivative_price(&linear, Some(lower), reference).unwrap(),
            reference
        );
        assert_eq!(
            conservative_derivative_price(&linear, Some(higher), reference).unwrap(),
            higher
        );
        assert_eq!(
            conservative_derivative_price(&inverse, Some(lower), reference).unwrap(),
            lower
        );
        assert_eq!(
            conservative_derivative_price(&inverse, Some(higher), reference).unwrap(),
            reference
        );
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
            exact_amounts: None,
        }
    }

    #[test]
    fn exact_derivative_projection_aggregates_positions_open_orders_and_candidate() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let mut spec = derivative_spec(instrument.clone());
        spec.exact_rules = Some(ExactTradingRules {
            contract_size: ExactDecimal::ONE,
            price_tick: ExactDecimal::parse("0.0000000001").unwrap(),
            quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        });
        let mut ledger = Ledger::new();
        let mut opened = derivative_order(10, &instrument, 1);
        opened.policy = Some(OrderPolicy {
            leverage: 3,
            ..opened.policy.unwrap()
        });
        ledger
            .apply_fill_with_spec(
                &opened,
                &Fill {
                    order_id: opened.client_id,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(100),
                    ts: 1,
                    account_id: "main".into(),
                    ..Fill::default()
                },
                "USDT",
                &spec,
            )
            .unwrap();

        let mut pending = derivative_order(11, &instrument, 1);
        pending.policy = Some(OrderPolicy {
            leverage: 5,
            ..pending.policy.unwrap()
        });
        let mut candidate = derivative_order(12, &instrument, 0);
        candidate.policy = Some(OrderPolicy {
            leverage: 10,
            ..candidate.policy.unwrap()
        });
        candidate.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse("0.0000000002").unwrap(),
            limit_price: None,
            filled: ExactDecimal::ZERO,
            quote_budget: None,
            quote_filled: ExactDecimal::ZERO,
            market_reference_price: Some(ExactDecimal::parse("100").unwrap()),
        });
        candidate.validate().unwrap();
        let (margin, fees) = projected_margin_and_fees_exact(
            &candidate,
            candidate.policy.unwrap(),
            &spec,
            ExactDecimal::parse("100").unwrap(),
            100,
            &ledger,
            "main",
            std::slice::from_ref(&pending),
        )
        .unwrap();

        assert_eq!(margin, ExactDecimal::parse("53.333333402").unwrap());
        assert_eq!(fees, ExactDecimal::parse("1.0000000002").unwrap());
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
                exact_amounts: None,
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
