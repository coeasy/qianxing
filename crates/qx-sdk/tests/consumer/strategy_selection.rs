use super::*;

#[test]
fn the_rust_sdk_facade_quick_selects_multiple_single_leg_strategy_kinds() {
    let output_dir = scratch("strategy-selection");
    let client = Client::new(CallerCapability::Research, "sdk-strategy-selection");
    let kinds = vec![
        BuiltinStrategyKind::SmaCross,
        BuiltinStrategyKind::EmaCross,
        BuiltinStrategyKind::Macd,
        BuiltinStrategyKind::Rsi,
        BuiltinStrategyKind::Bollinger,
        BuiltinStrategyKind::DonchianBreakout,
        BuiltinStrategyKind::Momentum,
        BuiltinStrategyKind::MeanReversion,
        BuiltinStrategyKind::Grid,
        BuiltinStrategyKind::AtrTrend,
    ];
    let result = client
        .run_experiment(&RunExperimentSpec {
            schema_version: 1,
            experiment_id: "sdk-classic-selection".into(),
            base: spec("unused-selection-base", &output_dir),
            parameter_space: Vec::new(),
            strategy_kinds: kinds,
            split_plan: None,
            baselines: Vec::new(),
        })
        .expect("Rust SDK should route strategy selection through qx-app");
    assert_eq!(result.total_candidates, 10);
    assert_eq!(result.succeeded_candidates, 10);
    assert_eq!(result.comparison.expect("shared ranking").runs.len(), 10);
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| candidate.strategy_kind.as_deref().unwrap())
            .collect::<Vec<_>>(),
        [
            "atr_trend",
            "bollinger",
            "donchian_breakout",
            "ema_cross",
            "grid",
            "macd",
            "mean_reversion",
            "momentum",
            "rsi",
            "sma_cross",
        ]
    );
}

#[test]
fn the_rust_sdk_facade_selects_classic_strategies_over_order_book_data() {
    let output_dir = scratch("depth-strategy-selection");
    let root = super::repo_root();
    let depth_path = root
        .join("deploy")
        .join("qianxing.depth-frame.example.json");
    let mut base_strategy =
        BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "sdk-depth-sma");
    base_strategy.fast_window = 2;
    base_strategy.slow_window = 3;
    let kinds = vec![
        BuiltinStrategyKind::SmaCross,
        BuiltinStrategyKind::EmaCross,
        BuiltinStrategyKind::Macd,
        BuiltinStrategyKind::Rsi,
        BuiltinStrategyKind::Bollinger,
        BuiltinStrategyKind::DonchianBreakout,
        BuiltinStrategyKind::Momentum,
        BuiltinStrategyKind::MeanReversion,
        BuiltinStrategyKind::Grid,
        BuiltinStrategyKind::AtrTrend,
    ];
    let client = Client::new(CallerCapability::Research, "sdk-depth-classic-selection");
    let result = client
        .run_depth_experiment(&RunDepthExperimentSpec {
            schema_version: 1,
            experiment_id: "sdk-depth-classic-selection".into(),
            base: DepthBacktestSpec {
                schema_version: 1,
                run_id: "unused-sdk-depth-selection".into(),
                depth_path: depth_path.to_string_lossy().into_owned(),
                tier: "l2".into(),
                settlement_currency: "USDT".into(),
                initial_cash_raw: 100_000 * 1_000_000_000,
                fee_bps: 0,
                latency_snapshots: 0,
                queue_position_bps: 0,
                market_impact_bps: 0,
                output_dir: output_dir.to_string_lossy().into_owned(),
                strategy: base_strategy,
                market_spec: None,
                order_policy: None,
                market_making: None,
                account_id: None,
                portfolio: None,
            },
            parameter_space: Vec::new(),
            strategy_kinds: kinds,
        })
        .expect("Rust SDK should run the shared OrderBook strategy experiment");
    assert_eq!(result.total_candidates, 10);
    assert_eq!(result.succeeded_candidates, 10);
    assert!(result.candidates.iter().all(|candidate| {
        candidate
            .outcome
            .as_ref()
            .is_some_and(|outcome| !outcome.result_hash.is_empty())
    }));
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| candidate.strategy_kind.as_deref().unwrap())
            .collect::<Vec<_>>(),
        [
            "atr_trend",
            "bollinger",
            "donchian_breakout",
            "ema_cross",
            "grid",
            "macd",
            "mean_reversion",
            "momentum",
            "rsi",
            "sma_cross",
        ]
    );
}

#[test]
fn the_rust_sdk_facade_selects_multi_leg_arbitrage_strategies() {
    let output_dir = scratch("multi-leg-strategy-selection");
    let root = super::repo_root().join("deploy");
    let mut strategy = BuiltinStrategySpec::new(
        BuiltinStrategyKind::PairsArbitrage,
        "sdk-multi-leg-selection",
    );
    strategy.quantity_raw = 1_000_000_000;
    let base = MultiLegBacktestSpec::new(
        "unused-sdk-multi-leg-selection",
        root.join("qianxing.bar-frame.pairs-primary.example.json")
            .to_string_lossy(),
        root.join("qianxing.bar-frame.pairs-reference.example.json")
            .to_string_lossy(),
        "USDT",
        1_000_000 * 1_000_000_000,
        1_000_000 * 1_000_000_000,
        7,
        output_dir.to_string_lossy(),
        strategy,
    );
    let client = Client::new(CallerCapability::Research, "sdk-multi-leg-selection");
    let result = client
        .run_multi_leg_experiment(&RunMultiLegExperimentSpec {
            schema_version: 1,
            experiment_id: "sdk-multi-leg-selection".into(),
            base,
            parameter_space: Vec::new(),
            strategy_kinds: vec![
                BuiltinStrategyKind::PairsArbitrage,
                BuiltinStrategyKind::BasisArbitrage,
                BuiltinStrategyKind::CrossVenueArbitrage,
                BuiltinStrategyKind::SpotFuturesArbitrage,
            ],
        })
        .expect("Rust SDK should execute all supported two-leg strategy kinds");
    assert_eq!(result.total_candidates, 4);
    assert_eq!(result.succeeded_candidates, 4);
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| candidate.strategy_kind.as_deref().unwrap())
            .collect::<Vec<_>>(),
        [
            "basis_arbitrage",
            "cross_venue_arbitrage",
            "pairs_arbitrage",
            "spot_futures_arbitrage",
        ]
    );
}
