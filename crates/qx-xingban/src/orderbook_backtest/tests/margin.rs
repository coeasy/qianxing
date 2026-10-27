use super::*;

struct MarginProbe {
    observed: Arc<Mutex<Vec<Option<i128>>>>,
    emitted: bool,
}

impl OrderBookStrategy for MarginProbe {
    fn on_order_book(
        &mut self,
        _snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        Ok(Vec::new())
    }

    fn on_order_book_actions_with_margin(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
        _open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        self.observed.lock().unwrap().push(available_margin_raw);
        if self.emitted {
            return Ok(OrderBookActions::default());
        }
        self.emitted = true;
        Ok(OrderBookActions {
            orders: vec![Order {
                client_id: 1,
                instrument: snapshot.instrument.clone(),
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: None,
                exact_amounts: None,
            }],
            cancel_client_ids: Vec::new(),
        })
    }
}

#[test]
fn spot_margin_snapshot_reaches_strategy_after_each_filled_order() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let mut strategy = MarginProbe {
        observed: observed.clone(),
        emitted: false,
    };
    let report = OrderBookBacktestEngine::new(l2_config(0))
        .run(
            &[snapshot(1, 100), snapshot(2, 101), snapshot(3, 102)],
            &mut strategy,
        )
        .expect("spot run with margin snapshots");
    assert_eq!(report.fills.len(), 1);
    assert_eq!(
        *observed.lock().unwrap(),
        vec![
            Some(Money::from_i64(10_000).raw()),
            Some(Money::from_i64(9_899).raw()),
            Some(Money::from_i64(9_899).raw()),
        ]
    );
}

struct DerivativeMarginProbe {
    observed: Arc<Mutex<Vec<Option<i128>>>>,
    emitted: bool,
}

impl OrderBookStrategy for DerivativeMarginProbe {
    fn on_order_book(
        &mut self,
        _snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        Ok(Vec::new())
    }

    fn on_order_book_actions_with_margin(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
        _open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        self.observed.lock().unwrap().push(available_margin_raw);
        if self.emitted {
            return Ok(OrderBookActions::default());
        }
        self.emitted = true;
        Ok(OrderBookActions {
            orders: vec![Order {
                client_id: 7,
                instrument: snapshot.instrument.clone(),
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: Some(qx_core::OrderPolicy {
                    leverage: 10,
                    ..qx_core::OrderPolicy::default()
                }),
                exact_amounts: None,
            }],
            cancel_client_ids: Vec::new(),
        })
    }
}

#[test]
fn derivative_margin_snapshot_is_marked_equity_less_position_exposure() {
    let instrument = qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec = qx_core::TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: qx_core::TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: qx_core::SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        min_notional_raw: 0,
        max_leverage: 10,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
        exact_rules: None,
    };
    let config = OrderBookBacktestConfig {
        instrument,
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: qx_core::Money::from_i64(10_000),
        fee_bps: 0,
        instrument_spec: Some(spec),
        risk: qx_zhenlu::RiskGate::conservative_default(),
        data_tier: crate::DataTier::L2L3,
    };
    let observed = Arc::new(Mutex::new(Vec::new()));
    let mut strategy = DerivativeMarginProbe {
        observed: observed.clone(),
        emitted: false,
    };
    let report = OrderBookBacktestEngine::new(config)
        .run(&[snapshot(1, 100), snapshot(2, 100)], &mut strategy)
        .unwrap();

    assert_eq!(report.fills.len(), 1);
    let observed = observed.lock().unwrap();
    assert_eq!(observed[0], Some(qx_core::Money::from_i64(10_000).raw()));
    assert!(observed[1].is_some_and(|available| available < observed[0].unwrap()));
}

#[test]
fn historical_derivative_funding_updates_exact_ledger_and_report_metrics() {
    let instrument = qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec = qx_core::TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: qx_core::TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: qx_core::SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        min_notional_raw: 0,
        max_leverage: 10,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
        exact_rules: None,
    };
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: qx_core::Money::from_i64(10_000),
        fee_bps: 0,
        instrument_spec: Some(spec),
        risk: qx_zhenlu::RiskGate::conservative_default(),
        data_tier: crate::DataTier::L2L3,
    };
    let settlement_ts = 15_123_456;
    let mut strategy = DerivativeMarginProbe {
        observed: Arc::new(Mutex::new(Vec::new())),
        emitted: false,
    };
    let report = OrderBookBacktestEngine::new(config)
        .with_funding_events(vec![qx_core::FundingRateEvent {
            instrument,
            settlement_ts,
            rate_bps: 100,
            funding_interval_ms: Some(60 * 60 * 1_000),
        }])
        .run(
            &[
                snapshot(1, 100),
                snapshot(2, 101),
                snapshot(settlement_ts, 100),
            ],
            &mut strategy,
        )
        .unwrap();

    assert_eq!(report.fills.len(), 1);
    assert_eq!(
        report.ledger.cash_for_exact("main", "USDT"),
        qx_core::ExactDecimal::parse("9999.005").unwrap()
    );
    let funding = report
        .ledger
        .entries()
        .iter()
        .find(|entry| entry.kind == qx_core::LedgerEntryKind::Funding)
        .expect("funding ledger fact");
    assert_eq!(
        funding.exact_amounts.unwrap().amount,
        qx_core::ExactDecimal::parse("-0.995").unwrap()
    );
    assert_eq!(
        report.exact_metrics_json().unwrap()["funding"]["settlement_count"],
        1
    );
    ReplayVerifier::verify(report.event_log.events(), &report.ledger).unwrap();
}

