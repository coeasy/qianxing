//! Rust SDK lifecycle paths use the same application use cases and kernel.

use crate::tests::fixtures::{backtest_spec, scratch, write_frame};
use crate::{CallerCapability, ExperimentParameterSpace, RunContext, RunExperimentSpec, RunStatus};

#[test]
fn bar_run_handle_executes_the_shared_use_case_and_delivers_its_result_once() {
    let root = scratch("run-handle-bar");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 80);
    let spec = backtest_spec("run-handle-bar", &bars, &root.join("out"));
    let mut handle = spec.start(RunContext::new(
        CallerCapability::Research,
        "run-handle-bar",
    ));

    assert_eq!(handle.run_id(), "run-handle-bar");
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    let outcome = handle
        .try_take_result()
        .expect("completed run has a result")
        .expect("shared Rust backtest succeeds");
    assert!(outcome.fills > 0);
    assert_eq!(handle.status(), RunStatus::Succeeded);
    assert!(handle.try_take_result().is_none());
}

#[test]
fn experiment_run_handle_returns_the_shared_candidate_report_once() {
    let root = scratch("run-handle-experiment");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 80);
    let spec = RunExperimentSpec {
        schema_version: 1,
        experiment_id: "run-handle-grid".into(),
        base: backtest_spec("unused-base", &bars, &root.join("out")),
        parameter_space: vec![ExperimentParameterSpace {
            name: "fast_window".into(),
            values: vec![1, 2],
        }],
    };
    let mut handle = spec.start(RunContext::new(
        CallerCapability::Research,
        "run-handle-grid",
    ));

    assert_eq!(handle.wait(), RunStatus::Succeeded);
    let result = handle
        .try_take_result()
        .expect("completed experiment has a result")
        .expect("shared Rust experiment succeeds");
    assert_eq!(result.completed_candidates, 2);
    assert_eq!(result.succeeded_candidates, 2);
    assert_eq!(result.comparison.unwrap().runs.len(), 2);
    assert!(handle.try_take_result().is_none());
}
