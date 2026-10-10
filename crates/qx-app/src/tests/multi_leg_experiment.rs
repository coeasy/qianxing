use crate::{
    run_multi_leg_experiment, AppErrorCategory, BuiltinStrategySpec, CallerCapability,
    ExperimentParameterSpace, MultiLegBacktestSpec, RunContext, RunMultiLegExperimentSpec,
    RunStatus,
};
use qx_core::SCALE;
use qx_strategy::BuiltinStrategyKind;
use std::path::Path;

fn base(root: &Path) -> MultiLegBacktestSpec {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    MultiLegBacktestSpec::new(
        "unused-base-run",
        fixtures
            .join("qianxing.bar-frame.pairs-primary.example.json")
            .to_string_lossy(),
        fixtures
            .join("qianxing.bar-frame.pairs-reference.example.json")
            .to_string_lossy(),
        "USDT",
        1_000_000 * SCALE,
        1_000_000 * SCALE,
        7,
        root.join("out").to_string_lossy(),
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "pair-grid"),
    )
}

#[test]
fn multi_leg_parameter_experiment_runs_independent_rust_candidates_and_ranks_them() {
    let root = super::fixtures::scratch("multi-leg-experiment");
    let spec = RunMultiLegExperimentSpec {
        schema_version: 1,
        experiment_id: "pairs-grid".into(),
        base: base(&root),
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: vec![SCALE, SCALE * 2],
        }],
    };
    let context = RunContext::new(CallerCapability::Research, "pairs-grid");
    let result = run_multi_leg_experiment(&spec, &context).expect("multi-leg parameter grid");
    assert_eq!(result.total_candidates, 2);
    assert_eq!(result.completed_candidates, 2);
    assert_eq!(result.succeeded_candidates, 2);
    assert_eq!(result.failed_candidates, 0);
    assert!(result
        .candidates
        .iter()
        .all(|candidate| candidate.error.is_none()));
    assert_eq!(result.ranking.len(), 2);
    assert!(result.ranking[0].combined_return_bps >= result.ranking[1].combined_return_bps);
    assert!(Path::new(&result.artifact_path).is_file());
    let repeated = run_multi_leg_experiment(&spec, &context).expect("idempotent experiment");
    assert_eq!(repeated, result);

    let mut changed = spec;
    changed.parameter_space[0].values = vec![SCALE, SCALE * 3];
    assert_eq!(
        run_multi_leg_experiment(&changed, &context)
            .unwrap_err()
            .category(),
        AppErrorCategory::Conflict
    );
}

#[test]
fn multi_leg_experiment_isolates_bad_candidates_and_supports_run_handle() {
    let root = super::fixtures::scratch("multi-leg-experiment-isolation");
    let spec = RunMultiLegExperimentSpec {
        schema_version: 1,
        experiment_id: "pairs-isolated-grid".into(),
        base: base(&root),
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: vec![0, SCALE],
        }],
    };
    let context = RunContext::new(CallerCapability::Research, "pairs-isolated-grid");
    let result = run_multi_leg_experiment(&spec, &context).expect("one bad candidate is isolated");
    assert_eq!(result.completed_candidates, 2);
    assert_eq!(result.failed_candidates, 1);
    assert_eq!(result.succeeded_candidates, 1);
    assert!(result.candidates[0].error.is_some());
    assert!(result.candidates[1].outcome.is_some());

    let mut handle = spec.start(RunContext::new(
        CallerCapability::Research,
        "pairs-isolated-handle",
    ));
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    let outcome = handle
        .try_take_result()
        .expect("single experiment result")
        .unwrap();
    assert_eq!(outcome.completed_candidates, 2);
    assert!(handle.try_take_result().is_none());
}
