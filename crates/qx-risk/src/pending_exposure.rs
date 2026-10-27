use qx_core::{ExactDecimal, InstrumentId, Order, Price, QxError, QxResult, TradingInstrumentSpec};
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
    orders.into_iter().try_fold(0_i128, |total, order| {
        if order.client_id == candidate_order_id
            || order.account_id != account_id
            || order.instrument != *instrument
            || order.status.is_terminal()
        {
            return Ok(total);
        }
        order
            .require_legacy_amounts()
            .map_err(QxError::BusinessViolation)?;
        if order.remaining().raw() <= 0 {
            return Ok(total);
        }
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
                )));
            }
        };
        total
            .checked_add(notional)
            .ok_or_else(|| QxError::Invariant("在途订单名义额合计溢出".into()))
    })
}

/// Scale-aware counterpart to [`pending_order_gross_notional_at_reference_mark`].
///
/// This reserves the worst of each pending order's exact limit notional and the supplied
/// reference-mark notional. Legacy and exact orders can coexist in an account event log; both
/// are read through `Order`'s decimal accessors so no order is silently treated as zero.
pub fn pending_order_gross_notional_exact_at_reference_mark<'a>(
    orders: impl IntoIterator<Item = &'a Order>,
    candidate_order_id: u64,
    account_id: &str,
    instrument: &InstrumentId,
    spec: &TradingInstrumentSpec,
    reference_mark: Option<ExactDecimal>,
) -> QxResult<ExactDecimal> {
    if spec.instrument != *instrument {
        return Err(QxError::BusinessViolation(
            "精确在途订单名义额规格与目标标的不一致".into(),
        ));
    }
    orders
        .into_iter()
        .try_fold(ExactDecimal::ZERO, |total, order| {
            if order.client_id == candidate_order_id
                || order.account_id != account_id
                || order.instrument != *instrument
                || order.status.is_terminal()
            {
                return Ok(total);
            }
            if !order.has_remaining_amount() {
                return Ok(total);
            }
            if let Some(remaining_budget) = order.remaining_budget_decimal() {
                if spec.product != qx_core::TradingProduct::Spot || order.side != qx_core::Side::Buy
                {
                    return Err(QxError::BusinessViolation(
                        "计价币预算在途名义额只支持现货买单".into(),
                    ));
                }
                return total
                    .checked_add(remaining_budget)
                    .ok_or_else(|| QxError::Invariant("精确在途预算名义额合计溢出".into()));
            }
            let remaining = order.remaining_decimal().ok_or_else(|| {
                QxError::Invariant(format!("精确在途订单 {} 剩余数量溢出", order.client_id))
            })?;
            if !remaining.is_positive() {
                return Ok(total);
            }
            let limit_notional = order
                .limit_price_decimal()
                .map(|price| spec.notional_exact(remaining, price))
                .transpose()?;
            let effective_mark = order.market_reference_price_decimal().or(reference_mark);
            let mark_notional = effective_mark
                .map(|price| spec.notional_exact(remaining, price))
                .transpose()?;
            let notional = match (limit_notional, mark_notional) {
                (Some(limit), Some(mark)) => {
                    if limit
                        .checked_cmp(mark)
                        .is_some_and(|ordering| ordering.is_gt())
                    {
                        limit
                    } else {
                        mark
                    }
                }
                (Some(limit), None) => limit,
                (None, Some(mark)) => mark,
                (None, None) => {
                    return Err(QxError::BusinessViolation(format!(
                        "精确在途市价订单 {} 缺少名义额风控参考价",
                        order.client_id
                    )));
                }
            };
            total
                .checked_add(notional)
                .ok_or_else(|| QxError::Invariant("精确在途订单名义额合计溢出".into()))
        })
}

