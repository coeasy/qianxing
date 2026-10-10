//! Rust SDK lifecycle paths use the same application use cases and kernel.

use crate::tests::fixtures::{backtest_spec, scratch, write_frame};
use crate::{CallerCapability, RunContext, RunStatus};

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
