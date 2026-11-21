use super::*;

#[test]
fn reused_client_order_id_must_match_the_original_submission_identity() {
    let mut existing = port_order(88);
    existing.status = OrderStatus::Accepted;

    let mut changed_legacy_order = existing.clone();
    changed_legacy_order.qty = Quantity::from_i64(2);
    let mut state = PortState::default();
    state.orders.push(existing);
    let mut never_called = NeverCalledVenue;
    let mut source_seq = 0;
    let error = PortExecutionService::new(
        &mut never_called,
        &mut state,
        "identity-contract",
        100,
        &mut source_seq,
    )
    .submit(changed_legacy_order)
    .expect_err("a changed legacy quantity cannot reuse an accepted client ID");
    assert!(error.contains("拒绝复用"), "{error}");
    assert!(state.events.is_empty());

    let exact_amounts = qx_core::ExactOrderAmounts {
        quantity: qx_core::ExactDecimal::parse("0.0000000001").unwrap(),
        limit_price: Some(qx_core::ExactDecimal::parse("12.3456789012").unwrap()),
        filled: qx_core::ExactDecimal::ZERO,
        quote_budget: None,
        quote_filled: qx_core::ExactDecimal::ZERO,
        market_reference_price: None,
        conditional: None,
    };
    let mut existing_exact = port_order(89);
    existing_exact.qty = Quantity::ZERO;
    existing_exact.limit = None;
    existing_exact.exact_amounts = Some(exact_amounts);
    existing_exact.status = OrderStatus::PartiallyFilled;
    existing_exact.exact_amounts.as_mut().unwrap().filled =
        qx_core::ExactDecimal::parse("0.00000000005").unwrap();

    let mut changed_exact = existing_exact.clone();
    changed_exact.exact_amounts.as_mut().unwrap().quantity =
        qx_core::ExactDecimal::parse("0.0000000002").unwrap();
    changed_exact.exact_amounts.as_mut().unwrap().filled = qx_core::ExactDecimal::ZERO;
    changed_exact.status = OrderStatus::PendingSubmit;

    let mut state = PortState::default();
    state.orders.push(existing_exact);
    let mut never_called = NeverCalledVenue;
    let mut source_seq = 0;
    let error = PortExecutionService::new(
        &mut never_called,
        &mut state,
        "identity-contract",
        101,
        &mut source_seq,
    )
    .submit(changed_exact)
    .expect_err("a changed exact quantity cannot reuse a partially filled client ID");
    assert!(error.contains("拒绝复用"), "{error}");
    assert!(state.events.is_empty());

    let mut same_exact_request = state.orders[0].clone();
    same_exact_request.status = OrderStatus::PendingSubmit;
    same_exact_request.exact_amounts.as_mut().unwrap().filled = qx_core::ExactDecimal::ZERO;
    assert!(state.orders[0].same_submission_identity(&same_exact_request));
}

/// 同一订单在同一个 `ts` 上可以有两笔真实成交（分档吃单、费用返还），
/// 只靠 `order_id + ts + qty + price` 会把第二笔判为重播而静默丢弃。
/// 身份段必须覆盖 `fee` 与 `venue_order_id`，与 `qx-runtime` 的 `fill_key` 对齐。
#[test]
fn fill_tag_distinguishes_facts_that_only_differ_in_fee_or_venue_order() {
    let base = qx_core::Fill {
        order_id: 7,
        qty: qx_core::Quantity::from_i64(1),
        price: qx_core::Price::from_i64(100),
        fee: qx_core::Money::ZERO,
        ts: 200,
        venue_order_id: Some("venue-1".into()),
        ..qx_core::Fill::default()
    };
    let fee_only = qx_core::Fill {
        fee: qx_core::Money::from_i64(3),
        ..base.clone()
    };
    let venue_order_only = qx_core::Fill {
        venue_order_id: Some("venue-2".into()),
        ..base.clone()
    };

    assert_eq!(
        fill_tag(&base),
        fill_tag(&qx_core::Fill {
            account_id: "other".into(),
            ..base.clone()
        }),
        "同一笔事实的展示字段不得改变身份"
    );
    assert_ne!(fill_tag(&base), fill_tag(&fee_only));
    assert_ne!(fill_tag(&base), fill_tag(&venue_order_only));
    assert_ne!(fill_tag(&fee_only), fill_tag(&venue_order_only));
}

