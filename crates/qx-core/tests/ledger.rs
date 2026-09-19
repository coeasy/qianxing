//! 账簿内核集成用例：只用公开 API 断言事实归约与重放一致性。

use qx_core::*;
use std::collections::BTreeMap;

fn order(side: Side) -> Order {
    Order {
        client_id: 7,
        instrument: InstrumentId::parse("BTC-USDT.BINANCE").unwrap(),
        side,
        qty: Quantity::from_i64(2),
        limit: None,
        status: OrderStatus::Accepted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
    }
}

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

#[test]
fn corporate_action_dividend_and_split_replay() {
    let instrument = InstrumentId::parse("000001.SZSE").unwrap();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "CNY", Money::from_i64(10_000), 1)
        .unwrap();
    let mut buy = order(Side::Buy);
    buy.client_id = 12;
    buy.instrument = instrument.clone();
    buy.qty = Quantity::from_i64(100);
    ledger
        .apply_fill(
            &buy,
            &Fill {
                order_id: 12,
                qty: Quantity::from_i64(100),
                price: Price::from_i64(10),
                ts: 2,
                ..Fill::default()
            },
            "CNY",
        )
        .unwrap();
    ledger
        .apply_corporate_action(
            "main",
            &instrument,
            "CNY",
            CorporateAction {
                cash_dividend_raw: Money::from_raw(SCALE / 10).raw(),
                split_num: 2,
                split_den: 1,
            },
            3,
        )
        .unwrap();
    assert_eq!(
        ledger.position_for("main", &instrument).quantity.raw(),
        200 * SCALE
    );
    assert_eq!(
        ledger.cash_for("main", "CNY"),
        Money::from_i64(9_000).raw() + 10 * SCALE
    );
    let mut replay = Ledger::new();
    for entry in ledger.entries().iter().cloned() {
        replay.apply_entry(entry).unwrap();
    }
    assert_eq!(
        replay.position_for("main", &instrument),
        ledger.position_for("main", &instrument)
    );
    assert_eq!(
        replay.cash_for("main", "CNY"),
        ledger.cash_for("main", "CNY")
    );
}

#[test]
fn cash_dividend_entitlement_pays_by_record_date_position() {
    let instrument = InstrumentId::parse("000001.SZSE").unwrap();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "CNY", Money::from_i64(10_000), 1)
        .unwrap();
    let mut buy = order(Side::Buy);
    buy.client_id = 13;
    buy.instrument = instrument.clone();
    buy.qty = Quantity::from_i64(100);
    ledger
        .apply_fill(
            &buy,
            &Fill {
                order_id: 13,
                qty: Quantity::from_i64(100),
                price: Price::from_i64(10),
                ts: 2,
                ..Fill::default()
            },
            "CNY",
        )
        .unwrap();
    ledger
        .grant_cash_dividend_entitlement(
            "main",
            &instrument,
            "CNY",
            Money::from_raw(SCALE / 10).raw(),
            3,
        )
        .unwrap();

    let mut sell = order(Side::Sell);
    sell.client_id = 14;
    sell.instrument = instrument.clone();
    sell.qty = Quantity::from_i64(100);
    ledger
        .apply_fill(
            &sell,
            &Fill {
                order_id: 14,
                qty: Quantity::from_i64(100),
                price: Price::from_i64(10),
                ts: 4,
                ..Fill::default()
            },
            "CNY",
        )
        .unwrap();
    assert_eq!(
        ledger.cash_dividend_entitlement_for("main", &instrument, "CNY"),
        Money::from_i64(10)
    );
    ledger
        .settle_cash_dividend_entitlement("main", &instrument, "CNY", 5)
        .unwrap();
    assert_eq!(
        ledger.cash_dividend_entitlement_for("main", &instrument, "CNY"),
        Money::ZERO
    );
    assert_eq!(
        ledger.cash_for("main", "CNY"),
        Money::from_i64(10_010).raw()
    );
    let mut replay = Ledger::new();
    for entry in ledger.entries().iter().cloned() {
        replay.apply_entry(entry).unwrap();
    }
    assert_eq!(
        replay.cash_dividend_entitlement_for("main", &instrument, "CNY"),
        Money::ZERO
    );
    assert_eq!(
        replay.cash_for("main", "CNY"),
        ledger.cash_for("main", "CNY")
    );
}

