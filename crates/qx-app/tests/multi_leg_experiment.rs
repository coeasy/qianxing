mod common;

use common::scratch;
use qx_app::{
    run_multi_leg_experiment, AppErrorCategory, BuiltinStrategySpec, CallerCapability,
    ExperimentParameterSpace, MultiLegBacktestSpec, MultiLegDatasetInput, RunContext,
    RunMultiLegExperimentSpec, RunStatus, StrategyArtifactIdentity,
};
use qx_core::{Quantity, Side, SCALE};
use qx_strategy::{
    BuiltinStrategyKind, MarketEvent, Strategy, StrategyContext, StrategyDecision,
    StrategyOrderIntent, STRATEGY_API_VERSION,
};
use std::path::Path;

fn base(root: &Path) -> MultiLegBacktestSpec {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    MultiLegBacktestSpec::new(
        MultiLegDatasetInput {
            run_id: ("unused-base-run").into(),
            primary_bars_path: (fixtures
                .join("qianxing.bar-frame.pairs-primary.example.json")
                .to_string_lossy())
            .into(),
            reference_bars_path: (fixtures
                .join("qianxing.bar-frame.pairs-reference.example.json")
                .to_string_lossy())
            .into(),
        },
        "USDT",
        1_000_000 * SCALE,
        1_000_000 * SCALE,
        7,
        root.join("out").to_string_lossy(),
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "pair-grid"),
    )
}

struct BuyEachLeg {
    quantity: i128,
    seen: std::collections::BTreeSet<String>,
}

