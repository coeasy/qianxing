use super::*;

fn exact_spot_spec(instrument: &InstrumentId) -> qx_core::TradingInstrumentSpec {
    qx_core::TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: qx_core::TradingProduct::Spot,
        base_currency: "T".into(),
        quote_currency: "USD".into(),
        settlement_currency: "USD".into(),
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
        exact_rules: Some(qx_core::ExactTradingRules {
            contract_size: qx_core::ExactDecimal::ONE,
            price_tick: qx_core::ExactDecimal::parse("0.000000001").unwrap(),
            quantity_step: qx_core::ExactDecimal::parse("0.000000000001").unwrap(),
            minimum_quantity: qx_core::ExactDecimal::parse("0.000000000001").unwrap(),
            minimum_notional: qx_core::ExactDecimal::ZERO,
        }),
    }
}

fn native_subnano_portfolio(strategy_id: &str, budget_raw: i128) -> PortfolioBarStrategy {
    let budget_exact = qx_core::ExactDecimal::from_parts(budget_raw, 9).unwrap();
    native_subnano_portfolio_with_exact_budget(
        strategy_id,
        budget_raw,
        budget_exact,
        qx_core::ExactDecimal::parse("0.000000000001").unwrap(),
    )
}

fn native_subnano_portfolio_with_exact_budget(
    strategy_id: &str,
    budget_raw: i128,
    budget_exact: qx_core::ExactDecimal,
    quantity_exact: qx_core::ExactDecimal,
) -> PortfolioBarStrategy {
    let context = qx_strategy::StrategyContext {
        strategy_id: strategy_id.into(),
        strategy_version: "v1".into(),
        account_id: "main".into(),
        venue_id: "SIM".into(),
        data_fingerprint: "portfolio-exact-budget".into(),
        as_of: 1,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: None,
        risk_state: "ready".into(),
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
    };
    let slot = PortfolioStrategySlot::native(
        0,
        strategy_id,
        budget_raw,
        NativeBarStrategy::new(
            NativeSubNanoBuyOnce {
                submitted: false,
                quantity_exact,
            },
            context,
        ),
    )
    .with_reserved_capital_exact(budget_exact);
    PortfolioBarStrategy::new(vec![slot]).unwrap()
}

#[test]
fn strategy_budget_sums_each_pending_instrument_in_budget_currency() {
    let first_instrument = InstrumentId::parse("T1.SIM").unwrap();
    let second_instrument = InstrumentId::parse("T2.SIM").unwrap();
    let first_spec = exact_spot_spec(&first_instrument);
    let second_spec = exact_spot_spec(&second_instrument);
    let first_order = Order {
        client_id: 1,
        instrument: first_instrument.clone(),
        side: Side::Buy,
        qty: Quantity::ZERO,
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: Some(qx_core::OrderTrace {
            strategy_id: Some("alpha".into()),
            signal_id: None,
            intent_id: None,
            rule_version: None,
        }),
        policy: None,
        exact_amounts: Some(qx_core::ExactOrderAmounts {
            quantity: qx_core::ExactDecimal::parse("0.1").unwrap(),
            limit_price: None,
            filled: qx_core::ExactDecimal::ZERO,
            quote_budget: None,
            quote_filled: qx_core::ExactDecimal::ZERO,
            market_reference_price: None,
        }),
    };
    let candidate = Order {
        client_id: 2,
        instrument: second_instrument.clone(),
        side: Side::Buy,
        qty: Quantity::ZERO,
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: first_order.trace.clone(),
        policy: None,
        exact_amounts: Some(qx_core::ExactOrderAmounts {
            quantity: qx_core::ExactDecimal::parse("0.15").unwrap(),
            limit_price: None,
            filled: qx_core::ExactDecimal::ZERO,
            quote_budget: None,
            quote_filled: qx_core::ExactDecimal::ZERO,
            market_reference_price: None,
        }),
    };
    let mut budget = crate::backtest::portfolio_budget::PortfolioBudgetState::default();
    budget
        .accept_if_owned_exact(
            |_| Some(20 * SCALE),
            &first_order,
            qx_core::ExactDecimal::parse("0.1").unwrap(),
            qx_core::ExactDecimal::parse("100").unwrap(),
        )
        .unwrap();
    let markets = BTreeMap::from([
        (
            first_instrument.clone(),
            crate::backtest::portfolio_budget::PortfolioBudgetMarket {
                spec: first_spec,
                mark: qx_core::ExactDecimal::parse("100").unwrap(),
                margin: std::sync::Arc::new(NoMargin),
                multiplier: 1,
            },
        ),
        (
            second_instrument.clone(),
            crate::backtest::portfolio_budget::PortfolioBudgetMarket {
                spec: second_spec.clone(),
                mark: qx_core::ExactDecimal::parse("100").unwrap(),
                margin: std::sync::Arc::new(NoMargin),
                multiplier: 1,
            },
        ),
    ]);
    let (budget_raw, required) = budget
        .order_budget_check_exact(
            |_| Some(20 * SCALE),
            &candidate,
            qx_core::ExactDecimal::parse("0.15").unwrap(),
            qx_core::ExactDecimal::parse("100").unwrap(),
            qx_core::ExactDecimal::parse("100").unwrap(),
            Some(&second_spec),
            1,
            &NoMargin,
            Some(&markets),
            "USD",
        )
        .unwrap()
        .unwrap();

    assert_eq!(budget_raw, qx_core::ExactDecimal::parse("20").unwrap());
    assert_eq!(required, qx_core::ExactDecimal::parse("25").unwrap());

    let mut cross_currency_markets = markets.clone();
    cross_currency_markets
        .get_mut(&second_instrument)
        .unwrap()
        .spec
        .quote_currency = "EUR".into();
    assert!(matches!(
        budget.order_budget_check_exact(
            |_| Some(20 * SCALE),
            &candidate,
            qx_core::ExactDecimal::parse("0.15").unwrap(),
            qx_core::ExactDecimal::parse("100").unwrap(),
            qx_core::ExactDecimal::parse("100").unwrap(),
            Some(&second_spec),
            1,
            &NoMargin,
            Some(&cross_currency_markets),
            "USD",
        ),
        Err(qx_core::QxError::BusinessViolation(reason))
            if reason.contains("跨计价币种")
    ));
}

