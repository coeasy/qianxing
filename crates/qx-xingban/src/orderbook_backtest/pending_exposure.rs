use super::*;

pub(super) struct PendingExposureContext<'a> {
    pub risk: &'a RiskGate,
    pub oms: &'a Oms,
    pub candidate_order_id: u64,
    pub account_id: &'a str,
    pub instrument: &'a qx_core::InstrumentId,
    pub spec: Option<&'a TradingInstrumentSpec>,
    pub reference_price: Option<Price>,
}

pub(super) fn position_snapshot(
    ledger: &Ledger,
    net_position: i128,
    context: PendingExposureContext<'_>,
) -> Result<OrderRiskPosition, qx_core::QxError> {
    let long_qty = ledger
        .position_for_side(
            context.account_id,
            context.instrument,
            qx_core::PositionSide::Long,
        )
        .quantity
        .raw();
    let short_qty = ledger
        .position_for_side(
            context.account_id,
            context.instrument,
            qx_core::PositionSide::Short,
        )
        .quantity
        .raw();
    let one_way_qty = net_position
        .checked_sub(long_qty)
        .and_then(|value| value.checked_sub(short_qty))
        .ok_or_else(|| {
            qx_core::QxError::Invariant("拆分订单簿 one-way/hedge 持仓数量溢出".into())
        })?;
    if let Some(spec) = context.spec {
        let filled_gross_notional = if let Some(price) = context.reference_price {
            let one_way = spec.notional(one_way_qty.saturating_abs(), price.raw())?;
            let long = spec.notional(long_qty.saturating_abs(), price.raw())?;
            let short = spec.notional(short_qty.saturating_abs(), price.raw())?;
            one_way
                .checked_add(long)
                .and_then(|value| value.checked_add(short))
                .ok_or_else(|| {
                    qx_core::QxError::Invariant("订单簿 hedge gross notional 溢出".into())
                })?
        } else {
            0
        };
        let gross_notional = gross_notional_with_pending_orders(
            filled_gross_notional,
            PendingExposureContext {
                risk: context.risk,
                oms: context.oms,
                candidate_order_id: context.candidate_order_id,
                account_id: context.account_id,
                instrument: context.instrument,
                spec: Some(spec),
                reference_price: context.reference_price,
            },
        )?;
        Ok(
            OrderRiskPosition::new_with_multiplier(one_way_qty, gross_notional, spec.contract_size)
                .with_hedge_legs(long_qty, short_qty),
        )
    } else {
        Ok(OrderRiskPosition::new(one_way_qty, 0).with_hedge_legs(long_qty, short_qty))
    }
}

pub(super) fn gross_notional_with_pending_orders(
    filled_gross_notional: i128,
    context: PendingExposureContext<'_>,
) -> Result<i128, qx_core::QxError> {
    let pending_gross_notional = if context.risk.rule_set().has_rule("MaxNotional") {
        let marks = context
            .reference_price
            .map(|price| BTreeMap::from([(context.instrument.clone(), price)]))
            .unwrap_or_default();
        qx_risk::pending_order_gross_notional(
            &context.oms.all_orders(),
            context.candidate_order_id,
            context.account_id,
            context.instrument,
            context.spec.ok_or_else(|| {
                qx_core::QxError::Invariant("缺少订单簿风险限额的交易品种规格".into())
            })?,
            &marks,
        )?
    } else {
        0
    };
    filled_gross_notional
        .checked_add(pending_gross_notional)
        .ok_or_else(|| qx_core::QxError::Invariant("订单簿持仓与在途订单名义额合计溢出".into()))
}