impl Strategy for BuyEachLeg {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let instrument = event
            .instrument()
            .cloned()
            .ok_or("event instrument missing")?;
        let intents = if self.seen.insert(instrument.to_string()) {
            vec![StrategyOrderIntent {
                intent_id: self.seen.len() as u64,
                instrument,
                side: Side::Buy,
                qty: Quantity::from_raw(self.quantity),
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,

                qty_exact: None,
                limit_price_exact: None,
                quote_budget: None,
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
fn multi_leg_parameter_experiment_runs_independent_rust_candidates_and_ranks_them() {
    let root = scratch("multi-leg-experiment");
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
    let artifact_locations = result.artifact_locations.as_ref().unwrap();
    let input_spec_path = artifact_locations
        .get("experiment_input")
        .expect("multi-leg experiment input spec locator");
    let input_spec_bytes = std::fs::read(input_spec_path).expect("persisted input spec sidecar");
    let record = qx_spec::RunRecordV2::from_json(
        &std::fs::read_to_string(result.run_record.as_ref().unwrap()).unwrap(),
    )
    .expect("RunRecord V2 parses");
    let input_ref = record
        .artifact_refs
        .iter()
        .find(|artifact| artifact.name == "experiment_input")
        .expect("RunRecord binds input spec");
    assert_eq!(
        input_ref.artifact_id,
        qx_strategy::sha256_hex(&input_spec_bytes)
    );
    let result_value = serde_json::to_value(&result).unwrap();
    qx_app::artifact_store::verify_declared_run_manifest(&result_value)
        .expect("RunRecord validates the original multi-leg input spec");
    std::fs::write(input_spec_path, b"tampered").expect("tamper input spec sidecar");
    assert!(qx_app::artifact_store::verify_declared_run_manifest(&result_value).is_err());
    std::fs::write(input_spec_path, input_spec_bytes).expect("restore input spec sidecar");
    let canonical_json = result
        .canonical_run_spec_json
        .as_deref()
        .expect("multi-leg experiment canonical identity");
    let canonical = qx_spec::CanonicalRunSpec::from_json(canonical_json)
        .expect("valid multi-leg experiment CanonicalRunSpec");
    assert_eq!(
        result.canonical_run_spec_fingerprint.as_deref(),
        Some(canonical.fingerprint().unwrap().as_str())
    );
    assert!(result.input_data_fingerprint.is_some());
    assert!(!canonical_json.contains(&root.to_string_lossy().to_string()));
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
fn multi_leg_experiment_accepts_exact_principal_v2_and_persists_v6_runs() {
    let root = scratch("multi-leg-experiment-exact");
    let exact_base = base(&root)
        .with_initial_cash_exact(
            qx_core::ExactDecimal::parse("1000000.0000000005").unwrap(),
            qx_core::ExactDecimal::parse("1000000.0000000005").unwrap(),
        )
        .expect("exact two-leg experiment base");
    let spec = RunMultiLegExperimentSpec {
        schema_version: 1,
        experiment_id: "pairs-grid-exact".into(),
        base: exact_base,
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: vec![SCALE],
        }],
        strategy_kinds: Vec::new(),
    };
    let result = run_multi_leg_experiment(
        &spec,
        &RunContext::new(CallerCapability::Research, "pairs-grid-exact"),
    )
    .expect("exact principal candidate experiment");
    assert_eq!(result.succeeded_candidates, 1);
    let outcome = result.candidates[0]
        .outcome
        .as_ref()
        .expect("candidate has a completed backtest");
    let artifact: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&outcome.attribution_artifact).unwrap())
            .unwrap();
    assert_eq!(artifact["schema_version"], 6);
    assert_eq!(artifact["initial_cash_exact"], "2000000.000000001");
    qx_app::read_multi_leg_report_facts_v1(
        outcome,
        &RunContext::new(CallerCapability::Research, "pairs-grid-exact-report"),
    )
    .expect("experiment candidate report is exact-principal verifiable");
}

#[test]
fn multi_leg_experiment_identity_ignores_input_paths_and_rejects_changed_data_reuse() {
    let root = scratch("multi-leg-experiment-canonical-identity");
    let source = base(&root);
    let alternate_inputs = root.join("alternate-inputs");
    std::fs::create_dir_all(&alternate_inputs).unwrap();
    let primary = alternate_inputs.join("primary.json");
    let reference = alternate_inputs.join("reference.json");
    std::fs::copy(&source.primary_bars_path, &primary).unwrap();
    std::fs::copy(&source.reference_bars_path, &reference).unwrap();

    let mut first = source.clone();
    first.output_dir = root.join("first-output").to_string_lossy().into_owned();
    let first = RunMultiLegExperimentSpec {
        schema_version: 1,
        experiment_id: "path-free-multi-leg".into(),
        base: first,
        parameter_space: Vec::new(),
        strategy_kinds: vec![BuiltinStrategyKind::PairsArbitrage],
    };
    let context = RunContext::new(CallerCapability::Research, "path-free-multi-leg");
    let first_result = run_multi_leg_experiment(&first, &context).unwrap();

    let mut second = source;
    second.primary_bars_path = primary.to_string_lossy().into_owned();
    second.reference_bars_path = reference.to_string_lossy().into_owned();
    second.output_dir = root.join("second-output").to_string_lossy().into_owned();
    let second = RunMultiLegExperimentSpec {
        schema_version: 1,
        experiment_id: "path-free-multi-leg".into(),
        base: second,
        parameter_space: Vec::new(),
        strategy_kinds: vec![BuiltinStrategyKind::PairsArbitrage],
    };
    let second_result = run_multi_leg_experiment(&second, &context).unwrap();
    assert_eq!(
        first_result.canonical_run_spec_json, second_result.canonical_run_spec_json,
        "same data and parameters at different paths must retain the canonical identity"
    );
    assert_eq!(
        first_result.canonical_run_spec_fingerprint,
        second_result.canonical_run_spec_fingerprint
    );

    let changed_frame_payload =
        std::fs::read_to_string(&primary).expect("read relocated primary BarFrame");
    let mut changed_frame = qx_datastruct::BarFrame::from_json(&changed_frame_payload)
        .expect("parse relocated primary BarFrame");
    changed_frame.close_raw[0] += 1;
    std::fs::write(&primary, changed_frame.to_json()).expect("write changed primary BarFrame");
    assert_eq!(
        run_multi_leg_experiment(&second, &context)
            .unwrap_err()
            .category(),
        AppErrorCategory::Conflict,
        "a changed content fingerprint must not reuse a prior experiment_id"
    );
}

#[test]
fn multi_leg_experiment_does_not_reuse_legacy_report_without_data_identity() {
    let root = scratch("multi-leg-experiment-legacy-identity");
    let spec = RunMultiLegExperimentSpec {
        schema_version: 1,
        experiment_id: "legacy-multi-leg-identity".into(),
        base: base(&root),
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: vec![SCALE],
        }],
        strategy_kinds: Vec::new(),
    };
    let context = RunContext::new(CallerCapability::Research, "legacy-multi-leg-identity");
    let result = run_multi_leg_experiment(&spec, &context).unwrap();
    let mut legacy: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&result.artifact_path).expect("read experiment report"),
    )
    .expect("parse experiment report");
    let report = legacy.as_object_mut().expect("report object");
    report.remove("canonical_run_spec_json");
    report.remove("canonical_run_spec_fingerprint");
    report.remove("input_data_fingerprint");
    std::fs::write(
        &result.artifact_path,
        serde_json::to_vec(&legacy).expect("serialize legacy report"),
    )
    .expect("write legacy report");

    let error = run_multi_leg_experiment(&spec, &context)
        .expect_err("legacy report cannot be reused without proven data identity");
    assert_eq!(error.category(), AppErrorCategory::Conflict);
    assert!(error.message().contains("缺少数据级 CanonicalRunSpec"));
}

