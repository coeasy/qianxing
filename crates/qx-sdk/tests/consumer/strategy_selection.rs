use super::*;
use qx_sdk::{
    MarketEvent, Strategy, StrategyArtifactIdentity, StrategyContext, StrategyDecision,
    StrategyOrderIntent, STRATEGY_API_VERSION,
};

struct BuyEachInstrument(std::collections::BTreeSet<String>);

impl Strategy for BuyEachInstrument {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let instrument = event.instrument().cloned().ok_or("instrument missing")?;
        let intents = if self.0.insert(instrument.to_string()) {
            vec![StrategyOrderIntent {
                intent_id: self.0.len() as u64,
                instrument,
                side: Side::Buy,
                qty: Quantity::from_raw(1_000_000_000),
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,
            }]
        } else {
            Vec::new()
        };
        Ok(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("{}:{}", context.strategy_id, event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 10_000,
            priority: 0,
            expires_at: 0,
            intents,
        })
    }
}

#[test]
fn rust_sdk_composes_multiple_external_strategies_in_one_shared_portfolio() {
    let output_dir = scratch("sdk-callback-portfolio");
    let mut backtest = spec("sdk-callback-portfolio", &output_dir);
    backtest.strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "custom-primary");
    backtest.portfolio = Some(PortfolioBacktestConfig {
        primary_priority: 0,
        primary_reserved_capital_raw: backtest.initial_cash_raw / 2,
        additional_strategies: vec![PortfolioStrategySpec {
            priority: 1,
            reserved_capital_raw: backtest.initial_cash_raw / 2,
            strategy: BuiltinStrategySpec::new(BuiltinStrategyKind::Momentum, "custom-secondary"),
        }],
    });
    let client = Client::new(CallerCapability::Research, "sdk-callback-portfolio");
    let result = client
        .run_backtest_with_strategy_slots(
            &backtest,
            vec![
                (
                    StrategyArtifactIdentity {
                        strategy_id: "custom-primary".into(),
                        strategy_version: "primary-v1".into(),
                        source_sha256: "b".repeat(64),
                    },
                    Box::new(BuyEachInstrument(Default::default())) as Box<dyn Strategy>,
                ),
                (
                    StrategyArtifactIdentity {
                        strategy_id: "custom-secondary".into(),
                        strategy_version: "secondary-v1".into(),
                        source_sha256: "c".repeat(64),
                    },
                    Box::new(BuyEachInstrument(Default::default())) as Box<dyn Strategy>,
                ),
            ],
        )
        .expect("Rust facade routes custom slots through the shared account engine");
    assert!(result.fills >= 2);
    let fills = std::fs::read_to_string(&result.artifacts.fills).expect("attributed fills");
    assert!(fills.contains("custom-primary"));
    assert!(fills.contains("custom-secondary"));
    assert!(
        client
            .verify_run(&result)
            .expect("shared replay verifier")
            .verified
    );
}

#[test]
fn independent_rust_sdk_consumer_runs_a_custom_strategy_on_both_pair_legs() {
    let output_dir = scratch("custom-multi-leg-strategy");
    let root = super::repo_root().join("deploy");
    let spec = MultiLegBacktestSpec::new(
        "sdk-custom-multi-leg",
        root.join("qianxing.bar-frame.pairs-primary.example.json")
            .to_string_lossy(),
        root.join("qianxing.bar-frame.pairs-reference.example.json")
            .to_string_lossy(),
        "USDT",
        1_000_000 * 1_000_000_000,
        1_000_000 * 1_000_000_000,
        7,
        output_dir.to_string_lossy(),
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "sdk-custom-pair"),
    );
    let identity = StrategyArtifactIdentity {
        strategy_id: "sdk-custom-pair".into(),
        strategy_version: "consumer-v1".into(),
        source_sha256: "a".repeat(64),
    };
    let client = Client::new(CallerCapability::Research, "sdk-custom-multi-leg");
    let result = client
        .run_multi_leg_backtest_with_strategy(
            &spec,
            &identity,
            BuyEachInstrument(Default::default()),
        )
        .expect("public SDK routes custom pair signals through Rust shared engines");
    assert!(result.primary_fills > 0 && result.reference_fills > 0);
    let mut handle = client
        .start_multi_leg_backtest_with_strategy(
            MultiLegBacktestSpec {
                run_id: "sdk-custom-multi-leg-handle".into(),
                output_dir: output_dir.join("handle").to_string_lossy().into_owned(),
                ..spec
            },
            identity,
            BuyEachInstrument(Default::default()),
        )
        .expect("public SDK starts cancellable custom pair run");
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    let result = handle
        .try_take_result()
        .expect("run result")
        .expect("success");
    assert!(result.primary_fills > 0 && result.reference_fills > 0);
}

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

#[test]
fn rust_sdk_runs_caller_strategy_factories_across_multi_leg_parameter_candidates() {
    let output_dir = scratch("custom-multi-leg-experiment");
    let root = super::repo_root().join("deploy");
    let mut strategy =
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "sdk-custom-pair-grid");
    strategy.quantity_raw = 1_000_000_000;
    let base = MultiLegBacktestSpec::new(
        "unused-custom-pair-grid",
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