#[test]
fn constant_funding_rate_settles_on_utc_eight_hour_boundary() {
    let instrument = qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: qx_core::Money::from_i64(10_000),
        fee_bps: 0,
        instrument_spec: Some(qx_core::TradingInstrumentSpec {
            instrument,
            product: qx_core::TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: qx_core::SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            min_notional_raw: 0,
            max_leverage: 10,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
            exact_rules: None,
        }),
        risk: qx_zhenlu::RiskGate::conservative_default(),
        data_tier: crate::DataTier::L2L3,
    };
    let mut strategy = DerivativeMarginProbe {
        observed: Arc::new(Mutex::new(Vec::new())),
        emitted: false,
    };
    let settlement_ts = qx_core::FUNDING_INTERVAL_MS;
    let report = OrderBookBacktestEngine::new(config)
        .with_funding_bps(100)
        .run(
            &[
                snapshot(1, 100),
                snapshot(2, 101),
                snapshot(settlement_ts, 100),
            ],
            &mut strategy,
        )
        .unwrap();

    let funding = report
        .ledger
        .entries()
        .iter()
        .filter(|entry| entry.kind == qx_core::LedgerEntryKind::Funding)
        .collect::<Vec<_>>();
    assert_eq!(funding.len(), 1);
    assert_eq!(funding[0].ts, settlement_ts);
    assert_eq!(
        funding[0].exact_amounts.unwrap().amount,
        qx_core::ExactDecimal::parse("-0.995").unwrap()
    );
}

struct PortfolioExactBuyer {
    emitted: bool,
}

impl OrderBookStrategy for PortfolioExactBuyer {
    fn supports_exact_orders(&self) -> bool {
        true
    }

    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if self.emitted {
            return Ok(Vec::new());
        }
        self.emitted = true;
        Ok(vec![Order {
            client_id: 1,
            instrument: snapshot.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::ZERO,
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "shared".into(),
            trace: None,
            policy: Some(qx_core::OrderPolicy {
                margin_mode: qx_core::MarginMode::Cross,
                position_mode: qx_core::PositionMode::OneWay,
                position_side: qx_core::PositionSide::Net,
                leverage: 10,
                ..qx_core::OrderPolicy::default()
            }),
            exact_amounts: Some(ExactOrderAmounts {
                quantity: qx_core::ExactDecimal::ONE,
                limit_price: None,
                filled: qx_core::ExactDecimal::ZERO,
                quote_budget: None,
                quote_filled: qx_core::ExactDecimal::ZERO,
                market_reference_price: None,
            }),
        }])
    }
}

fn exact_derivative_spec(instrument: qx_core::InstrumentId) -> qx_core::TradingInstrumentSpec {
    qx_core::TradingInstrumentSpec {
        instrument,
        product: qx_core::TradingProduct::Perpetual,
        base_currency: "BASE".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: qx_core::SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        min_notional_raw: 0,
        max_leverage: 10,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
        exact_rules: Some(qx_core::ExactTradingRules {
            contract_size: qx_core::ExactDecimal::ONE,
            price_tick: qx_core::ExactDecimal::parse("0.01").unwrap(),
            quantity_step: qx_core::ExactDecimal::parse("0.001").unwrap(),
            minimum_quantity: qx_core::ExactDecimal::parse("0.001").unwrap(),
            minimum_notional: qx_core::ExactDecimal::ZERO,
        }),
    }
}

fn exact_derivative_snapshot(
    instrument: &qx_core::InstrumentId,
    ts: u64,
    bid: &str,
    ask: &str,
) -> OrderBookSnapshot {
    OrderBookSnapshot {
        instrument: instrument.clone(),
        ts,
        sequence: ts,
        bids: vec![crate::BookLevel::from_exact(
            qx_core::ExactDecimal::parse(bid).unwrap(),
            qx_core::ExactDecimal::parse("10").unwrap(),
        )
        .unwrap()],
        asks: vec![crate::BookLevel::from_exact(
            qx_core::ExactDecimal::parse(ask).unwrap(),
            qx_core::ExactDecimal::parse("10").unwrap(),
        )
        .unwrap()],
    }
}

