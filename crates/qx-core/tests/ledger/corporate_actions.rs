use super::*;

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
