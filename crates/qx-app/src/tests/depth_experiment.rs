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
