use super::*;

#[test]
fn fill_changes_cash_and_position() {
    let mut l = Ledger::new();
    l.deposit("main", "USDT", Money::from_i64(1000), 1).unwrap();
    let o = order(Side::Buy);
    let f = Fill {
        order_id: 7,
        qty: Quantity::from_i64(2),
        price: Price::from_i64(100),
        fee: Money::from_i64(1),
        ts: 2,
        ..Fill::default()
    };
    l.apply_fill(&o, &f, "USDT").unwrap();
    assert_eq!(l.cash("USDT"), Money::from_i64(799).raw());
    assert_eq!(
        l.position(&o.instrument).quantity.raw(),
        Quantity::from_i64(2).raw()
    );
}

#[test]
fn exact_spot_fills_keep_sub_nano_cash_fees_position_and_replay() {
    let instrument = InstrumentId::parse("TINY/USDT.BINANCE").unwrap();
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: TradingProduct::Spot,
        base_currency: "TINY".into(),
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
        exact_rules: Some(ExactTradingRules {
            contract_size: ExactDecimal::parse("1").unwrap(),
            price_tick: ExactDecimal::parse("0.00000001").unwrap(),
            quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        }),
    };
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "BTC", Money::from_i64(1), 1)
        .unwrap();
    let mut buy = order(Side::Buy);
    buy.instrument = instrument.clone();
    buy.qty = Quantity::ZERO;
    buy.limit = None;
    buy.filled = Quantity::ZERO;
    buy.exact_amounts = Some(ExactOrderAmounts {
        quantity: ExactDecimal::parse("0.0000000003").unwrap(),
        limit_price: Some(ExactDecimal::parse("0.00000004").unwrap()),
        filled: ExactDecimal::ZERO,
        market_reference_price: None,
        quote_budget: None,
        quote_filled: Default::default(),
    });
    let buy_fill = Fill {
        order_id: buy.client_id,
        ts: 2,
        account_id: "main".into(),
        exact_amounts: Some(ExactFillAmounts {
            quantity: ExactDecimal::parse("0.0000000003").unwrap(),
            price: ExactDecimal::parse("0.00000004").unwrap(),
            quote_cost: None,
            quote_budget_complete: false,
        }),
        exact_fees: vec![
            ExactFillFee {
                amount: ExactDecimal::parse("0.0000000001").unwrap(),
                currency: Some("TINY".into()),
                source_trade_ids: vec!["buy-fee-base".into()],
            },
            ExactFillFee {
                amount: ExactDecimal::parse("0.000000000000000002").unwrap(),
                currency: Some("USDT".into()),
                source_trade_ids: vec!["buy-fee-quote".into()],
            },
        ],
        ..Fill::default()
    };
    ledger
        .apply_fill_with_spec(&buy, &buy_fill, "USDT", &spec)
        .unwrap();

    let mut sell = order(Side::Sell);
    sell.client_id = 8;
    sell.instrument = instrument.clone();
    sell.qty = Quantity::ZERO;
    sell.limit = None;
    sell.filled = Quantity::ZERO;
    sell.exact_amounts = Some(ExactOrderAmounts {
        quantity: ExactDecimal::parse("0.0000000001").unwrap(),
        limit_price: Some(ExactDecimal::parse("0.00000005").unwrap()),
        filled: ExactDecimal::ZERO,
        market_reference_price: None,
        quote_budget: None,
        quote_filled: Default::default(),
    });
    let sell_fill = Fill {
        order_id: sell.client_id,
        ts: 3,
        account_id: "main".into(),
        exact_amounts: Some(ExactFillAmounts {
            quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            price: ExactDecimal::parse("0.00000005").unwrap(),
            quote_cost: None,
            quote_budget_complete: false,
        }),
        ..Fill::default()
    };
    ledger
        .apply_fill_with_spec(&sell, &sell_fill, "USDT", &spec)
        .unwrap();

    assert_eq!(ledger.cash_for("main", "USDT"), 0);
    assert_eq!(
        ledger.cash_for_exact("main", "USDT"),
        ExactDecimal::parse("-0.000000000000000009").unwrap()
    );
    assert_eq!(
        ledger.cash_for_exact("main", "TINY"),
        ExactDecimal::parse("-0.0000000001").unwrap()
    );
    assert_eq!(
        ledger.cash_balances_exact_for("main").get("BTC"),
        Some(&ExactDecimal::parse("1").unwrap()),
        "exact balance view must retain currencies that still use legacy entries"
    );
    let position = ledger.position_for_side_exact("main", &instrument, PositionSide::Net);
    assert_eq!(
        position.quantity,
        ExactDecimal::parse("0.0000000002").unwrap()
    );
    assert_eq!(
        position.average_entry,
        ExactDecimal::parse("0.00000004").unwrap()
    );
    assert_eq!(
        position.realized_pnl,
        ExactDecimal::parse("0.000000000000000001").unwrap()
    );

    let mut replayed = Ledger::new();
    for entry in ledger.entries() {
        replayed.apply_entry(entry.clone()).unwrap();
    }
    assert_eq!(
        replayed.cash_for_exact("main", "USDT"),
        ledger.cash_for_exact("main", "USDT")
    );
    assert_eq!(
        replayed.cash_for_exact("main", "TINY"),
        ledger.cash_for_exact("main", "TINY")
    );
    assert_eq!(
        replayed.position_for_side_exact("main", &instrument, PositionSide::Net),
        position
    );
}

