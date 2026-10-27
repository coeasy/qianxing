use super::*;

#[test]
fn timed_rights_issue_lifecycle_uses_record_date_and_expires_remaining_rights() {
    let instrument = InstrumentId::parse("000001.SZSE").unwrap();
    let rights = InstrumentId::parse("700001.SZSE").unwrap();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "CNY", Money::from_i64(10_000), 1)
        .unwrap();
    let source_order = Order {
        client_id: 1,
        instrument: instrument.clone(),
        side: Side::Buy,
        qty: Quantity::from_i64(100),
        limit: Some(Price::from_i64(10)),
        status: OrderStatus::Filled,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
        exact_amounts: None,
    };
    ledger
        .apply_fill(
            &source_order,
            &Fill {
                order_id: 1,
                qty: Quantity::from_i64(100),
                price: Price::from_i64(10),
                ts: 2,
                ..Fill::default()
            },
            "CNY",
        )
        .unwrap();
    let record_ts = 10;
    let ex_ts = 20;
    let subscribe_ts = 30;
    let expiry_ts = 40;
    let rules = AshareRuleConfig {
        enabled: true,
        corporate_actions: vec![AshareCorporateActionEvent {
            ts: ex_ts,
            announcement_ts: None,
            record_ts: Some(record_ts),
            payment_ts: None,
            subscription_start_ts: Some(subscribe_ts),
            subscription_end_ts: Some(expiry_ts),
            action_type: AshareCorporateActionType::RightsIssue,
            split_num: 1,
            split_den: 1,
            rights_issue_price_raw: Price::from_i64(5).raw(),
            rights_issue_ratio_num: 20 * SCALE,
            rights_issue_ratio_den: 100 * SCALE,
            conversion_ratio_den: 1,
            rights_instrument: Some(rights.to_string()),
            subscription_qty_raw: Quantity::from_i64(10).raw(),
            ..AshareCorporateActionEvent::default()
        }],
        ..AshareRuleConfig::default()
    };
    rules.validate().unwrap();
    let action_schedule = AshareCorporateActionSchedule::new(&rules.corporate_actions);
    let mut config = VirtualTradingConfig {
        ashare_rules: Some(rules),
        ..VirtualTradingConfig::default()
    };
    let mut log = EventLog::new();
    let mut fills = Vec::new();
    for ts in [record_ts, ex_ts, subscribe_ts, expiry_ts] {
        let bar = Bar::new(ts, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, 1_000);
        let mut state = VirtualExecution {
            instrument: &instrument,
            account_id: "main",
            currency: "CNY",
            multiplier: 1,
            spec: None,
            ledger: &mut ledger,
            log: &mut log,
            fills: &mut fills,
            fee_engine: None,
        };
        apply_virtual_events(&config, Some(&action_schedule), &bar, &mut state).unwrap();
        if ts == record_ts {
            assert_eq!(
                ledger.rights_entitlement_for("main", &rights).raw(),
                20 * SCALE
            );
        }
        if ts == subscribe_ts {
            assert_eq!(
                ledger.position_for("main", &rights).quantity.raw(),
                10 * SCALE
            );
            assert_eq!(
                ledger.rights_entitlement_for("main", &rights).raw(),
                10 * SCALE
            );
        }
    }
    assert_eq!(
        ledger.rights_entitlement_for("main", &rights),
        Quantity::ZERO
    );
    assert_eq!(
        ledger.position_for("main", &rights).quantity.raw(),
        10 * SCALE
    );
    config.ashare_rules.as_mut().unwrap().validate().unwrap();
}