#[test]
fn shared_multi_market_derivatives_settle_exact_funding_by_instrument() {
    let btc = qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let eth = qx_core::InstrumentId::parse("ETHUSDT.BINANCE").unwrap();
    let settlement_ts = qx_core::FUNDING_INTERVAL_MS;
    let btc_snapshots = [
        exact_derivative_snapshot(&btc, 1, "99", "101"),
        exact_derivative_snapshot(&btc, 2, "99", "101"),
        exact_derivative_snapshot(&btc, settlement_ts, "99", "101"),
    ];
    let eth_snapshots = [
        exact_derivative_snapshot(&eth, 1, "199", "201"),
        exact_derivative_snapshot(&eth, 2, "199", "201"),
        exact_derivative_snapshot(&eth, settlement_ts, "199", "201"),
    ];
    let mut btc_strategy = PortfolioExactBuyer { emitted: false };
    let mut eth_strategy = PortfolioExactBuyer { emitted: false };
    let markets = vec![
        OrderBookPortfolioMarket {
            instrument_spec: exact_derivative_spec(btc.clone()),
            data_tier: crate::DataTier::L2L3,
            priority: 0,
            snapshots: &btc_snapshots,
            execution_model: crate::OrderBookExecutionModel::new(0).unwrap(),
            strategy: &mut btc_strategy,
        },
        OrderBookPortfolioMarket {
            instrument_spec: exact_derivative_spec(eth.clone()),
            data_tier: crate::DataTier::L2L3,
            priority: 1,
            snapshots: &eth_snapshots,
            execution_model: crate::OrderBookExecutionModel::new(0).unwrap(),
            strategy: &mut eth_strategy,
        },
    ];
    let report = OrderBookPortfolioBacktestEngine::new(
        "shared",
        "USDT",
        Money::from_i64(10_000),
        RiskGate::conservative_default(),
        markets,
    )
    .unwrap()
    .with_funding_bps(std::collections::BTreeMap::from([(btc, 100), (eth, 100)]))
    .run()
    .unwrap();

    assert_eq!(report.fills.len(), 2);
    let funding = report
        .ledger
        .entries()
        .iter()
        .filter(|entry| entry.kind == qx_core::LedgerEntryKind::Funding)
        .collect::<Vec<_>>();
    assert_eq!(funding.len(), 2);
    assert_eq!(funding[0].ts, settlement_ts);
    assert_eq!(funding[1].ts, settlement_ts);
    assert_eq!(
        funding
            .iter()
            .map(|entry| entry.exact_amounts.unwrap().amount)
            .try_fold(qx_core::ExactDecimal::ZERO, |sum, amount| sum
                .checked_add(amount)),
        Some(qx_core::ExactDecimal::parse("-3").unwrap())
    );
    assert_eq!(
        report.ledger.cash_for_exact("shared", "USDT"),
        qx_core::ExactDecimal::parse("9997").unwrap()
    );
    assert_eq!(
        report.exact_metrics_json().unwrap()["funding"]["settlement_count"],
        2
    );
    report.verify().unwrap();
}

#[test]
fn funding_rejects_a_stale_mark_when_settlement_falls_between_snapshots() {
    let instrument = qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: qx_core::Money::from_i64(10_000),
        fee_bps: 0,
        instrument_spec: Some(qx_core::TradingInstrumentSpec {
            instrument: instrument.clone(),
            product: qx_core::TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: qx_core::SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            min_notional_raw: 0,
            max_leverage: 10,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
            exact_rules: None,
        }),
        risk: qx_zhenlu::RiskGate::conservative_default(),
        data_tier: crate::DataTier::L2L3,
    };
    let settlement_ts = 2 * qx_core::FUNDING_INTERVAL_MS;
    let mut strategy = DerivativeMarginProbe {
        observed: Arc::new(Mutex::new(Vec::new())),
        emitted: false,
    };
    let error = OrderBookBacktestEngine::new(config)
        .with_funding_events(vec![qx_core::FundingRateEvent {
            instrument,
            settlement_ts,
            rate_bps: 100,
            funding_interval_ms: Some(60 * 60 * 1_000),
        }])
        .run(
            &[
                snapshot(1, 100),
                snapshot(2, 101),
                snapshot(settlement_ts + 1, 100),
            ],
            &mut strategy,
        )
        .err()
        .expect("stale funding mark must fail closed");
    assert!(error.to_string().contains("标记价已过期"), "{error}");
}