#[test]
fn quote_budget_spot_fill_books_reported_exact_quote_cost() {
    let instrument = InstrumentId::parse("TINY/USDT.BITGET").unwrap();
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: TradingProduct::Spot,
        base_currency: "TINY".into(),
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
        exact_rules: Some(ExactTradingRules {
            contract_size: ExactDecimal::parse("1").unwrap(),
            price_tick: ExactDecimal::parse("0.00000001").unwrap(),
            quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        }),
    };
    let mut buy = order(Side::Buy);
    buy.instrument = instrument;
    buy.qty = Quantity::ZERO;
    buy.limit = None;
    buy.filled = Quantity::ZERO;
    buy.exact_amounts = Some(ExactOrderAmounts {
        quantity: ExactDecimal::ZERO,
        limit_price: None,
        filled: ExactDecimal::ZERO,
        quote_budget: Some(ExactDecimal::parse("0.00000000000000002").unwrap()),
        quote_filled: ExactDecimal::ZERO,
        market_reference_price: None,
    });
    let fill = Fill {
        order_id: buy.client_id,
        ts: 2,
        account_id: "main".into(),
        exact_amounts: Some(ExactFillAmounts {
            quantity: ExactDecimal::parse("0.0000000003").unwrap(),
            price: ExactDecimal::parse("0.00000004").unwrap(),
            quote_cost: Some(ExactDecimal::parse("0.000000000000000012").unwrap()),
            quote_budget_complete: false,
        }),
        ..Fill::default()
    };
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_i64(1), 1)
        .unwrap();
    ledger
        .apply_fill_with_spec(&buy, &fill, "USDT", &spec)
        .unwrap();
    assert_eq!(
        ledger.cash_for_exact("main", "USDT"),
        ExactDecimal::parse("0.999999999999999988").unwrap()
    );
    assert_eq!(
        ledger
            .position_for_side_exact("main", &buy.instrument, qx_core::PositionSide::Net)
            .quantity,
        ExactDecimal::parse("0.0000000003").unwrap()
    );
}