#[test]
fn convertible_bond_issue_interest_and_tender_replay() {
    let bond = InstrumentId::parse("123001.SZSE").unwrap();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "CNY", Money::from_i64(100_000), 1)
        .unwrap();

    ledger
        .apply_convertible_bond_issue_subscription(
            "main",
            &bond,
            "CNY",
            Quantity::from_i64(100).raw(),
            Price::from_i64(100).raw(),
            2,
        )
        .unwrap();
    assert_eq!(
        ledger.position_for("main", &bond).quantity,
        Quantity::from_i64(100)
    );
    assert_eq!(
        ledger.cash_for("main", "CNY"),
        Money::from_i64(90_000).raw()
    );

    ledger
        .grant_convertible_bond_interest_entitlement(
            "main",
            &bond,
            "CNY",
            Price::from_i64(5).raw(),
            3,
        )
        .unwrap();
    assert_eq!(
        ledger.convertible_bond_interest_entitlement_for("main", &bond, "CNY"),
        Money::from_i64(500)
    );

    ledger
        .apply_convertible_bond_tender(
            "main",
            &bond,
            "CNY",
            Quantity::from_i64(40).raw(),
            Price::from_i64(110).raw(),
            4,
        )
        .unwrap();
    assert_eq!(
        ledger.position_for("main", &bond).quantity,
        Quantity::from_i64(60)
    );
    assert_eq!(
        ledger.convertible_bond_interest_entitlement_for("main", &bond, "CNY"),
        Money::from_i64(500)
    );

    ledger
        .settle_convertible_bond_interest_entitlement("main", &bond, "CNY", 5)
        .unwrap();
    assert_eq!(
        ledger.convertible_bond_interest_entitlement_for("main", &bond, "CNY"),
        Money::ZERO
    );
    assert_eq!(
        ledger.cash_for("main", "CNY"),
        Money::from_i64(94_900).raw()
    );

    let mut replay = Ledger::new();
    for entry in ledger.entries().iter().cloned() {
        replay.apply_entry(entry).unwrap();
    }
    assert_eq!(replay.entries(), ledger.entries());
    assert_eq!(
        replay.position_for("main", &bond),
        ledger.position_for("main", &bond)
    );
    assert_eq!(
        replay.cash_for("main", "CNY"),
        ledger.cash_for("main", "CNY")
    );
    assert_eq!(
        replay.convertible_bond_interest_entitlement_for("main", &bond, "CNY"),
        Money::ZERO
    );
}

#[test]
fn rights_subscription_is_explicit_and_replayable() {
    let instrument = InstrumentId::parse("000001.SZSE").unwrap();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "CNY", Money::from_i64(10_000), 1)
        .unwrap();
    let mut buy = order(Side::Buy);
    buy.client_id = 20;
    buy.instrument = instrument.clone();
    buy.qty = Quantity::from_i64(100);
    ledger
        .apply_fill(
            &buy,
            &Fill {
                order_id: 20,
                qty: Quantity::from_i64(100),
                price: Price::from_i64(10),
                ts: 2,
                ..Fill::default()
            },
            "CNY",
        )
        .unwrap();
    ledger
        .apply_rights_issue_subscription(
            "main",
            &instrument,
            &instrument,
            "CNY",
            ShareSubscription {
                entitled_qty_raw: Quantity::from_i64(20).raw(),
                subscription_qty_raw: Quantity::from_i64(20).raw(),
                subscription_price_raw: Price::from_i64(5).raw(),
            },
            3,
        )
        .unwrap();
    assert_eq!(
        ledger.position_for("main", &instrument).quantity.raw(),
        Quantity::from_i64(120).raw()
    );
    assert_eq!(ledger.cash_for("main", "CNY"), Money::from_i64(8_900).raw());
    let mut replay = Ledger::new();
    for entry in ledger.entries().iter().cloned() {
        replay.apply_entry(entry).unwrap();
    }
    assert_eq!(
        replay.cash_for("main", "CNY"),
        ledger.cash_for("main", "CNY")
    );
    assert_eq!(
        replay.position_for("main", &instrument),
        ledger.position_for("main", &instrument)
    );
}

