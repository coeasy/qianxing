use super::*;
use crate::{NextBarOpenFillModel, NoMargin, ZeroLatency};
use qx_core::{Order, OrderStatus, Quantity, Side, TradingProduct};

struct SubmitBoth {
    first: InstrumentId,
    second: InstrumentId,
    emitted: bool,
    fills: Vec<InstrumentId>,
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