#[test]
fn exact_linear_derivative_fills_book_contract_pnl_fees_and_replay() {
    let instrument = InstrumentId::parse("TINY/USDT:USDT.OKX").unwrap();
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: TradingProduct::Perpetual,
        base_currency: "TINY".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        min_notional_raw: 0,
        max_leverage: 20,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
        exact_rules: Some(ExactTradingRules {
            contract_size: ExactDecimal::parse("0.001").unwrap(),
            price_tick: ExactDecimal::parse("0.01").unwrap(),
            quantity_step: ExactDecimal::parse("0.1").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.1").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        }),
    };
    let policy = |reduce_only, position_side| OrderPolicy {
        reduce_only,
        position_side,
        margin_mode: MarginMode::Cross,
        position_mode: PositionMode::Hedge,
        leverage: 5,
        post_only: false,
    };
    let exact_order = |client_id, side, quantity, limit_price, reduce_only, position_side| {
        let mut order = order(side);
        order.client_id = client_id;
        order.instrument = instrument.clone();
        order.qty = Quantity::ZERO;
        order.limit = None;
        order.filled = Quantity::ZERO;
        order.policy = Some(policy(reduce_only, position_side));
        order.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse(quantity).unwrap(),
            limit_price: Some(ExactDecimal::parse(limit_price).unwrap()),
            filled: ExactDecimal::ZERO,
            market_reference_price: None,
            quote_budget: None,
            quote_filled: Default::default(),
        });
        order
    };
    let open = exact_order(41, Side::Buy, "2", "10000", false, PositionSide::Long);
    let open_fill = Fill {
        order_id: open.client_id,
        ts: 2,
        account_id: "main".into(),
        exact_amounts: Some(ExactFillAmounts {
            quantity: ExactDecimal::parse("2").unwrap(),
            price: ExactDecimal::parse("10000").unwrap(),
            quote_cost: None,
            quote_budget_complete: false,
        }),
        exact_fees: vec![ExactFillFee {
            amount: ExactDecimal::parse("0.00000000001").unwrap(),
            currency: Some("USDT".into()),
            source_trade_ids: vec!["open-fee".into()],
        }],
        ..Fill::default()
    };
    let close = exact_order(42, Side::Sell, "0.5", "11000", true, PositionSide::Long);
    let close_fill = Fill {
        order_id: close.client_id,
        ts: 3,
        account_id: "main".into(),
        exact_amounts: Some(ExactFillAmounts {
            quantity: ExactDecimal::parse("0.5").unwrap(),
            price: ExactDecimal::parse("11000").unwrap(),
            quote_cost: None,
            quote_budget_complete: false,
        }),
        exact_fees: vec![ExactFillFee {
            amount: ExactDecimal::parse("0.00000000002").unwrap(),
            currency: Some("USDT".into()),
            source_trade_ids: vec!["close-fee".into()],
        }],
        ..Fill::default()
    };
    let short_open = exact_order(43, Side::Sell, "1", "12000", false, PositionSide::Short);
    let short_open_fill = Fill {
        order_id: short_open.client_id,
        ts: 4,
        account_id: "main".into(),
        exact_amounts: Some(ExactFillAmounts {
            quantity: ExactDecimal::parse("1").unwrap(),
            price: ExactDecimal::parse("12000").unwrap(),
            quote_cost: None,
            quote_budget_complete: false,
        }),
        ..Fill::default()
    };
    let short_close = exact_order(44, Side::Buy, "0.5", "11000", true, PositionSide::Short);
    let short_close_fill = Fill {
        order_id: short_close.client_id,
        ts: 5,
        account_id: "main".into(),
        exact_amounts: Some(ExactFillAmounts {
            quantity: ExactDecimal::parse("0.5").unwrap(),
            price: ExactDecimal::parse("11000").unwrap(),
            quote_cost: None,
            quote_budget_complete: false,
        }),
        ..Fill::default()
    };

    let mut ledger = Ledger::new();
    ledger
        .apply_fill_with_spec(&open, &open_fill, "USDT", &spec)
        .unwrap();
    ledger
        .apply_fill_with_spec(&close, &close_fill, "USDT", &spec)
        .unwrap();
    ledger
        .apply_fill_with_spec(&short_open, &short_open_fill, "USDT", &spec)
        .unwrap();
    ledger
        .apply_fill_with_spec(&short_close, &short_close_fill, "USDT", &spec)
        .unwrap();

    let position = ledger.position_for_side_exact("main", &instrument, PositionSide::Long);
    assert_eq!(position.quantity, ExactDecimal::parse("1.5").unwrap());
    assert_eq!(
        position.average_entry,
        ExactDecimal::parse("10000").unwrap()
    );
    assert_eq!(position.realized_pnl, ExactDecimal::parse("0.5").unwrap());
    assert_eq!(
        ledger.cash_for_exact("main", "USDT"),
        ExactDecimal::parse("0.99999999997").unwrap()
    );
    let short_position = ledger.position_for_side_exact("main", &instrument, PositionSide::Short);
    assert_eq!(
        short_position.quantity,
        ExactDecimal::parse("-0.5").unwrap()
    );
    assert_eq!(
        short_position.average_entry,
        ExactDecimal::parse("12000").unwrap()
    );
    assert_eq!(
        short_position.realized_pnl,
        ExactDecimal::parse("0.5").unwrap()
    );

    let mut replayed = Ledger::new();
    for entry in ledger.entries() {
        replayed.apply_entry(entry.clone()).unwrap();
    }
    assert_eq!(
        replayed.position_for_side_exact("main", &instrument, PositionSide::Long),
        position
    );
    assert_eq!(
        replayed.cash_for_exact("main", "USDT"),
        ledger.cash_for_exact("main", "USDT")
    );
    assert_eq!(
        replayed.position_for_side_exact("main", &instrument, PositionSide::Short),
        short_position
    );
}

