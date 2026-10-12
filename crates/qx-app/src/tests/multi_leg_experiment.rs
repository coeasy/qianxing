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
        strategy_kinds: Vec::new(),
    };
    assert!(
        !spec.to_json().unwrap().contains("strategy_kinds"),
        "legacy specs retain their canonical JSON/fingerprint shape"
    );
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
        strategy_kinds: Vec::new(),
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

#[test]
fn multi_leg_strategy_selection_runs_all_arbitrage_kinds_deterministically() {
    let root = super::fixtures::scratch("multi-leg-strategy-selection");
    let kinds = [
        BuiltinStrategyKind::PairsArbitrage,
        BuiltinStrategyKind::BasisArbitrage,
        BuiltinStrategyKind::CrossVenueArbitrage,
        BuiltinStrategyKind::SpotFuturesArbitrage,
    ];
    let spec = RunMultiLegExperimentSpec {
        schema_version: 1,
        experiment_id: "multi-leg-strategy-selection".into(),
        base: base(&root),
        parameter_space: Vec::new(),
        strategy_kinds: kinds.to_vec(),
    };
    let mut reordered_spec = spec.clone();
    reordered_spec.strategy_kinds.reverse();
    assert_eq!(spec.to_json().unwrap(), reordered_spec.to_json().unwrap());
    let context = RunContext::new(CallerCapability::Research, "multi-leg-strategy-selection");
    let result = run_multi_leg_experiment(&spec, &context).expect("四种双腿策略应共用 Rust 内核");
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
            .map(|candidate| candidate.strategy_kind.as_deref().unwrap())
            .collect::<Vec<_>>(),
        [
            "basis_arbitrage",
            "cross_venue_arbitrage",
            "pairs_arbitrage",
            "spot_futures_arbitrage",
        ]
    );
    assert_eq!(run_multi_leg_experiment(&spec, &context).unwrap(), result);
    let mut reordered = spec.clone();
    reordered.strategy_kinds.reverse();
    assert_eq!(
        run_multi_leg_experiment(&reordered, &context).unwrap(),
        result
    );

    let mut duplicate = spec.clone();
    duplicate.strategy_kinds.push(kinds[0]);
    assert_eq!(
        duplicate.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
    let mut single_leg = spec;
    single_leg.strategy_kinds = vec![BuiltinStrategyKind::SmaCross];
    assert_eq!(
        single_leg.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}

#[test]
fn multi_leg_strategy_selection_forms_a_bounded_parameter_product() {
    let root = super::fixtures::scratch("multi-leg-strategy-parameter-product");
    let spec = RunMultiLegExperimentSpec {
        schema_version: 1,
        experiment_id: "multi-leg-strategy-parameter-product".into(),
        base: base(&root),
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: vec![SCALE, SCALE * 2],
        }],
        strategy_kinds: vec![
            BuiltinStrategyKind::PairsArbitrage,
            BuiltinStrategyKind::BasisArbitrage,
        ],
    };
    assert_eq!(spec.validate().unwrap(), 4);
    let result = run_multi_leg_experiment(
        &spec,
        &RunContext::new(
            CallerCapability::Research,
            "multi-leg-strategy-parameter-product",
        ),
    )
    .expect("strategy and numeric dimensions should form one bounded product");
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
            ("basis_arbitrage", SCALE),
            ("basis_arbitrage", SCALE * 2),
            ("pairs_arbitrage", SCALE),
            ("pairs_arbitrage", SCALE * 2),
        ]
    );
}
