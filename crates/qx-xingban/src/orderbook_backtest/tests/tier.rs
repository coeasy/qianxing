use super::*;

#[test]
fn order_book_backtest_rejects_bar_data_tier() {
    let config = OrderBookBacktestConfig {
        data_tier: DataTier::Bar,
        ..l2_config(0)
    };
    let error = OrderBookBacktestEngine::new(config)
        .run(&[snapshot(1, 100)], &mut BuyOnce { emitted: false })
        .err()
        .expect("Bar 档位不能用于盘口回测");
    assert!(format!("{error:?}").contains("不能声明为 Bar"));
}

#[test]
fn order_book_backtest_rejects_l2_snapshots_declared_as_l1() {
    let config = OrderBookBacktestConfig {
        data_tier: DataTier::L1,
        ..l2_config(0)
    };
    let mut l2_snapshot = snapshot(1, 100);
    l2_snapshot.bids.push(crate::BookLevel {
        price: qx_core::Price::from_i64(98),
        qty: qx_core::Quantity::from_i64(3),
    });
    let error = OrderBookBacktestEngine::new(config)
        .run(&[l2_snapshot], &mut BuyOnce { emitted: false })
        .err()
        .expect("多档输入不能声明为 L1");
    assert!(format!("{error:?}").contains("声明 L1 时每份快照必须恰有一档 bid/ask"));
}
