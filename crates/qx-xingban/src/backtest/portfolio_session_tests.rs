use super::*;
use crate::{NextBarOpenFillModel, NoMargin, ZeroLatency};
use qx_core::{Order, OrderStatus, Quantity, Side, TradingProduct};

struct SubmitBoth {
    first: InstrumentId,
    second: InstrumentId,
    emitted: bool,
    fills: Vec<InstrumentId>,
}

struct SubmitToSharedTargetFromEachMarket {
    target: InstrumentId,
    submit_at_ts: u64,
    submitted_by: std::collections::BTreeSet<InstrumentId>,
    fill_sources: Vec<String>,
}

impl BarStrategy for SubmitToSharedTargetFromEachMarket {
    fn on_bar_orders(
        &mut self,
        _history: &[Bar],
        callback_instrument: &InstrumentId,
        ts: u64,
        _position: i128,
    ) -> Vec<Order> {
        if ts != self.submit_at_ts {
            return Vec::new();
        }
        if !self.submitted_by.insert(callback_instrument.clone()) {
            return Vec::new();
        }
        vec![Order {
            client_id: self.submitted_by.len() as u64,
            instrument: self.target.clone(),
            side: Side::Buy,
            qty: Quantity::from_raw(SCALE),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "shared-account".into(),
            trace: Some(qx_core::OrderTrace {
                strategy_id: Some(callback_instrument.to_string()),
                signal_id: None,
                intent_id: None,
                rule_version: Some("same-visible-batch-v1".into()),
            }),
            policy: None,
        }]
    }

    fn on_fill(&mut self, order: &Order, _fill: &Fill) -> Result<(), qx_core::QxError> {
        self.fill_sources.push(
            order
                .trace
                .as_ref()
                .and_then(|trace| trace.strategy_id.clone())
                .unwrap_or_default(),
        );
        Ok(())
    }
}

impl BarStrategy for SubmitBoth {
    fn on_bar_orders(
        &mut self,
        _history: &[Bar],
        _instrument: &InstrumentId,
        _ts: u64,
        _position: i128,
    ) -> Vec<Order> {
        if self.emitted {
            return Vec::new();
        }
        self.emitted = true;
        [self.first.clone(), self.second.clone()]
            .into_iter()
            .enumerate()
            .map(|(index, instrument)| Order {
                client_id: index as u64 + 1,
                instrument,
                side: Side::Buy,
                qty: Quantity::from_raw(6 * SCALE),
                limit: None,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: "shared-account".into(),
                trace: None,
                policy: None,
            })
            .collect()
    }

    fn on_fill(&mut self, order: &Order, _fill: &Fill) -> Result<(), qx_core::QxError> {
        self.fills.push(order.instrument.clone());
        Ok(())
    }
}

struct NoopStrategy;

impl BarStrategy for NoopStrategy {}

fn spot_spec(instrument: &InstrumentId, base: &str) -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: TradingProduct::Spot,
        base_currency: base.into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: false,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        min_notional_raw: 0,
        max_leverage: 1,
        maintenance_margin_bps: 0,
        valid_from: 0,
        valid_to: None,
    }
}

fn market<'a>(
    instrument: InstrumentId,
    priority: u32,
    bars: &'a [Bar],
    base: &str,
) -> PortfolioBarMarket<'a> {
    PortfolioBarMarket {
        spec: spot_spec(&instrument, base),
        instrument,
        priority,
        bars,
        fill: Box::new(NextBarOpenFillModel),
        fee: Box::new(qx_core::ZeroFeeModel),
        latency: Box::new(ZeroLatency),
        margin: Box::new(NoMargin),
        seed: 7,
    }
}

fn run_with_priorities(btc_priority: u32, eth_priority: u32) -> PortfolioBarSessionReport {
    let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let eth = InstrumentId::parse("ETHUSDT.KRAKEN").unwrap();
    let bars = [
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
        Bar::new(
            3,
            100 * SCALE,
            101 * SCALE,
            99 * SCALE,
            100 * SCALE,
            100 * SCALE,
        ),
    ];
    let btc_bars = bars;
    let eth_bars = bars;
    let engine = PortfolioBarEngine::new(
        PortfolioBarSessionConfig {
            account_id: "shared-account".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_raw(1_000 * SCALE),
            risk: RiskGate::conservative_default(),
            funding_bps: BTreeMap::new(),
            funding_events: Vec::new(),
        },
        vec![
            market(btc.clone(), btc_priority, &btc_bars, "BTC"),
            market(eth.clone(), eth_priority, &eth_bars, "ETH"),
        ],
    )
    .unwrap();
    let mut strategy = SubmitBoth {
        first: btc,
        second: eth,
        emitted: false,
        fills: Vec::new(),
    };
    engine.run(&mut strategy).unwrap()
}

