use qx_core::{InstrumentId, Money, Price, Quantity};
use qx_guanxing::{Bar, QuoteTick};
use qx_xingban::{
    BacktestConfig, BacktestEngine, BarStrategy, BookLevel, DataTier, OrderBookBacktestConfig,
    OrderBookBacktestEngine, OrderBookSnapshot, OrderBookStrategy, TickBacktestConfig,
    TickBacktestEngine, TickStrategy,
};
use qx_zhenlu::RiskGate;

struct NoopBar;
impl BarStrategy for NoopBar {}

struct NoopBook;
impl OrderBookStrategy for NoopBook {
    fn on_order_book(
        &mut self,
        _snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<qx_core::Order>, String> {
        Ok(Vec::new())
    }
}

struct NoopTick;
impl TickStrategy for NoopTick {
    fn on_tick(
        &mut self,
        _tick: &QuoteTick,
        _position: i128,
    ) -> Result<Vec<qx_core::Order>, String> {
        Ok(Vec::new())
    }
}

fn instrument() -> InstrumentId {
    InstrumentId::parse("BTCUSDT.BINANCE").unwrap()
}

fn snapshot(ts: u64) -> OrderBookSnapshot {
    OrderBookSnapshot {
        instrument: instrument(),
        ts,
        sequence: ts,
        bids: vec![BookLevel {
            price: Price::from_i64(99),
            qty: Quantity::from_i64(2),
        }],
        asks: vec![BookLevel {
            price: Price::from_i64(100),
            qty: Quantity::from_i64(2),
        }],
    }
}

#[test]
fn bar_engine_cancels_on_a_bar_boundary_without_a_partial_report() {
    let config = BacktestConfig {
        instrument: instrument(),
        instrument_spec: None,
        account_id: "cancel-test".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        multiplier: 1,
        fill: Box::new(qx_xingban::NextBarOpenFillModel),
        fee: Box::new(qx_core::MakerTakerFeeModel {
            maker_bp: 0,
            taker_bp: 0,
        }),
        data_tier: DataTier::Bar,
        latency: Box::new(qx_xingban::ZeroLatency),
        margin: Box::new(qx_xingban::NoMargin),
        seed: 1,
        risk: qx_zhenlu::RiskGate::conservative_default(),
        virtual_trading: Default::default(),
    };
    let bars = (1..=4)
        .map(|ts| Bar::new(ts, 100, 101, 99, 100, 10))
        .collect::<Vec<_>>();
    let mut strategy = NoopBar;
    let mut checks = 0;
    let result = BacktestEngine::new(config)
        .run_with_cancel(&bars, &mut strategy, || {
            checks += 1;
            checks == 3
        })
        .unwrap();
    assert!(result.is_none());
    assert_eq!(checks, 3);
}

#[test]
fn order_book_engine_cancels_during_matching_without_a_partial_report() {
    let config = OrderBookBacktestConfig {
        instrument: instrument(),
        account_id: "cancel-test".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 0,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let snapshots = [snapshot(1), snapshot(2), snapshot(3)];
    let mut strategy = NoopBook;
    let mut checks = 0;
    let result = OrderBookBacktestEngine::new(config)
        .run_with_cancel(&snapshots, &mut strategy, || {
            checks += 1;
            checks == 5
        })
        .unwrap();
    assert!(result.is_none());
    assert_eq!(checks, 5);
}

#[test]
fn tick_engine_propagates_cancellation_into_l1_matching() {
    let config = TickBacktestConfig {
        instrument: instrument(),
        account_id: "cancel-test".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 0,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
    };
    let ticks = [
        QuoteTick::new(
            1,
            Price::from_i64(99),
            Quantity::from_i64(2),
            Price::from_i64(100),
            Quantity::from_i64(2),
            1,
        ),
        QuoteTick::new(
            2,
            Price::from_i64(100),
            Quantity::from_i64(2),
            Price::from_i64(101),
            Quantity::from_i64(2),
            2,
        ),
    ];
    let mut strategy = NoopTick;
    let mut checks = 0;
    let result = TickBacktestEngine::new(config)
        .run_with_cancel(&ticks, &mut strategy, || {
            checks += 1;
            checks == 6
        })
        .unwrap();
    assert!(result.is_none());
    assert_eq!(checks, 6);
}
