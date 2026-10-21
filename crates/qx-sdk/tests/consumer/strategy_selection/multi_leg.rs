use super::*;

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
        qx_sdk::MultiLegDatasetInput {
            run_id: ("unused-sdk-multi-leg-selection").into(),
            primary_bars_path: (root
                .join("qianxing.bar-frame.pairs-primary.example.json")
                .to_string_lossy())
            .into(),
            reference_bars_path: (root
                .join("qianxing.bar-frame.pairs-reference.example.json")
                .to_string_lossy())
            .into(),
        },
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

#[test]
fn rust_sdk_runs_caller_strategy_factories_across_multi_leg_parameter_candidates() {
    let output_dir = scratch("custom-multi-leg-experiment");
    let root = super::repo_root().join("deploy");
    let mut strategy =
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "sdk-custom-pair-grid");
    strategy.quantity_raw = 1_000_000_000;
    let base = MultiLegBacktestSpec::new(
        qx_sdk::MultiLegDatasetInput {
            run_id: ("unused-custom-pair-grid").into(),
            primary_bars_path: (root
                .join("qianxing.bar-frame.pairs-primary.example.json")
                .to_string_lossy())
            .into(),
            reference_bars_path: (root
                .join("qianxing.bar-frame.pairs-reference.example.json")
                .to_string_lossy())
            .into(),
        },
        "USDT",
        1_000_000 * 1_000_000_000,
        1_000_000 * 1_000_000_000,
        7,
        output_dir.to_string_lossy(),
        strategy,
    );
    let spec = RunMultiLegExperimentSpec {
        schema_version: 1,
        experiment_id: "sdk-custom-pair-grid".into(),
        base,
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: vec![500_000_000, 1_000_000_000],
        }],
        strategy_kinds: Vec::new(),
    };
    let identity = StrategyArtifactIdentity {
        strategy_id: "sdk-custom-pair-grid".into(),
        strategy_version: "consumer-v1".into(),
        source_sha256: "c".repeat(64),
    };
    let client = Client::new(CallerCapability::Research, "sdk-custom-pair-grid");
    let mut factory = |candidate: &MultiLegBacktestSpec| {
        assert!(candidate.run_id.starts_with("sdk-custom-pair-grid-"));
        Ok(Box::new(BuyEachInstrument(Default::default())) as Box<dyn Strategy>)
    };
    let result = client
        .run_multi_leg_experiment_with_strategy_factory(&spec, &identity, &mut factory)
        .expect("public Rust SDK strategy factory entrypoint");
    assert_eq!(result.total_candidates, 2);
    assert_eq!(result.succeeded_candidates, 2);
    assert!(result.candidates.iter().all(|candidate| {
        let outcome = candidate.outcome.as_ref().expect("candidate outcome");
        outcome.primary_fills > 0 && outcome.reference_fills > 0
    }));

    let mut handle = client
        .start_multi_leg_experiment_with_strategy_factory(spec, identity, |candidate| {
            assert!(candidate.run_id.starts_with("sdk-custom-pair-grid-"));
            Ok(Box::new(BuyEachInstrument(Default::default())) as Box<dyn Strategy>)
        })
        .expect("public Rust SDK cancellable strategy factory entrypoint");
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    assert_eq!(
        handle
            .try_take_result()
            .unwrap()
            .unwrap()
            .succeeded_candidates,
        2
    );
}