#[test]
fn shared_session_routes_cross_market_orders_through_one_cash_ledger_and_replay() {
    let report = run_with_priorities(0, 1);
    assert_eq!(report.fills.len(), 1);
    assert_eq!(
        report.fills_by_instrument[&InstrumentId::parse("BTCUSDT.BINANCE").unwrap()].len(),
        1
    );
    assert_eq!(
        report.ledger.cash_for("shared-account", "USDT"),
        400 * SCALE
    );
    assert_eq!(
        report
            .ledger
            .position_for(
                "shared-account",
                &InstrumentId::parse("BTCUSDT.BINANCE").unwrap()
            )
            .quantity
            .raw(),
        6 * SCALE
    );
    assert!(report
        .event_log
        .events()
        .iter()
        .any(|event| matches!(event.kind, EventKind::Rejected { .. })));
    report.verify().unwrap();
}

#[test]
fn shared_session_bounds_instrument_equity_samples_and_keeps_terminal_points() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let reference = InstrumentId::parse("ETHUSDT.KRAKEN").unwrap();
    let bars = (1..=2_005)
        .map(|ts| {
            Bar::new(
                ts,
                100 * SCALE,
                101 * SCALE,
                99 * SCALE,
                100 * SCALE,
                100 * SCALE,
            )
        })
        .collect::<Vec<_>>();
    let engine = PortfolioBarEngine::new(
        PortfolioBarSessionConfig {
            account_id: "shared-account".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_raw(1_000 * SCALE),
            risk: RiskGate::conservative_default(),
            funding_bps: BTreeMap::new(),
            funding_events: Vec::new(),
        },
        vec![
            market(instrument.clone(), 0, &bars, "BTC"),
            market(reference, 1, &bars, "ETH"),
        ],
    )
    .unwrap();
    let report = engine.run(&mut NoopStrategy).unwrap();
    let curve = &report.instrument_equity_contributions[&instrument];
    assert_eq!(report.equity.len(), bars.len() * 2);
    assert!(curve.len() <= 1_000);
    assert_eq!(curve.first().map(|point| point.0), Some(0));
    assert_eq!(
        curve.last().map(|point| point.0),
        Some(report.equity.len() as u64 - 1)
    );
    assert!(curve.windows(2).all(|pair| pair[0].0 < pair[1].0));
    report.verify().unwrap();
}

#[test]
fn same_visible_timestamp_orders_from_multiple_callbacks_share_one_stable_batch() {
    let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let eth = InstrumentId::parse("ETHUSDT.KRAKEN").unwrap();
    let bars = [
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
        Bar::new(
            3,
            100 * SCALE,
            101 * SCALE,
            99 * SCALE,
            100 * SCALE,
            100 * SCALE,
        ),
    ];
    let btc_bars = bars;
    let eth_bars = bars;
    let engine = PortfolioBarEngine::new(
        PortfolioBarSessionConfig {
            account_id: "shared-account".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_raw(1_000 * SCALE),
            risk: RiskGate::conservative_default(),
            funding_bps: BTreeMap::new(),
            funding_events: Vec::new(),
        },
        vec![
            market(btc.clone(), 0, &btc_bars, "BTC"),
            market(eth.clone(), 1, &eth_bars, "ETH"),
        ],
    )
    .unwrap();
    let mut strategy = SubmitToSharedTargetFromEachMarket {
        target: btc.clone(),
        submit_at_ts: 2,
        submitted_by: std::collections::BTreeSet::new(),
        fill_sources: Vec::new(),
    };

    let report = engine.run(&mut strategy).unwrap();

    assert_eq!(report.fills.len(), 2);
    assert_eq!(report.fills_by_instrument[&btc].len(), 2);
    assert!(report.fills_by_instrument[&eth].is_empty());
    assert_eq!(
        strategy.fill_sources,
        vec![btc.to_string(), eth.to_string()],
        "orders sharing (market_index, visible_ts) must retain callback append order"
    );
    assert_eq!(
        report
            .ledger
            .position_for("shared-account", &btc)
            .quantity
            .raw(),
        2 * SCALE
    );
    report.verify().unwrap();
}

