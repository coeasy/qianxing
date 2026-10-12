use super::*;
use crate::{
    BookLevel, DataTier, OrderBookBacktestConfig, OrderBookBacktestEngine, OrderBookSnapshot,
};
use qx_core::{EventKind, InstrumentId, Money, OrderStatus, Price, Quantity, Side};
use qx_zhenlu::RiskGate;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Probe {
    id: String,
    emitted: bool,
    observed: Arc<Mutex<Vec<(String, u64, i128)>>>,
    fills: Arc<Mutex<Vec<(String, u64, u64)>>>,
    qty: i64,
}

impl OrderBookStrategy for Probe {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
    ) -> Result<Vec<Order>, String> {
        self.observed
            .lock()
            .unwrap()
            .push((self.id.clone(), snapshot.ts, position));
        if self.emitted {
            return Ok(Vec::new());
        }
        self.emitted = true;
        Ok(vec![Order {
            client_id: 7,
            instrument: snapshot.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(self.qty),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: String::new(),
            trace: None,
            policy: None,
        }])
    }

    fn on_fill(&mut self, order: &Order, fill: &Fill) -> Result<(), QxError> {
        self.fills
            .lock()
            .unwrap()
            .push((self.id.clone(), order.client_id, fill.order_id));
        Ok(())
    }
}

fn snapshot(ts: u64, ask: i64) -> OrderBookSnapshot {
    OrderBookSnapshot {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        ts,
        sequence: ts,
        bids: vec![BookLevel {
            price: Price::from_i64(ask - 2),
            qty: Quantity::from_i64(10),
        }],
        asks: vec![BookLevel {
            price: Price::from_i64(ask),
            qty: Quantity::from_i64(10),
        }],
    }
}

fn engine() -> OrderBookBacktestEngine {
    OrderBookBacktestEngine::new(OrderBookBacktestConfig {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(1_000),
        fee_bps: 0,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    })
}

#[test]
fn orderbook_portfolio_shares_ledger_and_routes_virtual_positions_and_fills() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let fills = Arc::new(Mutex::new(Vec::new()));
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut portfolio = PortfolioOrderBookStrategy::new(
        instrument,
        vec![
            OrderBookStrategySlot::new(
                0,
                "alpha",
                150 * qx_core::SCALE,
                Probe {
                    id: "alpha".into(),
                    observed: observed.clone(),
                    fills: fills.clone(),
                    qty: 1,
                    ..Probe::default()
                },
            ),
            OrderBookStrategySlot::new(
                1,
                "beta",
                150 * qx_core::SCALE,
                Probe {
                    id: "beta".into(),
                    observed: observed.clone(),
                    fills: fills.clone(),
                    qty: 1,
                    ..Probe::default()
                },
            ),
        ],
    )
    .unwrap();
    let report = engine()
        .run(&[snapshot(1, 100), snapshot(2, 102)], &mut portfolio)
        .unwrap();

    assert_eq!(report.fills.len(), 2);
    assert_ne!(report.fills[0].order_id, report.fills[1].order_id);
    assert_eq!(
        report
            .fills
            .iter()
            .filter_map(|fill| fill.strategy_id.as_deref())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["alpha", "beta"])
    );
    let observed = observed.lock().unwrap();
    assert!(observed.contains(&("alpha".into(), 2, qx_core::SCALE)));
    assert!(observed.contains(&("beta".into(), 2, qx_core::SCALE)));
    assert_eq!(
        *fills.lock().unwrap(),
        vec![("alpha".into(), 7, 7), ("beta".into(), 7, 7)]
    );
}

#[test]
fn orderbook_portfolio_rejects_strategy_order_over_its_cap() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut portfolio = PortfolioOrderBookStrategy::new(
        instrument,
        vec![OrderBookStrategySlot::new(
            0,
            "alpha",
            100 * qx_core::SCALE,
            Probe {
                id: "alpha".into(),
                qty: 2,
                ..Probe::default()
            },
        )],
    )
    .unwrap();
    let report = engine()
        .run(&[snapshot(1, 100), snapshot(2, 100)], &mut portfolio)
        .unwrap();
    assert!(report.fills.is_empty());
    assert!(report.event_log.events().iter().any(|event| matches!(
        &event.kind,
        EventKind::Rejected { reason, .. } if reason.contains("资金额度不足")
    )));
}
