use super::*;

#[test]
fn rights_subscription_requires_a_granted_entitlement_and_is_replayable() {
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
    // 没有登记日的权利授予，认购必须被拒：入账入口只有"用已授予的权利认购"一条。
    assert!(matches!(
        ledger.apply_rights_issue_subscription_from_entitlement(
            "main",
            &instrument,
            "CNY",
            Quantity::from_i64(20).raw(),
            Price::from_i64(5).raw(),
            3,
        ),
        Err(QxError::BusinessViolation(_))
    ));
    ledger
        .grant_rights_entitlement(
            "main",
            &instrument,
            &instrument,
            "CNY",
            Quantity::from_i64(20).raw(),
            3,
        )
        .unwrap();
    ledger
        .apply_rights_issue_subscription_from_entitlement(
            "main",
            &instrument,
            "CNY",
            Quantity::from_i64(20).raw(),
            Price::from_i64(5).raw(),
            4,
        )
        .unwrap();
    assert_eq!(
        ledger.position_for("main", &instrument).quantity.raw(),
        Quantity::from_i64(120).raw()
    );
    assert_eq!(ledger.cash_for("main", "CNY"), Money::from_i64(8_900).raw());
    assert_eq!(
        ledger.rights_entitlement_for("main", &instrument),
        Quantity::ZERO
    );
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
