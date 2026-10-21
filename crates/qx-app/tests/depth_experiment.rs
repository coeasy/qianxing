mod common;

use common::{depth_spec as spec, scratch};
use qx_app::{
    run_depth_experiment, AppErrorCategory, CallerCapability, DepthBacktestSpec,
    ExperimentParameterSpace, RunContext, RunDepthExperimentSpec, RunStatus,
    StrategyArtifactIdentity,
};
use qx_core::{Quantity, Side, SCALE};
use qx_strategy::{MarketEvent, Strategy, StrategyContext, StrategyDecision, StrategyOrderIntent};
use std::path::Path;

#[test]
fn tick_parameter_experiment_is_deterministic_and_isolates_bad_candidates() {
    let root = scratch("tick-experiment");
    let spec = RunDepthExperimentSpec {
        schema_version: 1,
        experiment_id: "tick-grid".into(),
        base: spec("unused-tick-grid", &root, "l1", 1),
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: vec![0, SCALE],
        }],
        strategy_kinds: Vec::new(),
    };
    let context = RunContext::new(CallerCapability::Research, "tick-grid");
    let result = run_depth_experiment(&spec, &context).expect("Tick parameter grid");
    assert_eq!(result.total_candidates, 2);
    assert_eq!(result.completed_candidates, 2);
    assert_eq!(result.failed_candidates, 1);
    assert_eq!(result.succeeded_candidates, 1);
    assert!(result.candidates[0].error.is_some());
    assert!(result.candidates[1].outcome.is_some());
    assert!(Path::new(&result.artifact_path).is_file());
    assert_eq!(run_depth_experiment(&spec, &context).unwrap(), result);

    let mut changed = spec;
    changed.parameter_space[0].values = vec![SCALE, SCALE * 2];
    assert_eq!(
        run_depth_experiment(&changed, &context)
            .unwrap_err()
            .category(),
        AppErrorCategory::Conflict
    );
}

struct BuyOnFirstDepthEvent {
    quantity_raw: i128,
    sent: bool,
}

impl Strategy for BuyOnFirstDepthEvent {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let intent = if self.sent {
            Vec::new()
        } else {
            self.sent = true;
            vec![StrategyOrderIntent {
                intent_id: 1,
                instrument: event.instrument().cloned().expect("depth instrument"),
                side: Side::Buy,
                qty: Quantity::from_raw(self.quantity_raw),
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,
            }]
        };
        Ok(StrategyDecision {
            schema_version: qx_strategy::STRATEGY_API_VERSION,
            request_id: format!("{}:{}", context.strategy_id, event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 10_000,
            priority: 0,
            expires_at: 0,
            intents: intent,
        })
    }
}

#[test]
fn caller_strategy_factory_runs_fresh_tick_candidates_and_cancellable_handle() {
    let root = scratch("tick-custom-strategy-experiment");
    let spec = RunDepthExperimentSpec {
        schema_version: 1,
        experiment_id: "tick-custom-strategy-grid".into(),
        base: spec("unused-custom-tick-grid", &root, "l1", 1),
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: vec![SCALE, 2 * SCALE],
        }],
        strategy_kinds: Vec::new(),
    };
    let identity = StrategyArtifactIdentity {
        strategy_id: spec.base.strategy.strategy_id.clone(),
        strategy_version: "rust-depth-grid-v1".into(),
        source_sha256: "c".repeat(64),
    };
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut factory = {
        let seen = seen.clone();
        move |candidate: &DepthBacktestSpec| {
            let quantity_raw = candidate.strategy.quantity_raw;
            seen.lock().unwrap().push(quantity_raw);
            Ok(Box::new(BuyOnFirstDepthEvent {
                quantity_raw,
                sent: false,
            }) as Box<dyn Strategy>)
        }
    };
    let context = RunContext::new(CallerCapability::Research, "tick-custom-strategy-grid");
    let result = spec
        .run_with_strategy_factory(&identity, &mut factory, &context)
        .expect("custom Tick strategy candidates use the shared Rust depth engine");
    assert_eq!(*seen.lock().unwrap(), vec![SCALE, 2 * SCALE]);
    assert_eq!(result.succeeded_candidates, 2);
    assert!(result.candidates.iter().all(|candidate| candidate
        .outcome
        .as_ref()
        .is_some_and(|outcome| outcome.fills > 0)));

    let mut handle_spec = spec;
    handle_spec.experiment_id = "tick-custom-strategy-grid-handle".into();
    let mut handle = handle_spec
        .start_with_strategy_factory(
            identity,
            |candidate: &DepthBacktestSpec| {
                Ok(Box::new(BuyOnFirstDepthEvent {
                    quantity_raw: candidate.strategy.quantity_raw,
                    sent: false,
                }) as Box<dyn Strategy>)
            },
            RunContext::new(
                CallerCapability::Research,
                "tick-custom-strategy-grid-handle",
            ),
        )
        .expect("factory validates before worker starts");
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    let result = handle
        .try_take_result()
        .expect("completed depth experiment result")
        .expect("custom depth grid report");
    assert_eq!(result.succeeded_candidates, 2);
}

