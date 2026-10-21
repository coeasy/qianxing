use super::*;
#[path = "strategy_selection/multi_leg.rs"]
mod multi_leg;
#[path = "strategy_selection/multi_market.rs"]
mod multi_market;
use qx_sdk::{
    MarketEvent, RunContext, Strategy, StrategyArtifactIdentity, StrategyContext, StrategyDecision,
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
fn independent_rust_sdk_consumer_runs_a_custom_strategy_on_both_pair_legs() {
    let output_dir = scratch("custom-multi-leg-strategy");
    let root = super::repo_root().join("deploy");
    let spec = MultiLegBacktestSpec::new(
        qx_sdk::MultiLegDatasetInput {
            run_id: ("sdk-custom-multi-leg").into(),
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
                fee_bps: Some(0),
                latency_snapshots: 0,
                queue_position_bps: 0,
                queue_position_model: Default::default(),
                market_impact_bps: 0,
                output_dir: output_dir.to_string_lossy().into_owned(),
                strategy: base_strategy,
                market_spec: None,
                order_policy: None,
                market_making: None,
                account_id: None,
                portfolio: None,
                manifest_run_id: None,
                execution_cost_rules: None,
                execution_cost_source: None,
                risk_rules: None,
                allow_short: None,
                declared_ashare_rules: None,
                declared_fill_model: None,
                declared_bar_volume_participation_bps: None,
                quality_manifest_path: None,
                runtime_config_path: None,
                market_spec_path: None,
                market_spec_source: None,
                account_base_source: None,
                signal_source: None,
                signal_declared_unused: None,
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
struct BuyDepthQuantity {
    quantity_raw: i128,
    sent: bool,
}

impl Strategy for BuyDepthQuantity {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let intents = if self.sent {
            Vec::new()
        } else {
            self.sent = true;
            vec![StrategyOrderIntent {
                intent_id: 1,
                instrument: event.instrument().cloned().ok_or("instrument missing")?,
                side: Side::Buy,
                qty: Quantity::from_raw(self.quantity_raw),
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,
            }]
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
fn rust_sdk_depth_experiment_factory_runs_parameterized_custom_strategies() {
    let output_dir = scratch("sdk-custom-depth-grid");
    std::fs::create_dir_all(&output_dir).expect("create depth grid fixture directory");
    let snapshots = (0..48)
        .map(|index| {
            let phase = index % 20;
            let mid = 100_i128
                + if phase < 10 {
                    phase * 2
                } else {
                    (20 - phase) * 2
                };
            serde_json::json!({
                "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                "ts": 1_000 + index as u64 * 1_000,
                "sequence": index + 1,
                "bids": [{"price": (mid - 1) * 1_000_000_000_i128, "qty": 100_000_000_000_i128}],
                "asks": [{"price": (mid + 1) * 1_000_000_000_i128, "qty": 100_000_000_000_i128}],
            })
        })
        .collect::<Vec<_>>();
    let depth_path = output_dir.join("depth.json");
    std::fs::write(
        &depth_path,
        serde_json::json!({
            "schema_version": 1,
            "source": "qx-sdk-depth-grid-test",
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "snapshots": snapshots,
        })
        .to_string(),
    )
    .expect("write depth grid fixture");
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "sdk-depth-grid");
    strategy.fast_window = 2;
    strategy.slow_window = 3;
    strategy.quantity_raw = 1_000_000_000;
    let spec = RunDepthExperimentSpec {
        schema_version: 1,
        experiment_id: "sdk-custom-depth-grid".into(),
        base: DepthBacktestSpec {
            schema_version: 1,
            run_id: "unused-sdk-depth-grid".into(),
            depth_path: depth_path.to_string_lossy().into_owned(),
            tier: "l1".into(),
            settlement_currency: "USDT".into(),
            initial_cash_raw: 100_000 * 1_000_000_000,
            fee_bps: Some(0),
            latency_snapshots: 0,
            queue_position_bps: 0,
            queue_position_model: Default::default(),
            market_impact_bps: 0,
            output_dir: output_dir.to_string_lossy().into_owned(),
            strategy,
            market_spec: None,
            order_policy: None,
            market_making: None,
            account_id: None,
            portfolio: None,
            manifest_run_id: None,
            execution_cost_rules: None,
            execution_cost_source: None,
            risk_rules: None,
            allow_short: None,
            declared_ashare_rules: None,
            declared_fill_model: None,
            declared_bar_volume_participation_bps: None,
            quality_manifest_path: None,
            runtime_config_path: None,
            market_spec_path: None,
            market_spec_source: None,
            account_base_source: None,
            signal_source: None,
            signal_declared_unused: None,
        },
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: vec![1_000_000_000, 2_000_000_000],
        }],
        strategy_kinds: Vec::new(),
    };
    let identity = StrategyArtifactIdentity {
        strategy_id: "sdk-depth-grid".into(),
        strategy_version: "consumer-v1".into(),
        source_sha256: "d".repeat(64),
    };
    let mut seen = Vec::new();
    let mut factory = |candidate: &DepthBacktestSpec| {
        seen.push(candidate.strategy.quantity_raw);
        Ok(Box::new(BuyDepthQuantity {
            quantity_raw: candidate.strategy.quantity_raw,
            sent: false,
        }) as Box<dyn Strategy>)
    };
    let context = RunContext::new(CallerCapability::Research, "sdk-custom-depth-grid");
    let result = spec
        .run_with_strategy_factory(&identity, &mut factory, &context)
        .expect("public Rust SDK depth strategy factory entrypoint");
    assert_eq!(seen, [1_000_000_000, 2_000_000_000]);
    assert_eq!(result.succeeded_candidates, 2);
    assert!(result.candidates.iter().all(|candidate| candidate
        .outcome
        .as_ref()
        .is_some_and(|outcome| outcome.fills > 0)));
    let mut handle_spec = spec;
    handle_spec.experiment_id = "sdk-custom-depth-grid-handle".into();
    let mut handle = handle_spec
        .start_with_strategy_factory(
            identity,
            |candidate| {
                Ok(Box::new(BuyDepthQuantity {
                    quantity_raw: candidate.strategy.quantity_raw,
                    sent: false,
                }) as Box<dyn Strategy>)
            },
            RunContext::new(CallerCapability::Research, "sdk-custom-depth-grid-handle"),
        )
        .expect("public Rust SDK cancellable depth strategy factory entrypoint");
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