#[test]
fn exact_inverse_derivative_uses_harmonic_average_cost_for_pnl_and_replay() {
    let instrument = InstrumentId::parse("BTC/USD:BTC.OKX").unwrap();
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USD".into(),
        settlement_currency: "BTC".into(),
        contract_size: SCALE,
        linear: false,
        inverse: true,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        min_notional_raw: 0,
        max_leverage: 20,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
        exact_rules: Some(ExactTradingRules {
            contract_size: ExactDecimal::parse("100").unwrap(),
            price_tick: ExactDecimal::parse("0.1").unwrap(),
            quantity_step: ExactDecimal::parse("1").unwrap(),
            minimum_quantity: ExactDecimal::parse("1").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        }),
    };
    let exact_order = |client_id, side, quantity, price, reduce_only| {
        let mut order = order(side);
        order.client_id = client_id;
        order.instrument = instrument.clone();
        order.qty = Quantity::ZERO;
        order.limit = None;
        order.filled = Quantity::ZERO;
        order.policy = Some(OrderPolicy {
            reduce_only,
            position_side: PositionSide::Net,
            margin_mode: MarginMode::Cross,
            position_mode: PositionMode::OneWay,
            leverage: 5,
            post_only: false,
        });
        order.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse(quantity).unwrap(),
            limit_price: Some(ExactDecimal::parse(price).unwrap()),
            filled: ExactDecimal::ZERO,
            market_reference_price: None,
            quote_budget: None,
            quote_filled: Default::default(),
        });
        let fill = Fill {
            order_id: client_id,
            ts: client_id,
            account_id: "main".into(),
            exact_amounts: Some(ExactFillAmounts {
                quantity: ExactDecimal::parse(quantity).unwrap(),
                price: ExactDecimal::parse(price).unwrap(),
                quote_cost: None,
                quote_budget_complete: false,
            }),
            ..Fill::default()
        };
        (order, fill)
    };
    let (first, first_fill) = exact_order(71, Side::Buy, "1", "100", false);
    let (add, add_fill) = exact_order(72, Side::Buy, "1", "300", false);
    let (close, close_fill) = exact_order(73, Side::Sell, "1", "200", true);

    let mut ledger = Ledger::new();
    ledger
        .apply_fill_with_spec(&first, &first_fill, "BTC", &spec)
        .unwrap();
    ledger
        .apply_fill_with_spec(&add, &add_fill, "BTC", &spec)
        .unwrap();
    let increased = ledger.position_for_side_exact("main", &instrument, PositionSide::Net);
    assert_eq!(increased.quantity, ExactDecimal::parse("2").unwrap());
    assert_eq!(increased.average_entry, ExactDecimal::parse("150").unwrap());

    ledger
        .apply_fill_with_spec(&close, &close_fill, "BTC", &spec)
        .unwrap();
    let position = ledger.position_for_side_exact("main", &instrument, PositionSide::Net);
    assert_eq!(position.quantity, ExactDecimal::parse("1").unwrap());
    assert_eq!(position.average_entry, ExactDecimal::parse("150").unwrap());
    assert_eq!(
        position.realized_pnl,
        ExactDecimal::parse("0.166666666666666666").unwrap()
    );
    assert_eq!(ledger.cash_for_exact("main", "BTC"), position.realized_pnl);

    let mut replayed = Ledger::new();
    for entry in ledger.entries() {
        replayed.apply_entry(entry.clone()).unwrap();
    }
    assert_eq!(
        replayed.position_for_side_exact("main", &instrument, PositionSide::Net),
        position
    );
    assert_eq!(
        replayed.cash_for_exact("main", "BTC"),
        ledger.cash_for_exact("main", "BTC")
    );
}

