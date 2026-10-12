use crate::{
    run_depth_experiment, AppErrorCategory, CallerCapability, ExperimentParameterSpace, RunContext,
    RunDepthExperimentSpec, RunStatus,
};
use qx_core::SCALE;
use std::path::Path;

#[test]
fn tick_parameter_experiment_is_deterministic_and_isolates_bad_candidates() {
    let root = super::fixtures::scratch("tick-experiment");
    let spec = RunDepthExperimentSpec {
        schema_version: 1,
        experiment_id: "tick-grid".into(),
        base: super::depth_backtest::spec("unused-tick-grid", &root, "l1", 1),
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

#[test]
fn order_book_parameter_experiment_returns_a_cancellable_run_handle() {
    let root = super::fixtures::scratch("orderbook-experiment");
    let mut spec = RunDepthExperimentSpec {
        schema_version: 1,
        experiment_id: "book-grid-handle".into(),
        base: super::depth_backtest::spec("unused-book-grid", &root, "l2", 2),
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
    let root = super::fixtures::scratch("market-making-experiment");
    let mut base = super::depth_backtest::spec("unused-maker-grid", &root, "l2", 2);
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
    let root = super::fixtures::scratch("depth-classic-strategy-selection");
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
    ];
    let mut base = super::depth_backtest::spec("unused-depth-classics", &root, "l2", 2);
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
    let result = run_depth_experiment(&spec, &context).expect("十种策略应走共享 OrderBook 内核");
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
            "macd",
            "mean_reversion",
            "momentum",
            "rsi",
            "sma_cross"
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
    let root = super::fixtures::scratch("depth-strategy-parameter-product");
    let mut base = super::depth_backtest::spec("unused-depth-product", &root, "l1", 1);
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