#[test]
fn timed_cash_dividend_pays_after_post_record_date_sale() {
    let instrument = InstrumentId::parse("000001.SZSE").unwrap();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "CNY", Money::from_i64(10_000), 1)
        .unwrap();
    let mut buy = Order {
        client_id: 31,
        instrument: instrument.clone(),
        side: Side::Buy,
        qty: Quantity::from_i64(100),
        limit: Some(Price::from_i64(10)),
        status: OrderStatus::Filled,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
        exact_amounts: None,
    };
    ledger
        .apply_fill(
            &buy,
            &Fill {
                order_id: buy.client_id,
                qty: buy.qty,
                price: Price::from_i64(10),
                ts: 2,
                ..Fill::default()
            },
            "CNY",
        )
        .unwrap();
    let rules = AshareRuleConfig {
        enabled: true,
        corporate_actions: vec![AshareCorporateActionEvent {
            ts: 20,
            record_ts: Some(10),
            payment_ts: Some(30),
            action_type: AshareCorporateActionType::CashDividend,
            cash_dividend_raw: Money::from_raw(SCALE / 10).raw(),
            ..AshareCorporateActionEvent::default()
        }],
        ..AshareRuleConfig::default()
    };
    rules.validate().unwrap();
    let action_schedule = AshareCorporateActionSchedule::new(&rules.corporate_actions);
    let config = VirtualTradingConfig {
        ashare_rules: Some(rules),
        ..VirtualTradingConfig::default()
    };
    let mut log = EventLog::new();
    let mut fills = Vec::new();
    for ts in [10, 20] {
        let bar = Bar::new(ts, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, 1_000);
        let mut state = VirtualExecution {
            instrument: &instrument,
            account_id: "main",
            currency: "CNY",
            multiplier: 1,
            spec: None,
            ledger: &mut ledger,
            log: &mut log,
            fills: &mut fills,
            fee_engine: None,
        };
        apply_virtual_events(&config, Some(&action_schedule), &bar, &mut state).unwrap();
    }
    assert_eq!(ledger.cash_for("main", "CNY"), Money::from_i64(9_000).raw());
    assert_eq!(
        ledger.cash_dividend_entitlement_for("main", &instrument, "CNY"),
        Money::from_i64(10)
    );
    buy.client_id = 32;
    buy.side = Side::Sell;
    buy.status = OrderStatus::Filled;
    ledger
        .apply_fill(
            &buy,
            &Fill {
                order_id: 32,
                qty: Quantity::from_i64(100),
                price: Price::from_i64(10),
                ts: 25,
                ..Fill::default()
            },
            "CNY",
        )
        .unwrap();
    let payment_bar = Bar::new(30, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, 1_000);
    let mut state = VirtualExecution {
        instrument: &instrument,
        account_id: "main",
        currency: "CNY",
        multiplier: 1,
        spec: None,
        ledger: &mut ledger,
        log: &mut log,
        fills: &mut fills,
        fee_engine: None,
    };
    apply_virtual_events(&config, Some(&action_schedule), &payment_bar, &mut state).unwrap();
    assert_eq!(
        ledger.cash_for("main", "CNY"),
        Money::from_i64(10_010).raw()
    );
    assert_eq!(
        ledger.cash_dividend_entitlement_for("main", &instrument, "CNY"),
        Money::ZERO
    );
}

#[test]
fn timed_convertible_bond_issue_interest_and_redemption_flow() {
    let stock = InstrumentId::parse("000001.SZSE").unwrap();
    let bond = InstrumentId::parse("123001.SZSE").unwrap();
    let rules = AshareRuleConfig {
        enabled: true,
        corporate_actions: vec![
            AshareCorporateActionEvent {
                ts: 10,
                subscription_start_ts: Some(10),
                action_type: AshareCorporateActionType::ConvertibleBondIssue,
                convertible_bond_instrument: Some(bond.to_string()),
                issue_price_raw: Price::from_i64(100).raw(),
                subscription_qty_raw: Quantity::from_i64(100).raw(),
                ..AshareCorporateActionEvent::default()
            },
            AshareCorporateActionEvent {
                ts: 20,
                record_ts: Some(20),
                payment_ts: Some(30),
                action_type: AshareCorporateActionType::ConvertibleBondInterest,
                convertible_bond_instrument: Some(bond.to_string()),
                interest_per_bond_raw: Price::from_i64(5).raw(),
                ..AshareCorporateActionEvent::default()
            },
            AshareCorporateActionEvent {
                ts: 40,
                payment_ts: Some(40),
                action_type: AshareCorporateActionType::ConvertibleBondCall,
                convertible_bond_instrument: Some(bond.to_string()),
                settlement_price_raw: Price::from_i64(105).raw(),
                ..AshareCorporateActionEvent::default()
            },
        ],
        ..AshareRuleConfig::default()
    };
    rules.validate().unwrap();
    let action_schedule = AshareCorporateActionSchedule::new(&rules.corporate_actions);
    let config = VirtualTradingConfig {
        ashare_rules: Some(rules),
        ..VirtualTradingConfig::default()
    };
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "CNY", Money::from_i64(100_000), 1)
        .unwrap();
    let mut log = EventLog::new();
    let mut fills = Vec::new();
    for ts in [10, 20, 30, 40] {
        let bar = Bar::new(ts, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, 1_000);
        let mut state = VirtualExecution {
            instrument: &stock,
            account_id: "main",
            currency: "CNY",
            multiplier: 1,
            spec: None,
            ledger: &mut ledger,
            log: &mut log,
            fills: &mut fills,
            fee_engine: None,
        };
        apply_virtual_events(&config, Some(&action_schedule), &bar, &mut state).unwrap();
        if ts == 10 {
            assert_eq!(
                ledger.position_for("main", &bond).quantity,
                Quantity::from_i64(100)
            );
            assert_eq!(
                ledger.cash_for("main", "CNY"),
                Money::from_i64(90_000).raw()
            );
        }
        if ts == 20 {
            assert_eq!(
                ledger.convertible_bond_interest_entitlement_for("main", &bond, "CNY"),
                Money::from_i64(500)
            );
        }
        if ts == 30 {
            assert_eq!(
                ledger.convertible_bond_interest_entitlement_for("main", &bond, "CNY"),
                Money::ZERO
            );
            assert_eq!(
                ledger.cash_for("main", "CNY"),
                Money::from_i64(90_500).raw()
            );
        }
    }
    assert_eq!(ledger.position_for("main", &bond).quantity, Quantity::ZERO);
    assert_eq!(
        ledger.cash_for("main", "CNY"),
        Money::from_i64(101_000).raw()
    );
}
