use super::*;
use std::sync::{Arc, Mutex};

struct BuyThenCloseOwnPosition;

impl qx_strategy::Strategy for BuyThenCloseOwnPosition {
    fn on_event(
        &mut self,
        context: &qx_strategy::StrategyContext,
        event: &qx_strategy::MarketEvent,
    ) -> Result<qx_strategy::StrategyDecision, String> {
        let qx_strategy::MarketEvent::Bar { instrument, ts, .. } = event else {
            return Err("test strategy expects Bar events".into());
        };
        let position = context
            .positions
            .get(&instrument.to_string())
            .copied()
            .unwrap_or_default();
        let intent = match position {
            0 => Some((Side::Buy, false)),
            value if value == Quantity::from_i64(1).raw() => Some((Side::Sell, true)),
            _ => None,
        };
        Ok(qx_strategy::StrategyDecision {
            schema_version: qx_strategy::STRATEGY_API_VERSION,
            request_id: format!("{}-{ts}", context.strategy_id),
            strategy_id: context.strategy_id.clone(),
            signal_id: *ts,
            confidence: 1,
            priority: 0,
            expires_at: *ts,
            intents: intent
                .map(|(side, reduce_only)| {
                    vec![qx_strategy::StrategyOrderIntent {
                        intent_id: *ts,
                        instrument: instrument.clone(),
                        side,
                        qty: Quantity::from_i64(1),
                        limit: None,
                        policy: None,
                        reduce_only,
                        post_only: false,

                        qty_exact: None,
                        limit_price_exact: None,
                        quote_budget: None,
                    }]
                })
                .unwrap_or_default(),
        })
    }
}

struct ScheduledBuyer {
    id: &'static str,
    qty_units: i64,
    submitted: bool,
    observed_decisions: Arc<Mutex<Vec<(String, i128)>>>,
    observed_fills: Arc<Mutex<Vec<(String, String)>>>,
}

impl BarStrategy for ScheduledBuyer {
    fn on_bar_orders_with_margin_checked(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        _position: i128,
        available_margin_raw: Option<i128>,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        self.observed_decisions
            .lock()
            .unwrap()
            .push((self.id.into(), available_margin_raw.unwrap_or_default()));
        if self.submitted {
            return Ok(Vec::new());
        }
        self.submitted = true;
        Ok(vec![Order {
            client_id: 0,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(self.qty_units),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: Some(qx_core::OrderTrace {
                strategy_id: Some(self.id.into()),
                signal_id: Some(1),
                intent_id: Some(1),
                rule_version: Some("portfolio-test-v1".into()),
            }),
            policy: None,
            exact_amounts: None,
        }])
    }

    fn on_fill(&mut self, order: &Order, _fill: &qx_core::Fill) -> Result<(), qx_core::QxError> {
        let owner = order
            .trace
            .as_ref()
            .and_then(|trace| trace.strategy_id.as_deref())
            .unwrap_or_default()
            .to_string();
        self.observed_fills
            .lock()
            .unwrap()
            .push((self.id.into(), owner));
        Ok(())
    }
}