#[test]
fn derivative_fill_uses_contract_pnl_without_debiting_full_notional() {
    let instrument = InstrumentId::parse("BTC/USDT:USDT.BINANCE").unwrap();
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: crate::TradingProduct::Perpetual,
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
        max_leverage: 100,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
        exact_rules: None,
    };
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_i64(1000), 1)
        .unwrap();
    let mut buy = order(Side::Buy);
    buy.client_id = 1;
    buy.instrument = instrument.clone();
    buy.qty = Quantity::from_i64(1);
    let mut sell = order(Side::Sell);
    sell.client_id = 2;
    sell.instrument = instrument.clone();
    sell.qty = Quantity::from_i64(1);
    ledger
        .apply_fill_with_spec(
            &buy,
            &Fill {
                order_id: 1,
                qty: Quantity::from_i64(1),
                price: Price::from_i64(100),
                ts: 2,
                ..Fill::default()
            },
            "USDT",
            &spec,
        )
        .unwrap();
    assert_eq!(ledger.cash_for("main", "USDT"), Money::from_i64(1000).raw());
    let marks = BTreeMap::from([(instrument.clone(), Price::from_i64(110))]);
    assert_eq!(
        ledger
            .equity_for_with_spec("main", &marks, "USDT", &spec)
            .unwrap(),
        Money::from_i64(1010).raw()
    );
    ledger
        .apply_fill_with_spec(
            &sell,
            &Fill {
                order_id: 2,
                qty: Quantity::from_i64(1),
                price: Price::from_i64(110),
                ts: 3,
                ..Fill::default()
            },
            "USDT",
            &spec,
        )
        .unwrap();
    assert_eq!(ledger.cash_for("main", "USDT"), Money::from_i64(1010).raw());
    assert_eq!(ledger.position_for("main", &instrument).quantity.raw(), 0);
}

