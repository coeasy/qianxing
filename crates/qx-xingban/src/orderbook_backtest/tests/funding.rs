use super::super::{
    DataTier, OrderBookBacktestConfig, OrderBookBacktestEngine, OrderBookExecutionModel,
    OrderBookSnapshot,
};
use crate::{BookLevel, OrderBookStrategy};
use qx_core::{InstrumentId, Money, Order, OrderStatus, Price, Quantity, Side};
use qx_zhenlu::RiskGate;

struct TwoPendingSpotBuys;

impl OrderBookStrategy for TwoPendingSpotBuys {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        Ok([1, 2]
            .into_iter()
            .map(|client_id| Order {
                client_id,
                instrument: snapshot.instrument.clone(),
                side: Side::Buy,
                qty: Quantity::from_i64(60),
                limit: None,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: None,
            })
            .collect())
    }
}

struct SpotBuyAfterPartialFill {
    calls: u8,
}

impl OrderBookStrategy for SpotBuyAfterPartialFill {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        self.calls += 1;
        let (client_id, qty) = match self.calls {
            1 => (1, 60),
            2 => (2, 40),
            _ => return Ok(Vec::new()),
        };
        Ok(vec![Order {
            client_id,
            instrument: snapshot.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(qty),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        }])
    }
}

struct OneMarketBuy {
    submitted: bool,
}

impl OrderBookStrategy for OneMarketBuy {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if std::mem::replace(&mut self.submitted, true) {
            return Ok(Vec::new());
        }
        Ok(vec![Order {
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
        }])
    }
}

#[test]
fn market_buy_price_gap_fails_closed_before_cash_can_be_overdrawn() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(150),
        fee_bps: 0,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let make_snapshot = |ts, ask| OrderBookSnapshot {
        instrument: instrument.clone(),
        ts,
        sequence: ts,
        bids: vec![BookLevel {
            price: Price::from_i64(ask - 1),
            qty: Quantity::from_i64(10),
        }],
        asks: vec![BookLevel {
            price: Price::from_i64(ask),
            qty: Quantity::from_i64(10),
        }],
    };
    let error = OrderBookBacktestEngine::new(config)
        .run(
            &[make_snapshot(1, 100), make_snapshot(2, 200)],
            &mut OneMarketBuy { submitted: false },
        )
        .err()
        .expect("高开导致成交金额超过账户现金时必须失败关闭");
    assert!(
        error.to_string().contains("现货成交后账户现金不足"),
        "unexpected error: {error}"
    );
}

#[test]
fn pending_spot_buy_orders_reserve_shared_cash_before_acceptance() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let snapshot = OrderBookSnapshot {
        instrument: instrument.clone(),
        ts: 1,
        sequence: 1,
        bids: vec![BookLevel {
            price: Price::from_i64(99),
            qty: Quantity::from_i64(100),
        }],
        asks: vec![BookLevel {
            price: Price::from_i64(100),
            qty: Quantity::from_i64(100),
        }],
    };
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 1,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut delayed = OrderBookExecutionModel::new(1).unwrap();
    delayed.latency_snapshots = 100;
    let report = OrderBookBacktestEngine::new(config)
        .with_execution_model(delayed)
        .run(&[snapshot], &mut TwoPendingSpotBuys)
        .unwrap();

    assert!(report.fills.is_empty());
    assert_eq!(report.pending_orders, 1);
    assert_eq!(
        report.ledger.cash_for("main", "USDT"),
        Money::from_i64(10_000).raw()
    );
    let accepted = report
        .event_log
        .events()
        .iter()
        .filter(|event| matches!(&event.kind, qx_core::EventKind::Accepted { .. }))
        .count();
    let rejected = report
        .event_log
        .events()
        .iter()
        .filter(|event| matches!(&event.kind, qx_core::EventKind::Rejected { .. }))
        .count();
    assert_eq!(accepted, 1);
    assert_eq!(rejected, 1);
}

#[test]
fn partial_spot_fill_releases_only_the_filled_order_reservation() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let make_snapshot = |ts| OrderBookSnapshot {
        instrument: instrument.clone(),
        ts,
        sequence: ts,
        bids: vec![BookLevel {
            price: Price::from_i64(99),
            qty: Quantity::from_i64(100),
        }],
        asks: vec![BookLevel {
            price: Price::from_i64(100),
            qty: Quantity::from_i64(30),
        }],
    };
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 1,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = SpotBuyAfterPartialFill { calls: 0 };
    let report = OrderBookBacktestEngine::new(config)
        .run(
            &[make_snapshot(1), make_snapshot(2), make_snapshot(3)],
            &mut strategy,
        )
        .unwrap();

    assert_eq!(
        report.fills.iter().map(|fill| fill.qty.raw()).sum::<i128>(),
        Quantity::from_i64(60).raw()
    );
    assert_eq!(report.pending_orders, 0);
    assert!(report.ledger.cash_for("main", "USDT") >= 0);
    assert_eq!(
        report
            .event_log
            .events()
            .iter()
            .filter(|event| matches!(
                &event.kind,
                qx_core::EventKind::Rejected {
                    client_order_id: 2,
                    ..
                }
            ))
            .count(),
        1
    );
}