#[test]
fn portfolio_bar_strategy_schedules_by_priority_and_id_on_one_authoritative_ledger() {
    let decisions = Arc::new(Mutex::new(Vec::new()));
    let attributed_fills = Arc::new(Mutex::new(Vec::new()));
    let child = |id, priority, capital| {
        PortfolioStrategySlot::new(
            priority,
            id,
            capital,
            ScheduledBuyer {
                id,
                qty_units: 1,
                submitted: false,
                observed_decisions: Arc::clone(&decisions),
                observed_fills: Arc::clone(&attributed_fills),
            },
        )
    };
    let mut strategy = PortfolioBarStrategy::new(vec![
        child("zeta", 0, 300 * SCALE),
        child("beta", 1, 200 * SCALE),
        child("alpha", 0, 100 * SCALE),
    ])
    .unwrap();
    let instrument = InstrumentId::parse("T.SIM").unwrap();
    let report = BacktestEngine::new(simple_config())
        .run(
            &[
                Bar::new(1, 100, 101, 99, 100, 10),
                Bar::new(2, 102, 103, 101, 102, 10),
                Bar::new(3, 104, 105, 103, 104, 10),
            ],
            &mut strategy,
        )
        .unwrap();

    assert_eq!(report.fills.len(), 3);
    assert_eq!(
        report
            .ledger
            .position_for("main", &instrument)
            .quantity
            .raw(),
        Quantity::from_i64(3).raw()
    );
    assert_eq!(
        &decisions.lock().unwrap()[..3],
        &[
            ("alpha".into(), 100 * SCALE),
            ("zeta".into(), 300 * SCALE),
            ("beta".into(), 200 * SCALE),
        ]
    );
    assert_eq!(
        *attributed_fills.lock().unwrap(),
        vec![
            ("alpha".into(), "alpha".into()),
            ("zeta".into(), "zeta".into()),
            ("beta".into(), "beta".into()),
        ]
    );
    qx_core::ReplayVerifier::verify(report.event_log.events(), &report.ledger).unwrap();
}

#[test]
fn portfolio_rejects_order_attribution_that_disagrees_with_its_slot() {
    let instrument = InstrumentId::parse("T.SIM").unwrap();
    let mut strategy = PortfolioBarStrategy::new(vec![PortfolioStrategySlot::new(
        0,
        "alpha",
        100 * SCALE,
        ScheduledBuyer {
            id: "alpha",
            qty_units: 1,
            submitted: false,
            observed_decisions: Arc::new(Mutex::new(Vec::new())),
            observed_fills: Arc::new(Mutex::new(Vec::new())),
        },
    )])
    .unwrap();
    let result = strategy.on_bar_orders_with_margin_checked(
        &[Bar::new(1, 100, 101, 99, 100, 10)],
        &instrument,
        2,
        0,
        Some(100 * SCALE),
    );
    let attributed = result.unwrap();
    assert_eq!(
        attributed[0]
            .trace
            .as_ref()
            .and_then(|trace| trace.strategy_id.as_deref()),
        Some("alpha")
    );
    let mut mismatch = PortfolioBarStrategy::new(vec![PortfolioStrategySlot::new(
        0,
        "alpha",
        100 * SCALE,
        MisattributedBuyer,
    )])
    .unwrap();
    assert!(mismatch
        .on_bar_orders_with_margin_checked(
            &[Bar::new(1, 100, 101, 99, 100, 10)],
            &instrument,
            2,
            0,
            Some(100 * SCALE),
        )
        .is_err());
}

struct MisattributedBuyer;

impl BarStrategy for MisattributedBuyer {
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
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: Some(qx_core::OrderTrace {
                strategy_id: Some("beta".into()),
                signal_id: None,
                intent_id: None,
                rule_version: None,
            }),
            policy: None,
            exact_amounts: None,
        }])
    }
}

#[test]
fn portfolio_strategy_refuses_empty_duplicate_and_unbounded_slots() {
    let strategy = || ScheduledBuyer {
        id: "alpha",
        qty_units: 1,
        submitted: false,
        observed_decisions: Arc::new(Mutex::new(Vec::new())),
        observed_fills: Arc::new(Mutex::new(Vec::new())),
    };
    assert!(PortfolioBarStrategy::new(Vec::new()).is_err());
    assert!(PortfolioBarStrategy::new(vec![
        PortfolioStrategySlot::new(0, "alpha", SCALE, strategy()),
        PortfolioStrategySlot::new(1, "alpha", SCALE, strategy()),
    ])
    .is_err());
    assert!(PortfolioBarStrategy::new(vec![
        PortfolioStrategySlot::new(0, "alpha", 0, strategy(),)
    ])
    .is_err());
}