#[test]
fn hedge_mode_keeps_long_and_short_legs_separate_through_replay() {
    let instrument = InstrumentId::parse("BTC/USDT:USDT.BINANCE").unwrap();
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: crate::TradingProduct::Perpetual,
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
        max_leverage: 100,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
        exact_rules: None,
    };
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_i64(1000), 1)
        .unwrap();
    let hedge_order = |client_id, side, position_side| Order {
        client_id,
        instrument: instrument.clone(),
        side,
        qty: Quantity::from_i64(1),
        limit: None,
        status: OrderStatus::Accepted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: Some(crate::OrderPolicy {
            reduce_only: false,
            position_side,
            margin_mode: crate::MarginMode::Cross,
            position_mode: crate::PositionMode::Hedge,
            leverage: 10,
            post_only: false,
        }),

        exact_amounts: None,
    };
    let long = hedge_order(11, Side::Buy, crate::PositionSide::Long);
    let short = hedge_order(12, Side::Sell, crate::PositionSide::Short);
    ledger
        .apply_fill_with_spec(
            &long,
            &Fill {
                order_id: 11,
                qty: Quantity::from_i64(1),
                price: Price::from_i64(100),
                ts: 2,
                ..Fill::default()
            },
            "USDT",
            &spec,
        )
        .unwrap();
    ledger
        .apply_fill_with_spec(
            &short,
            &Fill {
                order_id: 12,
                qty: Quantity::from_i64(1),
                price: Price::from_i64(110),
                ts: 3,
                ..Fill::default()
            },
            "USDT",
            &spec,
        )
        .unwrap();
    assert_eq!(
        ledger
            .position_for_side("main", &instrument, crate::PositionSide::Long)
            .quantity
            .raw(),
        SCALE
    );
    assert_eq!(
        ledger
            .position_for_side("main", &instrument, crate::PositionSide::Short)
            .quantity
            .raw(),
        -SCALE
    );
    assert_eq!(ledger.position_for("main", &instrument).quantity.raw(), 0);
    let close_long = hedge_order(13, Side::Sell, crate::PositionSide::Long);
    ledger
        .apply_fill_with_spec(
            &close_long,
            &Fill {
                order_id: 13,
                qty: Quantity::from_i64(1),
                price: Price::from_i64(120),
                ts: 4,
                ..Fill::default()
            },
            "USDT",
            &spec,
        )
        .unwrap();
    assert_eq!(
        ledger
            .position_for_side("main", &instrument, crate::PositionSide::Long)
            .quantity
            .raw(),
        0
    );
    assert_eq!(ledger.cash_for("main", "USDT"), Money::from_i64(1020).raw());
    let mut replay = Ledger::new();
    for entry in ledger.entries().iter().cloned() {
        replay.apply_entry(entry).unwrap();
    }
    assert_eq!(
        replay.position_for("main", &instrument),
        ledger.position_for("main", &instrument)
    );
    assert_eq!(
        replay
            .position_for_side("main", &instrument, crate::PositionSide::Short)
            .quantity
            .raw(),
        -SCALE
    );
}

#[test]
fn replay_entries_are_append_only() {
    let mut l = Ledger::new();
    l.deposit("main", "USD", Money::from_i64(1), 1).unwrap();
    assert_eq!(l.entries()[0].id, 0);
    assert_eq!(l.entries().len(), 1);
}

#[test]
fn accounts_and_realized_pnl_are_isolated() {
    let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
    let mut l = Ledger::new();
    l.deposit("a", "USD", Money::from_i64(1_000), 1).unwrap();
    l.deposit("b", "USD", Money::from_i64(1_000), 1).unwrap();
    let mut buy = order(Side::Buy);
    buy.client_id = 1;
    buy.account_id = "a".into();
    buy.instrument = instrument.clone();
    let mut sell = order(Side::Sell);
    sell.client_id = 2;
    sell.account_id = "a".into();
    sell.instrument = instrument.clone();
    let fill_buy = Fill {
        order_id: 1,
        qty: Quantity::from_i64(2),
        price: Price::from_i64(100),
        ts: 2,
        ..Fill::default()
    };
    let fill_sell = Fill {
        order_id: 2,
        qty: Quantity::from_i64(1),
        price: Price::from_i64(110),
        ts: 3,
        ..Fill::default()
    };
    l.apply_fill(&buy, &fill_buy, "USD").unwrap();
    l.apply_fill(&sell, &fill_sell, "USD").unwrap();
    assert_eq!(l.position_for("a", &instrument).quantity.raw(), SCALE);
    assert_eq!(
        l.position_for("a", &instrument).average_entry.raw(),
        Price::from_i64(100).raw()
    );
    assert_eq!(
        l.position_for("a", &instrument).realized_pnl.raw(),
        Money::from_i64(10).raw()
    );
    assert_eq!(l.position_for("b", &instrument), PositionState::default());
    assert_eq!(l.cash_for("b", "USD"), Money::from_i64(1_000).raw());
}