#[test]
fn shared_session_uses_ordered_fallback_for_unaligned_visible_timestamps() {
    let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let eth = InstrumentId::parse("ETHUSDT.KRAKEN").unwrap();
    let btc_bars = [
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
        Bar::new(
            4,
            100 * SCALE,
            101 * SCALE,
            99 * SCALE,
            100 * SCALE,
            100 * SCALE,
        ),
        Bar::new(
            5,
            100 * SCALE,
            101 * SCALE,
            99 * SCALE,
            100 * SCALE,
            100 * SCALE,
        ),
    ];
    let eth_bars = [
        Bar::new(
            1,
            100 * SCALE,
            101 * SCALE,
            99 * SCALE,
            100 * SCALE,
            100 * SCALE,
        ),
        Bar::new(
            3,
            100 * SCALE,
            101 * SCALE,
            99 * SCALE,
            100 * SCALE,
            100 * SCALE,
        ),
        Bar::new(
            4,
            100 * SCALE,
            101 * SCALE,
            99 * SCALE,
            100 * SCALE,
            100 * SCALE,
        ),
        Bar::new(
            5,
            100 * SCALE,
            101 * SCALE,
            99 * SCALE,
            100 * SCALE,
            100 * SCALE,
        ),
    ];
    let engine = PortfolioBarEngine::new(
        PortfolioBarSessionConfig {
            account_id: "shared-account".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_raw(1_000 * SCALE),
            risk: RiskGate::conservative_default(),
            funding_bps: BTreeMap::new(),
            funding_events: Vec::new(),
        },
        vec![
            market(btc.clone(), 0, &btc_bars, "BTC"),
            market(eth.clone(), 1, &eth_bars, "ETH"),
        ],
    )
    .unwrap();
    let mut strategy = SubmitToSharedTargetFromEachMarket {
        target: btc.clone(),
        submit_at_ts: 4,
        submitted_by: std::collections::BTreeSet::new(),
        fill_sources: Vec::new(),
    };

    let report = engine.run(&mut strategy).unwrap();

    assert_eq!(report.fills_by_instrument[&btc].len(), 2);
    assert!(report.fills_by_instrument[&eth].is_empty());
    assert_eq!(
        strategy.fill_sources,
        vec![btc.to_string(), eth.to_string()],
        "unaligned visible timestamps must form distinct ordered batches"
    );
    assert_eq!(
        report
            .ledger
            .position_for("shared-account", &btc)
            .quantity
            .raw(),
        2 * SCALE
    );
    report.verify().unwrap();
}

#[test]
fn shared_session_market_priority_is_deterministic_and_controls_funding_competition() {
    let first = run_with_priorities(0, 1);
    let repeated = run_with_priorities(0, 1);
    let reversed = run_with_priorities(1, 0);
    assert_eq!(first.result_hash(), repeated.result_hash());
    assert_eq!(
        reversed.fills_by_instrument[&InstrumentId::parse("ETHUSDT.KRAKEN").unwrap()].len(),
        1
    );
    assert_ne!(first.result_hash(), reversed.result_hash());
}

#[test]
fn explicit_zero_funding_rate_is_valid_for_a_spot_market() {
    let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let eth = InstrumentId::parse("ETHUSDT.KRAKEN").unwrap();
    let bars = [Bar::new(
        1,
        100 * SCALE,
        101 * SCALE,
        99 * SCALE,
        100 * SCALE,
        100 * SCALE,
    )];
    let engine = PortfolioBarEngine::new(
        PortfolioBarSessionConfig {
            account_id: "shared-account".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_raw(1_000 * SCALE),
            risk: RiskGate::conservative_default(),
            funding_bps: [(btc.clone(), 0)].into_iter().collect(),
            funding_events: Vec::new(),
        },
        vec![market(btc, 0, &bars, "BTC"), market(eth, 1, &bars, "ETH")],
    )
    .unwrap();
    let mut strategy = SubmitBoth {
        first: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        second: InstrumentId::parse("ETHUSDT.KRAKEN").unwrap(),
        emitted: false,
        fills: Vec::new(),
    };
    let report = engine.run(&mut strategy).unwrap();
    assert!(report.funding_by_instrument_raw.is_empty());
}

struct BuyDerivativeOnce {
    instrument: InstrumentId,
    submitted: bool,
    observed_cash: Vec<i128>,
}