/// 提交**同步返回**的成交回报必须过与用户流回报同一个精度闸门：
/// 同一条落在 tick 之外的成交，不能因为"是 Venue 在 submit 里直接给的"就直接入账。
/// 覆盖三种决定性形状：裸 `Fill`（只认服务冻结的规格）、自带规格的 `FillWithSpec`
/// （以回报自带为准，因为落库保留的就是它），以及两者不一致时的优先级。
/// 越界时只能留下一条待对账事实（Accepted 与成交都不落），与"未知结果"契约同构。
#[test]
fn submit_returned_fills_share_the_precision_gate_with_user_stream_reports() {
    const STEP: i128 = qx_core::SCALE / 10;
    /// 100 USDT：在 0.1 与 1.0 两种 tick 上都成立。
    const ON_TICK: i128 = 100 * qx_core::SCALE;
    /// 100.05：不在 0.1 的 tick 上（两种规格都拒绝）。
    const OFF_TICK: i128 = ON_TICK + qx_core::SCALE / 20;
    /// 100.5：在 0.1 上、不在 1.0 上。配 "冻结宽规格 + 回报自带细规格" 用来钉死
    /// 优先级：以回报自带为准（否则落库的 `FillWithSpec` 会带着没校验过的规格）。
    const HALF_POINT: i128 = ON_TICK + qx_core::SCALE / 2;

    fn spec_with_tick(price_tick: i128) -> qx_core::TradingInstrumentSpec {
        qx_core::TradingInstrumentSpec {
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            product: qx_core::TradingProduct::Spot,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: qx_core::SCALE,
            linear: true,
            inverse: false,
            price_tick,
            qty_step: STEP,
            min_qty: STEP,
            min_notional_raw: 0,
            max_leverage: 1,
            maintenance_margin_bps: 0,
            valid_from: 0,
            valid_to: None,
            exact_rules: None,
        }
    }

    let fine = spec_with_tick(STEP);
    let coarse = spec_with_tick(qx_core::SCALE);
    // (用例, 服务冻结规格, 回报自带规格, 成交价, 是否必须转待对账)
    let scenarios = [
        ("bare-fill-on-tick", &fine, None, ON_TICK, false),
        ("bare-fill-off-tick", &fine, None, OFF_TICK, true),
        ("embedded-on-tick", &fine, Some(&fine), ON_TICK, false),
        ("embedded-off-tick", &fine, Some(&fine), OFF_TICK, true),
        (
            "embedded-spec-wins-over-frozen-spec",
            &coarse,
            Some(&fine),
            HALF_POINT,
            false,
        ),
    ];

    for (label, frozen, embedded, price_raw, must_reconcile) in scenarios {
        let fill = qx_core::Fill {
            order_id: 7,
            qty: Quantity::from_raw(STEP),
            price: Price::from_raw(price_raw),
            fee: qx_core::Money::ZERO,
            ts: 100,
            venue_order_id: Some("venue-7".into()),
            ..qx_core::Fill::default()
        };
        let facts = vec![
            ExecutionEvent::Accepted {
                client_order_id: 7,
                venue_order_id: "venue-7".into(),
            },
            match embedded {
                Some(spec) => ExecutionEvent::FillWithSpec {
                    fill: Box::new(fill),
                    spec: Box::new(spec.clone()),
                },
                None => ExecutionEvent::Fill(Box::new(fill)),
            },
        ];
        let mut venue = PortVenue { result: Ok(facts) };
        let mut state = PortState::default();
        let mut source_seq = 0;
        let order = Order {
            qty: Quantity::from_raw(10 * STEP),
            limit: None,
            ..port_order(7)
        };
        let outcome = PortExecutionService::new(
            &mut venue,
            &mut state,
            "precision-worker",
            100,
            &mut source_seq,
        )
        .with_instrument_spec(frozen.clone())
        .submit(order.clone());
        let seen = state
            .events
            .iter()
            .map(|event| match &event.event {
                ExecutionEvent::Accepted { .. } => "accepted",
                ExecutionEvent::Rejected { .. } => "rejected",
                ExecutionEvent::Fill(_) => "fill",
                ExecutionEvent::FillWithSpec { .. } => "fill-with-spec",
                ExecutionEvent::CancelPending { .. } => "cancel-pending",
                ExecutionEvent::OrderAmendmentPending { .. } => "amend-pending",
                ExecutionEvent::OrderAmended { .. } => "amended",
                ExecutionEvent::Cancelled { .. } => "cancelled",
                ExecutionEvent::ReconcileRequired { .. } => "reconcile",
                ExecutionEvent::MarketQuote { .. } => "quote",
                ExecutionEvent::FeeAdjustment { .. } => "fee-adjustment",
            })
            .collect::<Vec<_>>();
        if must_reconcile {
            let error = outcome.expect_err(&format!("{label}: {seen:?}"));
            assert!(error.contains("产品规格"), "{label} -> {error}");
            assert!(error.contains("对账"), "{label} -> {error}");
            assert_eq!(
                seen,
                vec!["reconcile"],
                "{label}: 越界成交只能留下一条待对账事实"
            );
        } else {
            let result = outcome.unwrap_or_else(|error| panic!("{label} 不应被拒: {error}"));
            assert_eq!(result.event_count, 2, "{label}: {seen:?}");
            assert_eq!(
                seen,
                vec!["accepted", "fill-with-spec"],
                "{label}: 在 tick 上的成交必须照常入账"
            );
        }
    }
}

