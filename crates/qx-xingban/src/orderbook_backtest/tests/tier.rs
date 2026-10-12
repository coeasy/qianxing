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
