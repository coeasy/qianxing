#[test]
fn independent_consumer_discovers_the_rust_owned_builtin_strategy_catalog() {
    let catalog = qx_sdk::builtin_strategy_catalog();
    assert_eq!(catalog.len(), 17);
    assert_eq!(catalog[0].name, "sma_cross");
    assert_eq!(catalog[0].signal_knobs, ["fast_window", "slow_window"]);
    assert!(catalog
        .iter()
        .any(|strategy| { strategy.name == "pairs_arbitrage" && strategy.requires_reference_leg }));
}