/// Convert a legacy raw reference mark into the exact decimal form used by the precise reserve.
pub fn pending_order_gross_notional_exact(
    orders: &[Order],
    candidate_order_id: u64,
    account_id: &str,
    instrument: &InstrumentId,
    spec: &TradingInstrumentSpec,
    marks: &BTreeMap<InstrumentId, Price>,
) -> QxResult<ExactDecimal> {
    let reference_mark = marks
        .get(instrument)
        .map(|price| ExactDecimal::from_parts(price.raw(), 9))
        .transpose()
        .map_err(|error| QxError::Invariant(error.into()))?;
    pending_order_gross_notional_exact_at_reference_mark(
        orders,
        candidate_order_id,
        account_id,
        instrument,
        spec,
        reference_mark,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{ExactOrderAmounts, OrderStatus, Quantity, Side, TradingProduct, SCALE};

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
            min_notional_raw: 0,
            max_leverage: 1,
            maintenance_margin_bps: 0,
            valid_from: 1,
            valid_to: None,
            exact_rules: None,
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
            exact_amounts: None,
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
    fn pending_exact_order_fails_closed_in_legacy_notional_reservation() {
        use qx_core::{ExactDecimal, ExactOrderAmounts};

        let instrument = InstrumentId::parse("PEPEUSDT.BINANCE").unwrap();
        let mut pending = order(1, &instrument, "main", OrderStatus::Accepted, 1, 0, Some(1));
        pending.qty = Quantity::ZERO;
        pending.limit = None;
        pending.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse("1000.0000000001").unwrap(),
            limit_price: Some(ExactDecimal::parse("0.00000001").unwrap()),
            filled: ExactDecimal::ZERO,
            market_reference_price: None,
            quote_budget: None,
            quote_filled: Default::default(),
        });
        assert!(pending_order_gross_notional(
            &[pending],
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

    #[test]
    fn exact_pending_reservation_preserves_sub_nano_notional_and_mixed_legacy_orders() {
        use qx_core::ExactOrderAmounts;

        let instrument = InstrumentId::parse("PEPEUSDT.BINANCE").unwrap();
        let legacy = order(1, &instrument, "main", OrderStatus::Working, 1, 0, Some(10));
        let mut exact = order(
            2,
            &instrument,
            "main",
            OrderStatus::PartiallyFilled,
            1,
            0,
            None,
        );
        exact.qty = Quantity::ZERO;
        exact.limit = None;
        exact.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse("0.0000000003").unwrap(),
            limit_price: Some(ExactDecimal::parse("0.00000001").unwrap()),
            filled: ExactDecimal::parse("0.0000000001").unwrap(),
            market_reference_price: None,
            quote_budget: None,
            quote_filled: Default::default(),
        });
        let mark = ExactDecimal::parse("0.00000002").unwrap();

        let actual = pending_order_gross_notional_exact_at_reference_mark(
            [&legacy, &exact],
            99,
            "main",
            &instrument,
            &spec(&instrument),
            Some(mark),
        )
        .unwrap();

        // The legacy order reserves 10 quote units. The exact remainder is 2e-10 base;
        // the mark is larger than its limit and contributes 4e-18 quote units.
        assert_eq!(
            actual,
            ExactDecimal::parse("10.000000000000000004").unwrap()
        );
    }

    #[test]
    fn exact_market_pending_order_requires_an_exact_reference_mark() {
        use qx_core::ExactOrderAmounts;

        let instrument = InstrumentId::parse("PEPEUSDT.BINANCE").unwrap();
        let mut market = order(1, &instrument, "main", OrderStatus::Accepted, 1, 0, None);
        market.qty = Quantity::ZERO;
        market.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            limit_price: None,
            filled: ExactDecimal::ZERO,
            market_reference_price: None,
            quote_budget: None,
            quote_filled: Default::default(),
        });
        let error = pending_order_gross_notional_exact_at_reference_mark(
            [&market],
            2,
            "main",
            &instrument,
            &spec(&instrument),
            None,
        )
        .expect_err("market pending order must not become a zero reserve");
        assert!(error.to_string().contains("缺少名义额风控参考价"));
    }

    #[test]
    fn exact_market_pending_order_uses_its_captured_sub_nano_reference_price() {
        use qx_core::ExactOrderAmounts;

        let instrument = InstrumentId::parse("PEPEUSDT.BINANCE").unwrap();
        let mut market = order(1, &instrument, "main", OrderStatus::Accepted, 1, 0, None);
        market.qty = Quantity::ZERO;
        market.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse("10").unwrap(),
            limit_price: None,
            filled: ExactDecimal::ZERO,
            market_reference_price: Some(ExactDecimal::parse("0.0000000009").unwrap()),
            quote_budget: None,
            quote_filled: Default::default(),
        });

        let actual = pending_order_gross_notional_exact(
            &[market],
            2,
            "main",
            &instrument,
            &spec(&instrument),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(actual, ExactDecimal::parse("0.000000009").unwrap());
    }

    #[test]
    fn quote_budget_pending_exposure_reserves_only_unspent_quote_currency() {
        let instrument = InstrumentId::parse("PEPE/USDT.BITGET").unwrap();
        let mut pending = order(
            1,
            &instrument,
            "main",
            OrderStatus::PartiallyFilled,
            0,
            0,
            None,
        );
        pending.qty = Quantity::ZERO;
        pending.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::ZERO,
            limit_price: None,
            filled: ExactDecimal::parse("1").unwrap(),
            quote_budget: Some(ExactDecimal::parse("10").unwrap()),
            quote_filled: ExactDecimal::parse("4").unwrap(),
            market_reference_price: Some(ExactDecimal::parse("0.000001").unwrap()),
        });
        assert_eq!(
            pending_order_gross_notional_exact_at_reference_mark(
                [&pending],
                2,
                "main",
                &instrument,
                &spec(&instrument),
                None,
            )
            .unwrap(),
            ExactDecimal::parse("6").unwrap()
        );
    }
}
