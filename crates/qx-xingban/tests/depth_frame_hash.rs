use qx_core::{InstrumentId, Money, Price, Quantity};
use qx_xingban::{
    BookLevel, DataTier, DepthFrame, OrderBookBacktestConfig, OrderBookBacktestEngine,
    OrderBookSnapshot, OrderBookStrategy,
};
use qx_zhenlu::RiskGate;

struct NoopStrategy;

impl OrderBookStrategy for NoopStrategy {
    fn on_order_book(
        &mut self,
        _snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<qx_core::Order>, String> {
        Ok(Vec::new())
    }
}

#[test]
fn depth_frame_hash_includes_bid_ask_boundaries() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let level = |price| BookLevel {
        price: Price::from_i64(price),
        qty: Quantity::from_i64(1),
    };
    let frame = |bids, asks| DepthFrame {
        schema_version: DepthFrame::SUPPORTED_SCHEMA_VERSION,
        source: "fixture".into(),
        instrument: instrument.clone(),
        snapshots: vec![OrderBookSnapshot {
            instrument: instrument.clone(),
            ts: 1,
            sequence: 1,
            bids,
            asks,
        }],
    };
    let bid_ask = frame(vec![level(99)], vec![level(100)]);
    let ask_only = frame(vec![], vec![level(99), level(100)]);

    assert!(bid_ask.validate().is_ok());
    assert!(ask_only.validate().is_ok());
    assert_ne!(bid_ask.input_hash(), ask_only.input_hash());
}

#[test]
fn order_book_report_hash_includes_bid_ask_boundaries() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let level = |price| BookLevel {
        price: Price::from_i64(price),
        qty: Quantity::from_i64(1),
    };
    let book = |bids: Vec<BookLevel>, asks: Vec<BookLevel>| -> Vec<OrderBookSnapshot> {
        [1, 2]
            .map(|ts| OrderBookSnapshot {
                instrument: instrument.clone(),
                ts,
                sequence: ts,
                bids: bids.clone(),
                asks: asks.clone(),
            })
            .to_vec()
    };
    let report = |snapshots: Vec<OrderBookSnapshot>| {
        OrderBookBacktestEngine::new(OrderBookBacktestConfig {
            instrument: instrument.clone(),
            account_id: "main".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_i64(1_000),
            fee_bps: 0,
            instrument_spec: None,
            risk: RiskGate::conservative_default(),
            data_tier: DataTier::L2L3,
        })
        .run(&snapshots, &mut NoopStrategy)
        .unwrap()
    };

    let bid_ask = report(book(vec![level(99)], vec![level(100)]));
    let ask_only = report(book(vec![], vec![level(99), level(100)]));

    assert_ne!(bid_ask.input_data_hash, ask_only.input_data_hash);
}