#[test]
fn order_book_parameter_experiment_returns_a_cancellable_run_handle() {
    let root = scratch("orderbook-experiment");
    let mut spec = RunDepthExperimentSpec {
        schema_version: 1,
        experiment_id: "book-grid-handle".into(),
        base: spec("unused-book-grid", &root, "l2", 2),
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: vec![SCALE],
        }],
        strategy_kinds: Vec::new(),
    };
    spec.base.run_id = "unused-book-grid".into();
    let mut handle = spec.start(RunContext::new(
        CallerCapability::Research,
        "book-grid-handle",
    ));
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    let result = handle
        .try_take_result()
        .expect("depth experiment result")
        .unwrap();
    assert_eq!(result.completed_candidates, 1);
    assert_eq!(result.ranking.len(), 1);
    assert_eq!(result.candidates[0].outcome.as_ref().unwrap().tier, "l2");
    assert!(handle.try_take_result().is_none());
}

#[test]
fn market_making_parameter_grid_changes_the_shared_strategy_configuration() {
    let root = scratch("market-making-experiment");
    let mut base = spec("unused-maker-grid", &root, "l2", 2);
    base.market_making = Some(qx_xingban::MarketMakingConfig {
        spread_bps: 100,
        quote_qty_raw: SCALE,
        max_inventory_raw: 5 * SCALE,
        tick_size_raw: SCALE / 100,
        inventory_skew_bps: 50,
    });
    let spec = RunDepthExperimentSpec {
        schema_version: 1,
        experiment_id: "maker-spread-grid".into(),
        base,
        parameter_space: vec![ExperimentParameterSpace {
            name: "market_making_spread_bps".into(),
            values: vec![50, 500],
        }],
        strategy_kinds: Vec::new(),
    };
    let result = run_depth_experiment(
        &spec,
        &RunContext::new(CallerCapability::Research, "maker-spread-grid"),
    )
    .expect("market-making parameter grid");
    assert_eq!(result.succeeded_candidates, 2);
    let summaries = result
        .candidates
        .iter()
        .map(|candidate| {
            let path = &candidate.outcome.as_ref().unwrap().artifacts.summary;
            let value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            value["market_making"]["spread_bps"].as_u64().unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(summaries, vec![50, 500]);

    let mut mixed = spec;
    mixed.parameter_space.push(ExperimentParameterSpace {
        name: "fast_window".into(),
        values: vec![2],
    });
    assert_eq!(
        mixed.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}

#[test]
fn order_book_strategy_selection_runs_classic_strategies_deterministically() {
    let root = scratch("depth-classic-strategy-selection");
    let kinds = [
        qx_strategy::BuiltinStrategyKind::SmaCross,
        qx_strategy::BuiltinStrategyKind::EmaCross,
        qx_strategy::BuiltinStrategyKind::Macd,
        qx_strategy::BuiltinStrategyKind::Rsi,
        qx_strategy::BuiltinStrategyKind::Bollinger,
        qx_strategy::BuiltinStrategyKind::DonchianBreakout,
        qx_strategy::BuiltinStrategyKind::Momentum,
        qx_strategy::BuiltinStrategyKind::MeanReversion,
        qx_strategy::BuiltinStrategyKind::Grid,
        qx_strategy::BuiltinStrategyKind::AtrTrend,
        qx_strategy::BuiltinStrategyKind::KeltnerTrend,
        qx_strategy::BuiltinStrategyKind::VwapReversion,
        qx_strategy::BuiltinStrategyKind::VolatilityBreakout,
    ];
    let mut base = spec("unused-depth-classics", &root, "l2", 2);
    base.strategy.fast_window = 2;
    base.strategy.slow_window = 3;
    let spec = RunDepthExperimentSpec {
        schema_version: 1,
        experiment_id: "depth-classic-strategy-selection".into(),
        base,
        parameter_space: Vec::new(),
        strategy_kinds: kinds.to_vec(),
    };
    let context = RunContext::new(
        CallerCapability::Research,
        "depth-classic-strategy-selection",
    );
    let result = run_depth_experiment(&spec, &context).expect("十三种策略应走共享 OrderBook 内核");
    assert_eq!(result.total_candidates, kinds.len());
    assert_eq!(
        result.succeeded_candidates,
        kinds.len(),
        "{:?}",
        result.candidates
    );
    assert_eq!(
        result
            .candidates
            .iter()
            .filter_map(|candidate| candidate.strategy_kind.as_deref())
            .collect::<Vec<_>>(),
        [
            "atr_trend",
            "bollinger",
            "donchian_breakout",
            "ema_cross",
            "grid",
            "keltner_trend",
            "macd",
            "mean_reversion",
            "momentum",
            "rsi",
            "sma_cross",
            "volatility_breakout",
            "vwap_reversion"
        ]
    );
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| candidate.outcome.as_ref().unwrap().result_hash.as_str())
            .collect::<Vec<_>>(),
        [
            "b8646edf8be77ad8",
            "b8646edf8be77ad8",
            "7c9c80855309e4f3",
            "96e1b8599ab38ac2",
            "45763eaafd9a88b5",
            "144082afa09698e7",
            "f9fac261f0c68fed",
            "99926bc58bbdc06e",
            "07bea6896df2684a",
            "462495222f7865f9",
            "29eaa3b5f7a79d32",
            "b8646edf8be77ad8",
            "91d4215d864e454e",
        ]
    );
    assert_eq!(run_depth_experiment(&spec, &context).unwrap(), result);

    let mut reordered = spec.clone();
    reordered.strategy_kinds.reverse();
    assert_eq!(run_depth_experiment(&reordered, &context).unwrap(), result);
    let mut duplicate = spec.clone();
    duplicate.strategy_kinds.push(kinds[0]);
    assert_eq!(
        duplicate.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
    let mut multi_leg = spec;
    multi_leg.strategy_kinds = vec![qx_strategy::BuiltinStrategyKind::PairsArbitrage];
    assert_eq!(
        multi_leg.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}

#[test]
fn depth_strategy_selection_forms_a_bounded_strategy_parameter_product() {
    let root = scratch("depth-strategy-parameter-product");
    let mut base = spec("unused-depth-product", &root, "l1", 1);
    base.strategy.fast_window = 2;
    base.strategy.slow_window = 3;
    let spec = RunDepthExperimentSpec {
        schema_version: 1,
        experiment_id: "depth-strategy-parameter-product".into(),
        base,
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: vec![SCALE, SCALE * 2],
        }],
        strategy_kinds: vec![
            qx_strategy::BuiltinStrategyKind::Momentum,
            qx_strategy::BuiltinStrategyKind::SmaCross,
        ],
    };
    assert_eq!(spec.validate().unwrap(), 4);
    let result = run_depth_experiment(
        &spec,
        &RunContext::new(
            CallerCapability::Research,
            "depth-strategy-parameter-product",
        ),
    )
    .expect("strategy and parameter dimensions should form one bounded product");
    assert_eq!(result.completed_candidates, 4);
    assert_eq!(result.succeeded_candidates, 4);
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| (
                candidate.strategy_kind.as_deref().unwrap(),
                candidate.parameters["quantity_raw"],
            ))
            .collect::<Vec<_>>(),
        [
            ("momentum", SCALE),
            ("momentum", SCALE * 2),
            ("sma_cross", SCALE),
            ("sma_cross", SCALE * 2),
        ]
    );
}