#[test]
fn multi_leg_experiment_rejects_candidate_manifest_detached_from_parent_identity() {
    let root = scratch("multi-leg-experiment-candidate-manifest");
    let spec = RunMultiLegExperimentSpec {
        schema_version: 1,
        experiment_id: "candidate-manifest-check".into(),
        base: base(&root),
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: vec![SCALE],
        }],
        strategy_kinds: Vec::new(),
    };
    let context = RunContext::new(CallerCapability::Research, "candidate-manifest-check");
    let result = run_multi_leg_experiment(&spec, &context).unwrap();
    let outcome = result.candidates[0]
        .outcome
        .as_ref()
        .expect("successful candidate");
    let manifest_path = Path::new(&outcome.attribution_artifact)
        .with_file_name(format!("{}.multi-leg.run.json", outcome.run_id));
    let mut manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&manifest_path).expect("read candidate manifest"),
    )
    .expect("parse candidate manifest");
    manifest["data_fingerprint"] = serde_json::Value::String("different-inputs".into());
    std::fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest).expect("serialize modified candidate manifest"),
    )
    .expect("write modified candidate manifest");

    assert_eq!(
        run_multi_leg_experiment(&spec, &context)
            .unwrap_err()
            .category(),
        AppErrorCategory::StorageFailure,
        "a detached candidate manifest must not be returned as a verified experiment"
    );
}

#[test]
fn multi_leg_experiment_isolates_bad_candidates_and_supports_run_handle() {
    let root = scratch("multi-leg-experiment-isolation");
    let spec = RunMultiLegExperimentSpec {
        schema_version: 1,
        experiment_id: "pairs-isolated-grid".into(),
        base: base(&root),
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: vec![i128::MAX, SCALE],
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
    let root = scratch("multi-leg-strategy-selection");
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
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| {
                let outcome = candidate.outcome.as_ref().unwrap();
                (
                    outcome.primary_result_hash.as_str(),
                    outcome.reference_result_hash.as_str(),
                )
            })
            .collect::<Vec<_>>(),
        [
            ("d825fc7cead9585f", "66078d855b4b6d81"),
            ("1e2690bc87ec6286", "8563dd0fe2ae0c86"),
            ("8a1218b12866ab9b", "f382693ecd7cb454"),
            ("d825fc7cead9585f", "4a6ff50e9df0a710"),
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
    let root = scratch("multi-leg-strategy-parameter-product");
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

#[test]
fn caller_strategy_factory_runs_fresh_parameterized_candidates_and_binds_source_identity() {
    let root = scratch("multi-leg-custom-strategy-experiment");
    let mut base = base(&root);
    base.strategy.strategy_id = "custom-pair-grid".into();
    let spec = RunMultiLegExperimentSpec {
        schema_version: 1,
        experiment_id: "custom-pair-grid".into(),
        base,
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: vec![SCALE, SCALE * 2],
        }],
        strategy_kinds: Vec::new(),
    };
    let identity = StrategyArtifactIdentity {
        strategy_id: "custom-pair-grid".into(),
        strategy_version: "user-v1".into(),
        source_sha256: "a".repeat(64),
    };
    let mut factory_calls = Vec::new();
    let mut factory = |candidate: &MultiLegBacktestSpec| {
        factory_calls.push((candidate.run_id.clone(), candidate.strategy.quantity_raw));
        Ok(Box::new(BuyEachLeg {
            quantity: candidate.strategy.quantity_raw,
            seen: Default::default(),
        }) as Box<dyn Strategy>)
    };
    let context = RunContext::new(CallerCapability::Research, "custom-pair-grid");
    let result = spec
        .run_with_strategy_factory(&identity, &mut factory, &context)
        .expect("custom strategy factory uses the shared Rust experiment runner");
    assert_eq!(factory_calls.len(), 2);
    assert_eq!(factory_calls[0].1, SCALE);
    assert_eq!(factory_calls[1].1, SCALE * 2);
    assert_eq!(result.completed_candidates, 2);
    assert_eq!(result.succeeded_candidates, 2);
    assert!(result.candidates.iter().all(|candidate| {
        let outcome = candidate.outcome.as_ref().expect("candidate outcome");
        outcome.primary_fills > 0 && outcome.reference_fills > 0
    }));
    assert_ne!(
        result.candidates[0]
            .outcome
            .as_ref()
            .unwrap()
            .primary_result_hash,
        result.candidates[1]
            .outcome
            .as_ref()
            .unwrap()
            .primary_result_hash,
        "candidate parameters must affect the strategy-generated result"
    );
    assert_eq!(
        spec.run_with_strategy_factory(
            &identity,
            &mut |candidate| Ok(Box::new(BuyEachLeg {
                quantity: candidate.strategy.quantity_raw,
                seen: Default::default(),
            }) as Box<dyn Strategy>),
            &context,
        )
        .unwrap(),
        result,
        "identical spec and source identity reuses deterministic evidence"
    );

    let mut changed_identity = identity;
    changed_identity.source_sha256 = "b".repeat(64);
    assert_eq!(
        spec.run_with_strategy_factory(
            &changed_identity,
            &mut |candidate| Ok(Box::new(BuyEachLeg {
                quantity: candidate.strategy.quantity_raw,
                seen: Default::default(),
            }) as Box<dyn Strategy>),
            &context,
        )
        .unwrap_err()
        .category(),
        AppErrorCategory::Conflict,
        "changed strategy source cannot reuse an experiment artifact"
    );
}