#[test]
fn strategy_budget_keeps_the_highest_pending_reference_price_per_instrument() {
    let instrument = InstrumentId::parse("T.SIM").unwrap();
    let spec = exact_spot_spec(&instrument);
    let order = |client_id, quantity: &str| Order {
        client_id,
        instrument: instrument.clone(),
        side: Side::Buy,
        qty: Quantity::ZERO,
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: Some(qx_core::OrderTrace {
            strategy_id: Some("alpha".into()),
            signal_id: None,
            intent_id: None,
            rule_version: None,
        }),
        policy: None,
        exact_amounts: Some(qx_core::ExactOrderAmounts {
            quantity: qx_core::ExactDecimal::parse(quantity).unwrap(),
            limit_price: None,
            filled: qx_core::ExactDecimal::ZERO,
            quote_budget: None,
            quote_filled: qx_core::ExactDecimal::ZERO,
            market_reference_price: None,
        }),
    };
    let first = order(1, "0.1");
    let second = order(2, "1");
    let candidate = order(3, "1");
    let mut budget = crate::backtest::portfolio_budget::PortfolioBudgetState::default();
    budget
        .accept_if_owned_exact(
            |_| Some(110 * SCALE),
            &first,
            qx_core::ExactDecimal::parse("0.1").unwrap(),
            qx_core::ExactDecimal::parse("1000").unwrap(),
        )
        .unwrap();
    budget
        .accept_if_owned_exact(
            |_| Some(110 * SCALE),
            &second,
            qx_core::ExactDecimal::ONE,
            qx_core::ExactDecimal::parse("10").unwrap(),
        )
        .unwrap();
    let markets = BTreeMap::from([(
        instrument.clone(),
        crate::backtest::portfolio_budget::PortfolioBudgetMarket {
            spec: spec.clone(),
            mark: qx_core::ExactDecimal::parse("10").unwrap(),
            margin: std::sync::Arc::new(NoMargin),
            multiplier: 1,
        },
    )]);

    let (budget_raw, required) = budget
        .order_budget_check_exact(
            |_| Some(110 * SCALE),
            &candidate,
            qx_core::ExactDecimal::ONE,
            qx_core::ExactDecimal::parse("10").unwrap(),
            qx_core::ExactDecimal::parse("10").unwrap(),
            Some(&spec),
            1,
            &NoMargin,
            Some(&markets),
            "USD",
        )
        .unwrap()
        .unwrap();

    assert_eq!(budget_raw, qx_core::ExactDecimal::parse("110").unwrap());
    assert!(required.checked_cmp(budget_raw).unwrap().is_gt());
}

