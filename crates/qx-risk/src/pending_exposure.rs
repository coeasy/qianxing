use qx_core::{InstrumentId, Order, Price, QxError, QxResult, TradingInstrumentSpec};
use std::collections::BTreeMap;

/// Sum remaining notional for live orders in the candidate's account and instrument.
///
/// This is a risk reservation, not an extra position: it is added to the filled
/// gross notional before `OrderRiskContext::project_exposure` evaluates a new order.
pub fn pending_order_gross_notional(
    orders: &[Order],
    candidate_order_id: u64,
    account_id: &str,
    instrument: &InstrumentId,
    spec: &TradingInstrumentSpec,
    marks: &BTreeMap<InstrumentId, Price>,
) -> QxResult<i128> {
    pending_order_gross_notional_at_reference_mark(
        orders,
        candidate_order_id,
        account_id,
        instrument,
        spec,
        marks.get(instrument).copied(),
    )
}

/// Sum remaining notional using a caller-provided reference mark.
///
/// This avoids allocating a one-entry mark map when a caller already has the
/// candidate instrument's visible price.
pub fn pending_order_gross_notional_at_reference_mark<'a>(
    orders: impl IntoIterator<Item = &'a Order>,
    candidate_order_id: u64,
    account_id: &str,
    instrument: &InstrumentId,
    spec: &TradingInstrumentSpec,
    reference_mark: Option<Price>,
) -> QxResult<i128> {
    if spec.instrument != *instrument {
        return Err(QxError::BusinessViolation(
            "在途订单名义额规格与目标标的不一致".into(),
        ));
    }
    orders
        .into_iter()
        .filter(|order| {
            order.client_id != candidate_order_id
                && order.account_id == account_id
                && order.instrument == *instrument
                && !order.status.is_terminal()
                && order.remaining().raw() > 0
        })
        .try_fold(0_i128, |total, order| {
            let quantity = order.remaining().raw();
            let limit_notional = order
                .limit
                .map(|price| spec.notional(quantity, price.raw()))
                .transpose()?;
            let mark_notional = reference_mark
                .map(|price| spec.notional(quantity, price.raw()))
                .transpose()?;
            let notional = match (limit_notional, mark_notional) {
                (Some(limit), Some(mark)) => limit.max(mark),
                (Some(limit), None) => limit,
                (None, Some(mark)) => mark,
                (None, None) => {
                    return Err(QxError::BusinessViolation(format!(
                        "在途市价订单 {} 缺少名义额风控参考价",
                        order.client_id
                    )))
                }
            };
            total
                .checked_add(notional)
                .ok_or_else(|| QxError::Invariant("在途订单名义额合计溢出".into()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{OrderStatus, Quantity, Side, TradingProduct, SCALE};

    fn spec(instrument: &InstrumentId) -> TradingInstrumentSpec {
        TradingInstrumentSpec {
            instrument: instrument.clone(),
            product: TradingProduct::Spot,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 1,
            maintenance_margin_bps: 0,
            valid_from: 1,
            valid_to: None,
        }
    }

    fn order(
        client_id: u64,
        instrument: &InstrumentId,
        account_id: &str,
        status: OrderStatus,
        qty: i64,
        filled: i64,
        limit: Option<i64>,
    ) -> Order {
        Order {
            client_id,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(qty),
            limit: limit.map(Price::from_i64),
            status,
            filled: Quantity::from_i64(filled),
            account_id: account_id.into(),
            trace: None,
            policy: None,
        }
    }

    #[test]
    fn reserves_remaining_same_account_orders_and_ignores_other_or_terminal_orders() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let other_instrument = InstrumentId::parse("ETHUSDT.BINANCE").unwrap();
        let candidate_id = 99;
        let orders = vec![
            order(
                1,
                &instrument,
                "main",
                OrderStatus::PartiallyFilled,
                3,
                1,
                Some(10),
            ),
            order(
                candidate_id,
                &instrument,
                "main",
                OrderStatus::Accepted,
                50,
                0,
                Some(10),
            ),
            order(
                2,
                &instrument,
                "other-account",
                OrderStatus::Accepted,
                50,
                0,
                Some(10),
            ),
            order(
                3,
                &other_instrument,
                "main",
                OrderStatus::Accepted,
                50,
                0,
                Some(10),
            ),
            order(
                4,
                &instrument,
                "main",
                OrderStatus::Cancelled,
                50,
                0,
                Some(10),
            ),
        ];
        let actual = pending_order_gross_notional(
            &orders,
            candidate_id,
            "main",
            &instrument,
            &spec(&instrument),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(actual, 20 * SCALE, "仅计入部分成交订单剩余的 2 × 10");
    }

    #[test]
    fn market_order_without_mark_fails_closed() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let market_order = order(1, &instrument, "main", OrderStatus::Accepted, 1, 0, None);
        assert!(pending_order_gross_notional(
            &[market_order],
            2,
            "main",
            &instrument,
            &spec(&instrument),
            &BTreeMap::new(),
        )
        .is_err());
    }

    #[test]
    fn pending_limit_reservation_uses_the_larger_limit_or_mark_notional() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let sell_limit_below_mark =
            order(1, &instrument, "main", OrderStatus::Accepted, 2, 0, Some(9));
        let marks = BTreeMap::from([(instrument.clone(), Price::from_i64(10))]);

        assert_eq!(
            pending_order_gross_notional(
                &[sell_limit_below_mark],
                2,
                "main",
                &instrument,
                &spec(&instrument),
                &marks,
            )
            .unwrap(),
            20 * SCALE
        );
    }
}