impl BarStrategy for BuyDerivativeOnce {
    fn update_account_state(
        &mut self,
        cash: &BTreeMap<String, i128>,
        _available_margin_raw: Option<i128>,
    ) {
        self.observed_cash
            .push(cash.get("USDT").copied().unwrap_or_default());
    }

    fn on_bar_orders(
        &mut self,
        _history: &[Bar],
        _instrument: &InstrumentId,
        _ts: u64,
        _position: i128,
    ) -> Vec<Order> {
        if self.submitted {
            return Vec::new();
        }
        self.submitted = true;
        vec![Order {
            client_id: 91,
            instrument: self.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_raw(SCALE),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "shared-account".into(),
            trace: None,
            policy: None,
        }]
    }
}

#[test]
fn derivative_funding_is_applied_to_shared_ledger_before_later_callbacks_and_replays() {
    const EIGHT_HOURS_MS: u64 = 8 * 60 * 60 * 1_000;
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let bars = [
        Bar::new(1, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, SCALE),
        Bar::new(2, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, SCALE),
        Bar::new(
            EIGHT_HOURS_MS + 1,
            100 * SCALE,
            101 * SCALE,
            99 * SCALE,
            100 * SCALE,
            SCALE,
        ),
    ];
    let mut derivative = market(instrument.clone(), 0, &bars, "BTC");
    derivative.spec.product = TradingProduct::Perpetual;
    derivative.spec.linear = true;
    derivative.spec.max_leverage = 10;
    derivative.spec.maintenance_margin_bps = 500;
    let second_instrument = InstrumentId::parse("ETHUSDT.KRAKEN").unwrap();
    let second_bars = [
        Bar::new(1, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, SCALE),
        Bar::new(2, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, SCALE),
        Bar::new(
            EIGHT_HOURS_MS + 1,
            100 * SCALE,
            101 * SCALE,
            99 * SCALE,
            100 * SCALE,
            SCALE,
        ),
    ];
    let engine = PortfolioBarEngine::new(
        PortfolioBarSessionConfig {
            account_id: "shared-account".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_raw(1_000 * SCALE),
            risk: RiskGate::conservative_default(),
            funding_bps: BTreeMap::from([(instrument.clone(), 100)]),
            funding_events: Vec::new(),
        },
        vec![
            derivative,
            market(second_instrument.clone(), 1, &second_bars, "ETH"),
        ],
    )
    .unwrap();
    let mut strategy = BuyDerivativeOnce {
        instrument: instrument.clone(),
        submitted: false,
        observed_cash: Vec::new(),
    };
    let report = engine.run(&mut strategy).unwrap();
    assert_eq!(report.fills.len(), 1);
    assert_eq!(
        report.ledger.cash_for("shared-account", "USDT"),
        999 * SCALE
    );
    assert_eq!(report.funding_by_instrument_raw[&instrument], -SCALE);
    assert!(strategy.observed_cash.contains(&(999 * SCALE)));
    assert_eq!(
        report.funding_events_by_instrument[&instrument],
        vec![(EIGHT_HOURS_MS, -SCALE)]
    );
    assert!(report.event_log.events().iter().any(|event| {
        event.ts == EIGHT_HOURS_MS
            && matches!(
                &event.kind,
                EventKind::LedgerApplied { entry }
                    if entry.kind == qx_core::LedgerEntryKind::Funding
                        && entry.amount.raw() == -SCALE
                        && entry.instrument.as_ref() == Some(&instrument)
                        && entry.quantity.raw() == SCALE
                        && entry.price.map(|price| price.raw()) == Some(100 * SCALE)
            )
    }));
    report.verify().unwrap();

    let mut derivative = market(instrument.clone(), 0, &bars, "BTC");
    derivative.spec.product = TradingProduct::Perpetual;
    derivative.spec.linear = true;
    derivative.spec.max_leverage = 10;
    derivative.spec.maintenance_margin_bps = 500;
    let negative_engine = PortfolioBarEngine::new(
        PortfolioBarSessionConfig {
            account_id: "shared-account".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_raw(1_000 * SCALE),
            risk: RiskGate::conservative_default(),
            funding_bps: BTreeMap::from([(instrument.clone(), -100)]),
            funding_events: Vec::new(),
        },
        vec![
            derivative,
            market(second_instrument, 1, &second_bars, "ETH"),
        ],
    )
    .unwrap();
    let mut negative_strategy = BuyDerivativeOnce {
        instrument: instrument.clone(),
        submitted: false,
        observed_cash: Vec::new(),
    };
    let negative_report = negative_engine.run(&mut negative_strategy).unwrap();
    assert_eq!(
        negative_report.funding_by_instrument_raw[&instrument],
        SCALE
    );
    assert_eq!(
        negative_report.ledger.cash_for("shared-account", "USDT"),
        1_001 * SCALE
    );
    negative_report.verify().unwrap();
}