#[test]
fn paper_quote_validation_accepts_sub_nano_exact_l1_without_raw_values() {
    use qx_core::ExactDecimal;

    let quote = QuoteTick::new(
        100,
        Price::from_raw(9),
        Quantity::ZERO,
        Price::from_raw(10),
        Quantity::ZERO,
        1,
    )
    .with_exact_prices(
        ExactDecimal::parse("0.0000000099").unwrap(),
        ExactDecimal::parse("0.0000000100").unwrap(),
    )
    .with_exact_sizes(
        ExactDecimal::parse("0.0000000005").unwrap(),
        ExactDecimal::parse("0.0000000002").unwrap(),
    );

    assert!(validate_paper_quote(&quote).is_ok());
}

#[test]
fn paper_quote_validation_rejects_conflicting_exact_raw_projection() {
    use qx_core::ExactDecimal;

    let quote = QuoteTick::new(
        100,
        Price::from_raw(8),
        Quantity::ZERO,
        Price::from_raw(10),
        Quantity::ZERO,
        1,
    )
    .with_exact_prices(
        ExactDecimal::parse("0.0000000099").unwrap(),
        ExactDecimal::parse("0.0000000100").unwrap(),
    )
    .with_exact_sizes(
        ExactDecimal::parse("0.0000000005").unwrap(),
        ExactDecimal::parse("0.0000000002").unwrap(),
    );

    assert!(validate_paper_quote(&quote).is_err());
}

#[test]
fn exact_fill_identity_and_spec_gate_keep_sub_nano_reports_distinct() {
    use qx_core::{
        ExactDecimal, ExactFillAmounts, ExactFillFee, ExactOrderAmounts, ExactTradingRules,
    };

    let instrument = InstrumentId::parse("PEPE/USDT.BINANCE").unwrap();
    let mut order = port_order(7);
    order.instrument = instrument.clone();
    order.qty = Quantity::ZERO;
    order.limit = None;
    order.exact_amounts = Some(ExactOrderAmounts {
        quantity: ExactDecimal::parse("0.0000000002").unwrap(),
        limit_price: Some(ExactDecimal::parse("0.00000001").unwrap()),
        filled: ExactDecimal::ZERO,
        market_reference_price: None,
        quote_budget: None,
        quote_filled: Default::default(),
        conditional: None,
    });

    let mut spec = qx_core::TradingInstrumentSpec::spot_default(&instrument, "USDT");
    spec.base_currency = "PEPE".into();
    spec.exact_rules = Some(ExactTradingRules {
        contract_size: ExactDecimal::parse("1").unwrap(),
        price_tick: ExactDecimal::parse("0.00000001").unwrap(),
        quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
        minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
        minimum_notional: ExactDecimal::ZERO,
    });
    let fill = qx_core::Fill {
        order_id: order.client_id,
        ts: 200,
        exact_amounts: Some(ExactFillAmounts {
            quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            price: ExactDecimal::parse("0.00000001").unwrap(),
            quote_cost: None,
            quote_budget_complete: false,
        }),
        exact_fees: vec![ExactFillFee {
            amount: ExactDecimal::ZERO,
            currency: None,
            source_trade_ids: vec!["trade-a".into()],
        }],
        ..qx_core::Fill::default()
    };
    let second_fill = qx_core::Fill {
        exact_fees: vec![ExactFillFee {
            source_trade_ids: vec!["trade-b".into()],
            ..fill.exact_fees[0].clone()
        }],
        ..fill.clone()
    };

    assert_ne!(fill_tag(&fill), fill_tag(&second_fill));
    let facts = [
        ExecutionEvent::Accepted {
            client_order_id: order.client_id,
            venue_order_id: "remote-7".into(),
        },
        ExecutionEvent::FillWithSpec {
            fill: Box::new(fill.clone()),
            spec: Box::new(spec.clone()),
        },
    ];
    assert!(gate_submit_facts(&order, &facts, Some(&spec)).is_ok());

    let mut off_step = fill;
    off_step.exact_amounts.as_mut().unwrap().quantity =
        ExactDecimal::parse("0.00000000015").unwrap();
    let invalid = [ExecutionEvent::FillWithSpec {
        fill: Box::new(off_step),
        spec: Box::new(spec.clone()),
    }];
    assert!(matches!(
        gate_submit_facts(&order, &invalid, Some(&spec)),
        Err(SubmitFactViolation::Precision(_))
    ));
}