#[test]
fn native_children_trade_against_their_own_fills_not_the_shared_net_position() {
    let instrument = InstrumentId::parse("T.SIM").unwrap();
    let native = |strategy_id: &str| {
        NativeBarStrategy::new(
            BuyThenCloseOwnPosition,
            qx_strategy::StrategyContext {
                strategy_id: strategy_id.into(),
                strategy_version: "portfolio-test-v1".into(),
                account_id: "main".into(),
                venue_id: "SIM".into(),
                data_fingerprint: "portfolio-bars".into(),
                as_of: 1,
                positions_exact: std::collections::BTreeMap::new(),
                cash_exact: std::collections::BTreeMap::new(),
                available_margin_exact: None,
                positions: std::collections::BTreeMap::new(),
                cash: std::collections::BTreeMap::from([("USD".into(), 1_000 * SCALE)]),
                available_margin_raw: Some(1_000 * SCALE),
                risk_state: "backtest".into(),
            },
        )
    };
    let mut first = native("first");
    first.initialize().unwrap();
    let mut second = native("second");
    second.initialize().unwrap();
    let mut strategy = PortfolioBarStrategy::new(vec![
        PortfolioStrategySlot::native(0, "first", 500 * SCALE, first),
        PortfolioStrategySlot::native(1, "second", 500 * SCALE, second),
    ])
    .unwrap();
    let report = BacktestEngine::new(simple_config())
        .run(
            &[
                Bar::new(1, 100, 101, 99, 100, 10),
                Bar::new(2, 102, 103, 101, 102, 10),
                Bar::new(3, 104, 105, 103, 104, 10),
            ],
            &mut strategy,
        )
        .unwrap();

    assert_eq!(report.fills.len(), 4);
    assert_eq!(
        report
            .ledger
            .position_for("main", &instrument)
            .quantity
            .raw(),
        0
    );
    assert_eq!(
        report
            .fills
            .iter()
            .filter_map(|fill| fill.strategy_id.as_deref())
            .collect::<Vec<_>>(),
        vec!["first", "second", "first", "second"]
    );
}

#[test]
fn shared_account_funding_is_reserved_once_in_deterministic_priority_order() {
    let observed_fills = Arc::new(Mutex::new(Vec::new()));
    let mut strategy = PortfolioBarStrategy::new(vec![
        PortfolioStrategySlot::new(
            0,
            "beta",
            900 * SCALE,
            ScheduledBuyer {
                id: "beta",
                qty_units: 6,
                submitted: false,
                observed_decisions: Arc::new(Mutex::new(Vec::new())),
                observed_fills: Arc::clone(&observed_fills),
            },
        ),
        PortfolioStrategySlot::new(
            0,
            "alpha",
            900 * SCALE,
            ScheduledBuyer {
                id: "alpha",
                qty_units: 6,
                submitted: false,
                observed_decisions: Arc::new(Mutex::new(Vec::new())),
                observed_fills: Arc::clone(&observed_fills),
            },
        ),
    ])
    .unwrap();
    let instrument = InstrumentId::parse("T.SIM").unwrap();
    let report = BacktestEngine::new(simple_config())
        .run(
            &[
                Bar::new(
                    1,
                    100 * SCALE,
                    101 * SCALE,
                    99 * SCALE,
                    100 * SCALE,
                    100 * SCALE,
                ),
                Bar::new(
                    2,
                    100 * SCALE,
                    101 * SCALE,
                    99 * SCALE,
                    100 * SCALE,
                    100 * SCALE,
                ),
            ],
            &mut strategy,
        )
        .unwrap();

    assert_eq!(report.fills.len(), 1);
    assert_eq!(
        report
            .ledger
            .position_for("main", &instrument)
            .quantity
            .raw(),
        Quantity::from_i64(6).raw()
    );
    assert_eq!(
        *observed_fills.lock().unwrap(),
        vec![("alpha".into(), "alpha".into())]
    );
    assert!(report.ledger.cash_for("main", "USD") >= 0);
}
