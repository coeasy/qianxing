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