#[test]
fn rights_entitlement_lifecycle_supports_partial_subscription_and_expiry() {
    let source = InstrumentId::parse("000001.SZSE").unwrap();
    let rights = InstrumentId::parse("700001.SZSE").unwrap();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "CNY", Money::from_i64(10_000), 1)
        .unwrap();
    let mut buy = order(Side::Buy);
    buy.client_id = 22;
    buy.instrument = source.clone();
    buy.qty = Quantity::from_i64(100);
    ledger
        .apply_fill(
            &buy,
            &Fill {
                order_id: 22,
                qty: Quantity::from_i64(100),
                price: Price::from_i64(10),
                ts: 2,
                ..Fill::default()
            },
            "CNY",
        )
        .unwrap();

    ledger
        .apply_rights_issue_event(
            "main",
            RightsIssueEvent {
                source_instrument: source.clone(),
                rights_instrument: rights.clone(),
                currency: "CNY".into(),
                entitled_qty_raw: Quantity::from_i64(20).raw(),
                subscription_qty_raw: Quantity::from_i64(10).raw(),
                subscription_price_raw: Price::from_i64(5).raw(),
            },
            3,
        )
        .unwrap();
    assert_eq!(
        ledger.rights_entitlement_for("main", &rights).raw(),
        Quantity::from_i64(10).raw()
    );
    assert_eq!(
        ledger.position_for("main", &source).quantity.raw(),
        Quantity::from_i64(100).raw()
    );
    assert_eq!(
        ledger.position_for("main", &rights).quantity.raw(),
        Quantity::from_i64(10).raw()
    );
    assert_eq!(ledger.cash_for("main", "CNY"), Money::from_i64(8_950).raw());

    ledger
        .expire_rights_entitlement("main", &rights, Quantity::from_i64(10).raw(), "CNY", 4)
        .unwrap();
    assert_eq!(
        ledger.rights_entitlement_for("main", &rights),
        Quantity::ZERO
    );

    let mut replay = Ledger::new();
    for entry in ledger.entries().iter().cloned() {
        replay.apply_entry(entry).unwrap();
    }
    assert_eq!(
        replay.rights_entitlement_for("main", &rights),
        ledger.rights_entitlement_for("main", &rights)
    );
    assert_eq!(
        replay.position_for("main", &rights),
        ledger.position_for("main", &rights)
    );
    assert_eq!(
        replay.cash_for("main", "CNY"),
        ledger.cash_for("main", "CNY")
    );
}

#[test]
fn rights_entitlement_can_be_granted_then_subscribed_after_position_changes() {
    let source = InstrumentId::parse("000001.SZSE").unwrap();
    let rights = InstrumentId::parse("700001.SZSE").unwrap();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "CNY", Money::from_i64(10_000), 1)
        .unwrap();
    let mut buy = order(Side::Buy);
    buy.client_id = 23;
    buy.instrument = source.clone();
    buy.qty = Quantity::from_i64(100);
    ledger
        .apply_fill(
            &buy,
            &Fill {
                order_id: 23,
                qty: Quantity::from_i64(100),
                price: Price::from_i64(10),
                ts: 2,
                ..Fill::default()
            },
            "CNY",
        )
        .unwrap();
    ledger
        .grant_rights_entitlement(
            "main",
            &source,
            &rights,
            "CNY",
            Quantity::from_i64(20).raw(),
            3,
        )
        .unwrap();

    let mut sell = order(Side::Sell);
    sell.client_id = 24;
    sell.instrument = source.clone();
    sell.qty = Quantity::from_i64(100);
    ledger
        .apply_fill(
            &sell,
            &Fill {
                order_id: 24,
                qty: Quantity::from_i64(100),
                price: Price::from_i64(10),
                ts: 4,
                ..Fill::default()
            },
            "CNY",
        )
        .unwrap();
    ledger
        .apply_rights_issue_subscription_from_entitlement(
            "main",
            &rights,
            "CNY",
            Quantity::from_i64(10).raw(),
            Price::from_i64(5).raw(),
            5,
        )
        .unwrap();
    assert_eq!(
        ledger.rights_entitlement_for("main", &rights).raw(),
        10 * SCALE
    );
    assert_eq!(
        ledger.position_for("main", &rights).quantity.raw(),
        10 * SCALE
    );
    assert_eq!(ledger.position_for("main", &source).quantity.raw(), 0);
    let mut replay = Ledger::new();
    for entry in ledger.entries().iter().cloned() {
        replay.apply_entry(entry).unwrap();
    }
    assert_eq!(
        replay.rights_entitlement_for("main", &rights),
        ledger.rights_entitlement_for("main", &rights)
    );
    assert_eq!(
        replay.position_for("main", &rights),
        ledger.position_for("main", &rights)
    );
}