#[test]
fn exact_subnano_order_uses_exact_exposure_under_reserved_capital_budget() {
    let instrument = InstrumentId::parse("T.SIM").unwrap();
    let mut config = simple_config();
    config.instrument_spec = Some(exact_spot_spec(&instrument));
    let mut strategy = native_subnano_portfolio("alpha", 1);
    let report = BacktestEngine::new(config)
        .run(
            &[
                Bar::new(1, 100 * SCALE, 100 * SCALE, 100 * SCALE, 100 * SCALE, SCALE),
                Bar::new(2, 101 * SCALE, 101 * SCALE, 101 * SCALE, 101 * SCALE, SCALE),
                Bar::new(3, 102 * SCALE, 102 * SCALE, 102 * SCALE, 102 * SCALE, SCALE),
            ],
            &mut strategy,
        )
        .unwrap();

    assert_replay_matches(&report);
    assert_eq!(report.fills.len(), 1);
    assert_eq!(
        report.fills[0].quantity_decimal(),
        qx_core::ExactDecimal::parse("0.000000000001").unwrap()
    );
    assert!(!report.event_log.events().iter().any(|event| matches!(
        event.kind,
        qx_core::EventKind::Rejected { ref reason, .. }
            if reason.contains("reserved_capital_raw")
    )));
}

#[test]
fn exact_reserved_capital_keeps_precision_below_the_raw_projection() {
    let instrument = InstrumentId::parse("T.SIM").unwrap();
    let bars = [
        Bar::new(1, 100 * SCALE, 100 * SCALE, 100 * SCALE, 100 * SCALE, SCALE),
        Bar::new(2, 100 * SCALE, 100 * SCALE, 100 * SCALE, 100 * SCALE, SCALE),
        Bar::new(3, 100 * SCALE, 100 * SCALE, 100 * SCALE, 100 * SCALE, SCALE),
    ];
    let mut legacy_config = simple_config();
    legacy_config.instrument_spec = Some(exact_spot_spec(&instrument));
    let quantity = qx_core::ExactDecimal::parse("0.000000000011").unwrap();

    let mut legacy_strategy = native_subnano_portfolio_with_exact_budget(
        "alpha",
        1,
        qx_core::ExactDecimal::parse("0.000000001").unwrap(),
        quantity,
    );
    let legacy = BacktestEngine::new(legacy_config)
        .run(&bars, &mut legacy_strategy)
        .unwrap();
    assert!(legacy.fills.is_empty());
    assert!(legacy.event_log.events().iter().any(|event| matches!(
        event.kind,
        qx_core::EventKind::Rejected { ref reason, .. }
            if reason.contains("reserved_capital_raw")
    )));

    let mut exact_strategy = native_subnano_portfolio_with_exact_budget(
        "alpha",
        1,
        qx_core::ExactDecimal::parse("0.0000000012").unwrap(),
        quantity,
    );
    let mut exact_config = simple_config();
    exact_config.instrument_spec = Some(exact_spot_spec(&instrument));
    let exact = BacktestEngine::new(exact_config)
        .run(&bars, &mut exact_strategy)
        .unwrap();
    assert_replay_matches(&exact);
    assert_eq!(exact.fills.len(), 1);
    assert_eq!(exact.fills[0].quantity_decimal(), quantity);
}

#[test]
fn exact_subnano_order_over_reserved_capital_budget_is_rejected_before_fill() {
    let instrument = InstrumentId::parse("T.SIM").unwrap();
    let mut config = simple_config();
    config.instrument_spec = Some(exact_spot_spec(&instrument));
    let mut strategy = native_subnano_portfolio("alpha", 1);
    let report = BacktestEngine::new(config)
        .run(
            &[
                Bar::new(
                    1,
                    100_000 * SCALE,
                    100_000 * SCALE,
                    100_000 * SCALE,
                    100_000 * SCALE,
                    SCALE,
                ),
                Bar::new(
                    2,
                    100_000 * SCALE,
                    100_000 * SCALE,
                    100_000 * SCALE,
                    100_000 * SCALE,
                    SCALE,
                ),
            ],
            &mut strategy,
        )
        .unwrap();

    assert!(report.fills.is_empty());
    assert!(report.event_log.events().iter().any(|event| matches!(
        event.kind,
        qx_core::EventKind::Rejected { ref reason, .. }
            if reason.contains("reserved_capital_raw")
    )));
    assert_replay_matches(&report);
}

struct BudgetBuyer {
    submitted: bool,
}

impl BarStrategy for BudgetBuyer {
    fn on_bar_orders_checked(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        _position: i128,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        if self.submitted {
            return Ok(Vec::new());
        }
        self.submitted = true;
        Ok(vec![Order {
            client_id: 0,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        }])
    }
}