#[test]
fn ledger_entries_replay_to_same_state() {
    let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
    let mut original = Ledger::new();
    original
        .deposit("main", "USD", Money::from_i64(1_000), 1)
        .unwrap();
    let mut o = order(Side::Buy);
    o.client_id = 3;
    o.instrument = instrument.clone();
    original
        .apply_fill(
            &o,
            &Fill {
                order_id: 3,
                qty: Quantity::from_i64(2),
                price: Price::from_i64(100),
                ts: 2,
                ..Fill::default()
            },
            "USD",
        )
        .unwrap();
    let mut replay = Ledger::new();
    for entry in original.entries().iter().cloned() {
        replay.apply_entry(entry).unwrap();
    }
    let mut marks = BTreeMap::new();
    marks.insert(instrument.clone(), Price::from_i64(120));
    assert_eq!(
        replay.cash_for("main", "USD"),
        original.cash_for("main", "USD")
    );
    assert_eq!(
        replay.position_for("main", &instrument),
        original.position_for("main", &instrument)
    );
    assert_eq!(
        replay.equity_for("main", &marks, "USD"),
        original.equity_for("main", &marks, "USD")
    );
}

#[test]
fn equity_uses_explicit_contract_multiplier() {
    let instrument = InstrumentId::parse("FUTURE.SIM").unwrap();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USD", Money::from_i64(1_000), 1)
        .unwrap();
    let mut buy = order(Side::Buy);
    buy.client_id = 8;
    buy.instrument = instrument.clone();
    ledger
        .apply_fill_with_multiplier(
            &buy,
            &Fill {
                order_id: 8,
                qty: Quantity::from_i64(2),
                price: Price::from_i64(100),
                ts: 2,
                ..Fill::default()
            },
            "USD",
            10,
        )
        .unwrap();
    let marks = BTreeMap::from([(instrument, Price::from_i64(110))]);
    assert_eq!(
        ledger.equity_for_with_multiplier("main", &marks, "USD", 10),
        Some(Money::from_i64(1_200).raw())
    );
    assert_eq!(
        ledger.equity_for_with_multiplier("main", &marks, "USD", 0),
        None
    );
}

#[test]
fn cross_currency_equity_requires_and_applies_explicit_fx_rates() {
    let instrument = InstrumentId::parse("BTC/USDT:USDT.SIM").unwrap();
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: crate::TradingProduct::Perpetual,
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
        max_leverage: 100,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
        exact_rules: None,
    };
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "BTC", Money::from_i64(1), 1)
        .unwrap();
    ledger
        .deposit("main", "USDT", Money::from_i64(100), 1)
        .unwrap();
    let marks = BTreeMap::new();
    assert!(ledger
        .equity_for_with_spec_and_fx("main", &marks, "USDT", &spec, &BTreeMap::new())
        .is_err());
    let fx = BTreeMap::from([(String::from("BTC"), Price::from_i64(60_000))]);
    assert_eq!(
        ledger
            .equity_for_with_spec_and_fx("main", &marks, "USDT", &spec, &fx)
            .unwrap(),
        Money::from_i64(60_100).raw()
    );
}

#[test]
fn multiplier_is_preserved_in_realized_pnl_replay() {
    let instrument = InstrumentId::parse("FUTURE.SIM").unwrap();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USD", Money::from_i64(10_000), 1)
        .unwrap();
    let mut buy = order(Side::Buy);
    buy.client_id = 10;
    buy.instrument = instrument.clone();
    let mut sell = order(Side::Sell);
    sell.client_id = 11;
    sell.instrument = instrument.clone();
    ledger
        .apply_fill_with_multiplier(
            &buy,
            &Fill {
                order_id: 10,
                qty: Quantity::from_i64(2),
                price: Price::from_i64(100),
                ts: 2,
                ..Fill::default()
            },
            "USD",
            10,
        )
        .unwrap();
    ledger
        .apply_fill_with_multiplier(
            &sell,
            &Fill {
                order_id: 11,
                qty: Quantity::from_i64(2),
                price: Price::from_i64(110),
                ts: 3,
                ..Fill::default()
            },
            "USD",
            10,
        )
        .unwrap();
    assert_eq!(
        ledger.position_for("main", &instrument).realized_pnl,
        Money::from_i64(200)
    );
    let mut replay = Ledger::new();
    for entry in ledger.entries().iter().cloned() {
        replay.apply_entry(entry).unwrap();
    }
    assert_eq!(
        replay.position_for("main", &instrument).realized_pnl,
        ledger.position_for("main", &instrument).realized_pnl
    );
}
