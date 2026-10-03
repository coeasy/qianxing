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
        max_leverage: 100,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
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
        max_leverage: 100,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
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
        max_leverage: 100,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
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