#[test]
fn fixed_quantity_order_cannot_exceed_its_strategy_capital_budget() {
    let mut strategy = PortfolioBarStrategy::new(vec![PortfolioStrategySlot::new(
        0,
        "alpha",
        50 * SCALE,
        BudgetBuyer { submitted: false },
    )])
    .unwrap();
    let report = BacktestEngine::new(simple_config())
        .run(
            &[
                Bar::new(
                    1,
                    100 * SCALE,
                    101 * SCALE,
                    99 * SCALE,
                    100 * SCALE,
                    10 * SCALE,
                ),
                Bar::new(
                    2,
                    100 * SCALE,
                    101 * SCALE,
                    99 * SCALE,
                    100 * SCALE,
                    10 * SCALE,
                ),
            ],
            &mut strategy,
        )
        .unwrap();

    assert!(report.fills.is_empty());
    assert!(report.event_log.events().iter().any(|event| matches!(
        event.kind,
        qx_core::EventKind::Rejected { ref reason, .. }
            if reason.contains("reserved_capital_raw")
    )));
}

#[test]
fn fill_price_gap_that_breaks_strategy_budget_fails_closed_before_booking() {
    let mut strategy = PortfolioBarStrategy::new(vec![PortfolioStrategySlot::new(
        0,
        "alpha",
        150 * SCALE,
        BudgetBuyer { submitted: false },
    )])
    .unwrap();
    let result = BacktestEngine::new(simple_config()).run(
        &[
            Bar::new(
                1,
                100 * SCALE,
                101 * SCALE,
                99 * SCALE,
                100 * SCALE,
                10 * SCALE,
            ),
            Bar::new(
                2,
                200 * SCALE,
                201 * SCALE,
                199 * SCALE,
                200 * SCALE,
                10 * SCALE,
            ),
        ],
        &mut strategy,
    );

    assert!(result.is_err());
}

struct RestingBudgetBuyer;

impl BarStrategy for RestingBudgetBuyer {
    fn on_bar_orders_checked(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        _position: i128,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        Ok(vec![Order {
            client_id: 0,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(qx_core::Price::from_raw(50 * SCALE)),
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        }])
    }
}

#[test]
fn unfilled_resting_orders_consume_their_strategy_budget() {
    let mut strategy = PortfolioBarStrategy::new(vec![PortfolioStrategySlot::new(
        0,
        "alpha",
        100 * SCALE,
        RestingBudgetBuyer,
    )])
    .unwrap();
    let report = BacktestEngine::new(simple_config())
        .run(
            &[
                Bar::new(
                    1,
                    100 * SCALE,
                    101 * SCALE,
                    99 * SCALE,
                    100 * SCALE,
                    10 * SCALE,
                ),
                Bar::new(
                    2,
                    100 * SCALE,
                    101 * SCALE,
                    99 * SCALE,
                    100 * SCALE,
                    10 * SCALE,
                ),
                Bar::new(
                    3,
                    100 * SCALE,
                    101 * SCALE,
                    99 * SCALE,
                    100 * SCALE,
                    10 * SCALE,
                ),
                Bar::new(
                    4,
                    100 * SCALE,
                    101 * SCALE,
                    99 * SCALE,
                    100 * SCALE,
                    10 * SCALE,
                ),
            ],
            &mut strategy,
        )
        .unwrap();
    let accepted = report
        .event_log
        .events()
        .iter()
        .filter(|event| matches!(event.kind, qx_core::EventKind::Accepted { .. }))
        .count();
    let rejected = report
        .event_log
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                qx_core::EventKind::Rejected { ref reason, .. }
                    if reason.contains("reserved_capital_raw")
            )
        })
        .count();

    assert_eq!(accepted, 2);
    assert_eq!(rejected, 1);
    assert!(report.fills.is_empty());
}

struct RestingCashBuyer;

impl BarStrategy for RestingCashBuyer {
    fn on_bar_orders_checked(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        _position: i128,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        Ok(vec![Order {
            client_id: 0,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(6),
            limit: Some(qx_core::Price::from_raw(50 * SCALE)),
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        }])
    }
}

#[test]
fn resting_spot_orders_reserve_account_cash_across_bars_before_filling() {
    let mut strategy = RestingCashBuyer;
    let bars = (1..=6)
        .map(|ts| {
            let open = if ts >= 5 { 50 * SCALE } else { 100 * SCALE };
            Bar::new(ts, open, open + SCALE, open - SCALE, open, 10 * SCALE)
        })
        .collect::<Vec<_>>();
    let report = BacktestEngine::new(simple_config())
        .run(&bars, &mut strategy)
        .unwrap();

    let accepted = report
        .event_log
        .events()
        .iter()
        .filter(|event| matches!(event.kind, qx_core::EventKind::Accepted { .. }))
        .count();
    let cash_rejections = report
        .event_log
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                qx_core::EventKind::Rejected { ref reason, .. }
                    if reason.contains("账户可用现金")
            )
        })
        .count();

    assert_eq!(
        accepted, 3,
        "three resting orders reserve the 1,000 cash budget"
    );
    assert_eq!(cash_rejections, 2);
    assert_eq!(report.fills.len(), 3);
}
