use super::*;

#[test]
fn depth_bar_view_lets_bar_only_strategies_trade_on_order_book_snapshots() {
    let mut strategy =
        DepthBarStrategy::new(NativeBuy, book_context(), snapshot(1, 100).instrument);
    let report = OrderBookBacktestEngine::new(l2_config(0))
        .run(&[snapshot(1, 100), snapshot(2, 101)], &mut strategy)
        .unwrap();
    assert_eq!(report.fills.len(), 1);
    assert_eq!(report.pending_orders, 1);
    assert_eq!(report.positions, vec![0, Quantity::from_i64(1).raw()]);
}