#[test]
fn rights_entitlement_rejects_insufficient_expiry_and_subscription_atomically() {
    let source = InstrumentId::parse("000001.SZSE").unwrap();
    let rights = InstrumentId::parse("700001.SZSE").unwrap();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "CNY", Money::from_i64(10), 1)
        .unwrap();
    let before_entries = ledger.entries().len();
    assert!(ledger
        .apply_rights_issue_event(
            "main",
            RightsIssueEvent {
                source_instrument: source.clone(),
                rights_instrument: rights.clone(),
                currency: "CNY".into(),
                entitled_qty_raw: Quantity::from_i64(1).raw(),
                subscription_qty_raw: Quantity::from_i64(1).raw(),
                subscription_price_raw: Price::from_i64(100).raw(),
            },
            2,
        )
        .is_err());
    assert_eq!(ledger.entries().len(), before_entries);
    assert_eq!(
        ledger.rights_entitlement_for("main", &rights),
        Quantity::ZERO
    );
    assert!(ledger
        .expire_rights_entitlement("main", &rights, Quantity::from_i64(1).raw(), "CNY", 3)
        .is_err());
    assert_eq!(ledger.entries().len(), before_entries);
}

#[test]
fn convertible_conversion_transfers_two_instruments() {
    let bond = InstrumentId::parse("123001.SZSE").unwrap();
    let stock = InstrumentId::parse("000001.SZSE").unwrap();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "CNY", Money::from_i64(1_000), 1)
        .unwrap();
    let mut buy = order(Side::Buy);
    buy.client_id = 21;
    buy.instrument = bond.clone();
    buy.qty = Quantity::from_i64(10);
    ledger
        .apply_fill(
            &buy,
            &Fill {
                order_id: 21,
                qty: Quantity::from_i64(10),
                price: Price::from_i64(100),
                ts: 2,
                ..Fill::default()
            },
            "CNY",
        )
        .unwrap();
    ledger
        .apply_convertible_bond_conversion(
            "main",
            "CNY",
            ConvertibleBondConversion {
                bond_instrument: bond.clone(),
                target_instrument: stock.clone(),
                bond_qty_raw: Quantity::from_i64(2).raw(),
                target_qty_raw: Quantity::from_i64(20).raw(),
                conversion_price_raw: Price::from_i64(10).raw(),
            },
            3,
        )
        .unwrap();
    assert_eq!(
        ledger.position_for("main", &bond).quantity.raw(),
        Quantity::from_i64(8).raw()
    );
    assert_eq!(
        ledger.position_for("main", &stock).quantity.raw(),
        Quantity::from_i64(20).raw()
    );
    assert_eq!(ledger.position_for("main", &bond).realized_pnl, Money::ZERO);
    let mut replay = Ledger::new();
    for entry in ledger.entries().iter().cloned() {
        replay.apply_entry(entry).unwrap();
    }
    assert_eq!(replay.entries(), ledger.entries());
    assert_eq!(
        replay.position_for("main", &stock),
        ledger.position_for("main", &stock)
    );
}

#[test]
fn subscription_rejects_insufficient_cash_without_partial_entries() {
    let instrument = InstrumentId::parse("000001.SZSE").unwrap();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "CNY", Money::from_i64(1), 1)
        .unwrap();
    let before = ledger.entries().len();
    let result = ledger.apply_new_share_subscription(
        "main",
        &instrument,
        "CNY",
        Quantity::from_i64(1).raw(),
        Price::from_i64(5).raw(),
        2,
    );
    assert!(matches!(result, Err(QxError::BusinessViolation(_))));
    assert_eq!(ledger.entries().len(), before);
    assert_eq!(ledger.cash_for("main", "CNY"), Money::from_i64(1).raw());
}
