use qx_core::{InstrumentId, Price, Quantity};
use qx_xingban::{BookLevel, DepthFrame, OrderBookSnapshot};

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