#[test]
fn historical_funding_events_apply_each_signed_rate_at_its_own_settlement() {
    let first_settlement = qx_core::FUNDING_INTERVAL_MS;
    let second_settlement = 2 * qx_core::FUNDING_INTERVAL_MS;
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let bars = [
        Bar::new(1, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, SCALE),
        Bar::new(2, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, SCALE),
        Bar::new(
            first_settlement,
            100 * SCALE,
            101 * SCALE,
            99 * SCALE,
            100 * SCALE,
            SCALE,
        ),
        Bar::new(
            second_settlement + 1,
            100 * SCALE,
            101 * SCALE,
            99 * SCALE,
            100 * SCALE,
            SCALE,
        ),
    ];
    let mut derivative = market(instrument.clone(), 0, &bars, "BTC");
    derivative.spec.product = TradingProduct::Perpetual;
    derivative.spec.linear = true;
    derivative.spec.max_leverage = 10;
    derivative.spec.maintenance_margin_bps = 500;
    let second_instrument = InstrumentId::parse("ETHUSDT.KRAKEN").unwrap();
    let second_bars = bars;
    let engine = PortfolioBarEngine::new(
        PortfolioBarSessionConfig {
            account_id: "shared-account".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_raw(1_000 * SCALE),
            risk: RiskGate::conservative_default(),
            funding_bps: BTreeMap::new(),
            funding_events: vec![
                qx_core::FundingRateEvent {
                    instrument: instrument.clone(),
                    settlement_ts: first_settlement,
                    rate_bps: 100,
                },
                qx_core::FundingRateEvent {
                    instrument: instrument.clone(),
                    settlement_ts: second_settlement,
                    rate_bps: -50,
                },
            ],
        },
        vec![
            derivative,
            market(second_instrument, 1, &second_bars, "ETH"),
        ],
    )
    .unwrap();
    let mut strategy = BuyDerivativeOnce {
        instrument: instrument.clone(),
        submitted: false,
        observed_cash: Vec::new(),
    };
    let report = engine.run(&mut strategy).unwrap();
    assert_eq!(
        report.funding_events_by_instrument[&instrument],
        vec![(first_settlement, -SCALE), (second_settlement, SCALE / 2)]
    );
    assert_eq!(report.funding_by_instrument_raw[&instrument], -SCALE / 2);
    assert!(strategy.observed_cash.contains(&(999 * SCALE)));
    assert!(strategy.observed_cash.contains(&(999 * SCALE + SCALE / 2)));
    report.verify().unwrap();
}

#[test]
fn historical_funding_events_reject_nonzero_spot_rates_before_execution() {
    let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let eth = InstrumentId::parse("ETHUSDT.KRAKEN").unwrap();
    let bars = [Bar::new(
        1,
        100 * SCALE,
        101 * SCALE,
        99 * SCALE,
        100 * SCALE,
        SCALE,
    )];
    let engine = PortfolioBarEngine::new(
        PortfolioBarSessionConfig {
            account_id: "shared-account".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_raw(1_000 * SCALE),
            risk: RiskGate::conservative_default(),
            funding_bps: BTreeMap::new(),
            funding_events: vec![qx_core::FundingRateEvent {
                instrument: btc.clone(),
                settlement_ts: qx_core::FUNDING_INTERVAL_MS,
                rate_bps: 1,
            }],
        },
        vec![
            market(btc.clone(), 0, &bars, "BTC"),
            market(eth, 1, &bars, "ETH"),
        ],
    )
    .unwrap();
    let mut strategy = SubmitBoth {
        first: btc,
        second: InstrumentId::parse("ETHUSDT.KRAKEN").unwrap(),
        emitted: false,
        fills: Vec::new(),
    };
    let error = match engine.run(&mut strategy) {
        Ok(_) => panic!("nonzero funding cannot be posted to a spot instrument"),
        Err(error) => error,
    };
    assert!(matches!(error, qx_core::QxError::BusinessViolation(_)));
}
