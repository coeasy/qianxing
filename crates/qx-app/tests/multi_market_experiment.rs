mod common;

use common::{scratch, write_frame};
use qx_app::{
    run_multi_market_experiment, AppErrorCategory, BuiltinStrategySpec, CallerCapability,
    ExperimentParameterRange, ExperimentParameterSpace, ExperimentSearchAlgorithm,
    ExperimentSearchPlan, ExperimentSplitPlan, MultiMarketBacktestMarket, MultiMarketBacktestSpec,
    PortfolioStrategySpec, RunContext, RunMultiMarketExperimentSpec, RunStatus,
    StrategyArtifactIdentity,
};
use qx_strategy::{
    BuiltinStrategyKind, MarketEvent, Strategy, StrategyContext, StrategyDecision,
    STRATEGY_API_VERSION,
};

const SCALE: i128 = 1_000_000_000;

fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

struct NoopStrategy;

impl Strategy for NoopStrategy {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        Ok(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("{}:{}", context.strategy_id, event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 10_000,
            priority: 0,
            expires_at: 0,
            intents: Vec::new(),
        })
    }
}

fn experiment(root: &std::path::Path) -> RunMultiMarketExperimentSpec {
    let instruments = ["BTCUSDT.BINANCE", "ETHUSDT.KRAKEN", "SOLUSDT.BYBIT"];
    let markets = instruments
        .iter()
        .enumerate()
        .map(|(index, instrument)| {
            let bars_path = write_frame(root, &format!("market-{index}.json"), instrument, 80);
            let mut strategy =
                BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, format!("slot-{index}"));
            strategy.fast_window = 2;
            strategy.slow_window = 4;
            MultiMarketBacktestMarket {
                instrument: (*instrument).into(),
                bars_path,
                dataset_manifest_path: None,
                priority: index as u32,
                market_spec: None,
                asset_class: None,
                fee_schedule: None,
                order_policy: None,
                margin_model: None,
                funding_bps: 0,
                funding_events: Vec::new(),
                strategies: vec![PortfolioStrategySpec {
                    priority: 0,
                    reserved_capital_raw: 20_000 * SCALE,
                    reserved_capital_exact: None,
                    strategy,
                }],
            }
        })
        .collect();
    RunMultiMarketExperimentSpec {
        schema_version: 1,
        experiment_id: "multi-market-grid".into(),
        target_strategy_id: "slot-0".into(),
        base: MultiMarketBacktestSpec {
            schema_version: 1,
            run_id: "unused-grid-base".into(),
            account_id: "grid-account".into(),
            settlement_currency: "USDT".into(),
            initial_cash_raw: 100_000 * SCALE,
            initial_cash_exact: None,
            seed: 7,
            output_dir: root.join("output").to_string_lossy().into_owned(),
            markets,
        },
        parameter_space: vec![ExperimentParameterSpace {
            name: "fast_window".into(),
            values: vec![2, 3],
        }],
        strategy_kinds: vec![BuiltinStrategyKind::SmaCross, BuiltinStrategyKind::EmaCross],
        callback_parameter_space: Vec::new(),
        split_plan: None,
        search: None,
    }
}

#[test]
fn multi_market_parameter_grid_runs_shared_engine_ranks_and_reuses_identity() {
    let root = scratch("multi-market-experiment");
    let spec = experiment(&root);
    assert_eq!(spec.validate().unwrap(), 4);
    let outcome = run_multi_market_experiment(&spec, &research("multi-market-grid"))
        .expect("grid uses shared multi-market backtest");
    assert_eq!(outcome.total_candidates, 4);
    assert_eq!(outcome.completed_candidates, 4);
    assert_eq!(outcome.failed_candidates, 0);
    assert_eq!(outcome.succeeded_candidates, 4);
    assert_eq!(outcome.ranking.len(), 4);
    assert!(outcome.candidates.iter().all(|candidate| {
        candidate
            .outcome
            .as_ref()
            .is_some_and(|run| run.markets_total == 3)
    }));
    assert_eq!(
        outcome
            .candidates
            .iter()
            .map(|candidate| candidate.ordinal)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert!(outcome
        .ranking
        .windows(2)
        .all(|pair| pair[0].return_bps > pair[1].return_bps
            || (pair[0].return_bps == pair[1].return_bps
                && (pair[0].max_drawdown_bps < pair[1].max_drawdown_bps
                    || (pair[0].max_drawdown_bps == pair[1].max_drawdown_bps
                        && pair[0].ordinal < pair[1].ordinal)))));
    assert!(outcome.canonical_run_spec_json.is_some());
    assert!(outcome.canonical_run_spec_fingerprint.is_some());
    assert!(outcome.input_data_fingerprint.is_some());
    assert!(outcome.result_hash.is_some());
    let locations = outcome.artifact_locations.as_ref().unwrap();
    let input_path = std::path::PathBuf::from(&locations["experiment_input"]);
    let input_bytes = std::fs::read(&input_path).expect("experiment input sidecar is persisted");
    let record_path = outcome.run_record.as_ref().unwrap();
    let record: qx_spec::RunRecordV2 = serde_json::from_str(
        &std::fs::read_to_string(record_path).expect("RunRecord V2 is persisted"),
    )
    .expect("RunRecord V2 JSON is valid");
    let input_ref = record
        .artifact_refs
        .iter()
        .find(|artifact| artifact.name == "experiment_input")
        .expect("RunRecord binds the input sidecar");
    assert_eq!(input_ref.byte_len, input_bytes.len() as u64);
    assert_eq!(input_ref.artifact_id, qx_strategy::sha256_hex(&input_bytes));
    let summary: serde_json::Value = serde_json::from_str(&outcome.to_json().unwrap()).unwrap();
    qx_app::artifact_store::verify_declared_run_manifest(&summary)
        .expect("RunManifest and RunRecord verify");
    let mut tampered = input_bytes.clone();
    tampered.push(b' ');
    std::fs::write(&input_path, &tampered).expect("tamper input sidecar");
    assert!(qx_app::artifact_store::verify_declared_run_manifest(&summary).is_err());
    std::fs::write(&input_path, &input_bytes).expect("restore input sidecar");
    qx_app::artifact_store::verify_declared_run_manifest(&summary)
        .expect("restored input sidecar verifies");

    let repeated = run_multi_market_experiment(&spec, &research("multi-market-grid"))
        .expect("same experiment identity returns prior report");
    assert_eq!(repeated.spec_fingerprint, outcome.spec_fingerprint);
    assert_eq!(repeated.ranking, outcome.ranking);
    assert_eq!(repeated.artifact_path, outcome.artifact_path);

    let mut asynchronous_spec = spec.clone();
    asynchronous_spec.experiment_id = "multi-market-grid-async".into();
    let mut handle = asynchronous_spec.start(research("multi-market-grid-async"));
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    let asynchronous = handle
        .try_take_result()
        .unwrap()
        .expect("async grid succeeds");
    assert_eq!(asynchronous.total_candidates, 4);
    assert_eq!(asynchronous.failed_candidates, 0);
}

#[test]
fn multi_market_legacy_experiment_report_upgrades_to_run_record_v2() {
    let root = scratch("multi-market-experiment-legacy-upgrade");
    let spec = experiment(&root);
    let mut legacy = run_multi_market_experiment(&spec, &research("multi-market-legacy-upgrade"))
        .expect("initial run creates an experiment report");
    legacy.canonical_run_spec_json = None;
    legacy.canonical_run_spec_fingerprint = None;
    legacy.input_data_fingerprint = None;
    legacy.result_hash = None;
    legacy.run_manifest = None;
    legacy.run_record = None;
    legacy.artifact_locations = None;
    std::fs::write(&legacy.artifact_path, legacy.to_json().unwrap())
        .expect("replace stable report with a legacy shape");

    let upgraded = run_multi_market_experiment(&spec, &research("multi-market-legacy-upgrade"))
        .expect("legacy result is upgraded without rerunning candidates");
    assert!(upgraded.canonical_run_spec_json.is_some());
    assert!(upgraded.run_manifest.is_some());
    assert!(upgraded.run_record.is_some());
    assert!(upgraded.artifact_locations.is_some());
    assert_eq!(upgraded.ranking, legacy.ranking);
    let summary: serde_json::Value = serde_json::from_str(&upgraded.to_json().unwrap()).unwrap();
    qx_app::artifact_store::verify_declared_run_manifest(&summary)
        .expect("upgraded result verifies");
}

#[test]
fn multi_market_experiment_reuses_canonical_identity_after_bar_files_move() {
    let root = scratch("multi-market-experiment-relocated-inputs");
    let spec = experiment(&root);
    let original = run_multi_market_experiment(&spec, &research("multi-market-relocated"))
        .expect("initial experiment persists a canonical identity");
    let original_fingerprint = original
        .canonical_run_spec_fingerprint
        .clone()
        .expect("canonical identity exists");
    let moved_root = root.join("moved-data");
    std::fs::create_dir_all(&moved_root).expect("create relocated data directory");
    let mut relocated = spec.clone();
    for (index, market) in relocated.base.markets.iter_mut().enumerate() {
        let new_path = moved_root.join(format!("market-{index}.json"));
        std::fs::copy(&market.bars_path, &new_path).expect("copy identical BarFrame");
        market.bars_path = new_path.to_string_lossy().into_owned();
    }

    let reused = run_multi_market_experiment(&relocated, &research("multi-market-relocated"))
        .expect("path-only changes reuse verified canonical experiment identity");
    assert_eq!(
        reused.canonical_run_spec_fingerprint.as_deref(),
        Some(original_fingerprint.as_str())
    );
    assert_eq!(
        reused.input_data_fingerprint,
        original.input_data_fingerprint
    );
    assert_eq!(reused.ranking, original.ranking);
    assert_eq!(reused.artifact_path, original.artifact_path);
    let summary: serde_json::Value = serde_json::from_str(&reused.to_json().unwrap()).unwrap();
    qx_app::artifact_store::verify_declared_run_manifest(&summary)
        .expect("relocated identical inputs retain a verifiable envelope");

    let original_bar = std::path::PathBuf::from(&spec.base.markets[0].bars_path);
    let mut changed_frame: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&original_bar).expect("read original BarFrame"))
            .expect("parse relocated BarFrame");
    changed_frame["volume_raw"][0] =
        serde_json::json!(changed_frame["volume_raw"][0].as_i64().unwrap() + 1);
    std::fs::write(
        &original_bar,
        serde_json::to_vec(&changed_frame).expect("serialize changed BarFrame"),
    )
    .expect("change original BarFrame content");
    assert_eq!(
        run_multi_market_experiment(&spec, &research("multi-market-relocated"))
            .unwrap_err()
            .category(),
        AppErrorCategory::Conflict,
        "canonical reuse must still reject changed data bytes at the same path"
    );
}

#[test]
fn multi_market_experiment_rejects_ambiguous_target_and_caps_cartesian_candidates() {
    let root = scratch("multi-market-experiment-validation");
    let mut invalid = experiment(&root);
    invalid.target_strategy_id = "missing-slot".into();
    assert_eq!(
        invalid.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );

    let mut too_many = experiment(&root);
    too_many.strategy_kinds = vec![BuiltinStrategyKind::SmaCross];
    too_many.parameter_space = vec![ExperimentParameterSpace {
        name: "fast_window".into(),
        values: (1..=257).collect(),
    }];
    assert_eq!(
        too_many.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );

    let mut callback_grid = experiment(&root);
    callback_grid.experiment_id = "multi-market-callback-grid".into();
    callback_grid.parameter_space.clear();
    callback_grid.strategy_kinds.clear();
    callback_grid.callback_parameter_space = vec![ExperimentParameterSpace {
        name: "risk_scale".into(),
        values: vec![2, 4],
    }];
    assert_eq!(callback_grid.validate().unwrap(), 2);
    let mut callback_json = serde_json::to_value(&callback_grid).unwrap();
    callback_json
        .as_object_mut()
        .unwrap()
        .remove("parameter_space");
    assert_eq!(
        RunMultiMarketExperimentSpec::from_json(&callback_json.to_string()).unwrap(),
        callback_grid
    );
    assert_eq!(
        run_multi_market_experiment(&callback_grid, &research("callback-without-factory"))
            .unwrap_err()
            .category(),
        AppErrorCategory::InvalidInput
    );
    callback_grid.callback_parameter_space = (0..7)
        .map(|index| ExperimentParameterSpace {
            name: format!("parameter_{index}"),
            values: vec![1],
        })
        .collect();
    assert_eq!(
        callback_grid.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}

#[test]
fn multi_market_experiment_parameter_maps_have_deterministic_order() {
    let root = scratch("multi-market-experiment-grid-order");
    let mut spec = experiment(&root);
    spec.strategy_kinds.reverse();
    let mut dimensions = spec.parameter_space.clone();
    dimensions.reverse();
    spec.parameter_space = dimensions;
    let json = spec.to_json().unwrap();
    let reparsed = RunMultiMarketExperimentSpec::from_json(&json).unwrap();
    assert_eq!(json, reparsed.to_json().unwrap());
    assert_eq!(
        reparsed
            .strategy_kinds
            .iter()
            .map(|kind| kind.name())
            .collect::<Vec<_>>(),
        ["ema_cross", "sma_cross"]
    );
}

#[test]
fn multi_market_experiment_uses_walk_forward_training_and_fresh_oos_runs() {
    let root = scratch("multi-market-experiment-walk-forward");
    let mut spec = experiment(&root);
    spec.experiment_id = "multi-market-walk-forward".into();
    spec.strategy_kinds = vec![BuiltinStrategyKind::SmaCross];
    spec.parameter_space = vec![ExperimentParameterSpace {
        name: "fast_window".into(),
        values: vec![2, 3],
    }];
    spec.split_plan = Some(ExperimentSplitPlan {
        train_bars: 32,
        test_bars: 16,
        walk_forward: true,
        step_bars: 16,
        purge_bars: 2,
        embargo_bars: 0,
    });

    let result = run_multi_market_experiment(&spec, &research("multi-market-walk-forward"))
        .expect("walk-forward experiment completes");
    let report = result.out_of_sample.expect("OOS report is present");
    assert_eq!(report.folds.len(), 2);
    assert_eq!(report.folds[0].train_range, [0, 32]);
    assert_eq!(report.folds[0].test_range, [34, 50]);
    assert_eq!(report.folds[0].purge_range, Some([32, 34]));
    assert_eq!(report.folds[1].train_range, [16, 48]);
    assert_eq!(report.folds[1].test_range, [50, 66]);
    assert!(result.candidates.iter().all(|candidate| {
        candidate
            .outcome
            .as_ref()
            .is_some_and(|outcome| outcome.run_id.contains("-train-"))
    }));
    for fold in &report.folds {
        let oos_dir = root
            .join("output")
            .join(&spec.experiment_id)
            .join(format!("fold-{:04}", fold.fold))
            .join(format!("oos-{:04}", fold.selected_ordinal));
        assert!(oos_dir.is_dir());
        let run_id = format!(
            "{}-f{:04}-oos-{:04}",
            spec.experiment_id, fold.fold, fold.selected_ordinal
        );
        let artifact_path = oos_dir.join(format!("{run_id}.multi-market.json"));
        let artifact: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(artifact_path).expect("OOS artifact is persisted"),
        )
        .expect("OOS artifact JSON is valid");
        let test_start = artifact["execution_window"]["trade_start_ts"]
            .as_u64()
            .expect("OOS start timestamp is bound to the artifact");
        let fills_path = artifact["fills_path"]
            .as_str()
            .expect("OOS artifact references its fills");
        let fills: Vec<serde_json::Value> = serde_json::from_str(
            &std::fs::read_to_string(fills_path).expect("OOS fills are persisted"),
        )
        .expect("OOS fills JSON is valid");
        assert!(fills
            .iter()
            .all(|fill| fill["ts"].as_u64().is_some_and(|ts| ts >= test_start)));
    }
}

#[test]
fn multi_market_walk_forward_report_surfaces_per_fold_candidate_failures() {
    let root = scratch("multi-market-experiment-fold-failure");
    let mut spec = experiment(&root);
    spec.experiment_id = "multi-market-fold-failure".into();
    spec.strategy_kinds.clear();
    spec.parameter_space.clear();
    spec.callback_parameter_space = vec![ExperimentParameterSpace {
        name: "risk_scale".into(),
        values: vec![1, 2],
    }];
    spec.split_plan = Some(ExperimentSplitPlan {
        train_bars: 32,
        test_bars: 16,
        walk_forward: true,
        step_bars: 16,
        purge_bars: 0,
        embargo_bars: 0,
    });
    let identity = StrategyArtifactIdentity {
        strategy_id: spec.target_strategy_id.clone(),
        strategy_version: "no-op-v1".into(),
        source_sha256: "e".repeat(64),
    };
    let result = spec
        .run_with_parameter_factory(
            &identity,
            &mut |candidate, _parameters| {
                if candidate.run_id.contains("f0002-train-0001") {
                    return Err("intentional fold-local factory failure".into());
                }
                Ok(Box::new(NoopStrategy) as Box<dyn Strategy>)
            },
            &research("multi-market-fold-failure"),
        )
        .expect("other candidates remain eligible after one fold-local failure");
    let report = result.out_of_sample.as_ref().unwrap();
    assert_eq!(report.folds.len(), 3);
    assert_eq!(report.folds[1].selected_ordinal, 2);
    let candidate_fingerprints = result
        .candidates
        .iter()
        .filter_map(|candidate| candidate.outcome.as_ref())
        .map(|outcome| outcome.spec_fingerprint.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(candidate_fingerprints.len(), 2);
    assert_eq!(result.fold_candidate_failures.len(), 1);
    assert_eq!(result.fold_candidate_failures[0].fold, 2);
    assert_eq!(result.fold_candidate_failures[0].ordinal, 1);
    assert!(result.fold_candidate_failures[0]
        .error
        .contains("intentional fold-local factory failure"));
}

#[test]
fn multi_market_adaptive_search_uses_training_only_and_restarts_per_fold() {
    let root = scratch("multi-market-adaptive-search");
    let mut spec = experiment(&root);
    spec.experiment_id = "multi-market-adaptive-search".into();
    spec.parameter_space.clear();
    spec.strategy_kinds.clear();
    spec.callback_parameter_space.clear();
    spec.split_plan = Some(ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: true,
        step_bars: 20,
        purge_bars: 0,
        embargo_bars: 0,
    });
    spec.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::Tpe,
        seed: 41,
        budget: 5,
        max_training_drawdown_bps: None,
        fold_local_search: true,
        neighborhood_radius: None,
        continuous_parameter_space: vec![ExperimentParameterRange {
            name: "risk_scale".into(),
            min: 1,
            max: 8,
            step: 1,
        }],
        early_pruning: None,
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
    });
    let identity = StrategyArtifactIdentity {
        strategy_id: spec.target_strategy_id.clone(),
        strategy_version: "no-op-v1".into(),
        source_sha256: "f".repeat(64),
    };
    let mut observed_parameters = Vec::new();
    let result = spec
        .run_with_parameter_factory(
            &identity,
            &mut |_candidate, parameters| {
                observed_parameters.push(parameters.clone());
                Ok(Box::new(NoopStrategy) as Box<dyn Strategy>)
            },
            &research("multi-market-adaptive-search"),
        )
        .expect("each fold runs its own training-only adaptive search and OOS check");

    let search = result.search.as_ref().expect("search trace is included");
    let oos = result
        .out_of_sample
        .as_ref()
        .expect("OOS report is included");
    assert_eq!(result.total_candidates, 5);
    assert_eq!(oos.folds.len(), 3);
    assert_eq!(search.fold_searches.len(), oos.folds.len());
    assert!(observed_parameters.len() > 15); // Training proposals plus one selected OOS run per fold.
    for (index, fold) in search.fold_searches.iter().enumerate() {
        assert_eq!(fold.fold, index + 1);
        assert_eq!(fold.trials.len(), 5);
        assert_eq!(fold.trials.last().unwrap().proposal, "tpe_continuous");
        assert!(fold.trials.iter().all(|trial| {
            trial.parameters["risk_scale"] >= 1
                && trial.parameters["risk_scale"] <= 8
                && trial.training_score_bps.is_some()
        }));
        if index > 0 {
            assert_ne!(fold.seed, search.fold_searches[index - 1].seed);
        }
    }
    assert_eq!(oos.folds[0].train_range, [0, 20]);
    assert_eq!(oos.folds[0].test_range, [20, 30]);
}

#[test]
fn multi_market_adaptive_search_requires_split_and_rejects_unsupported_report_options() {
    let root = scratch("multi-market-adaptive-search-validation");
    let mut spec = experiment(&root);
    spec.strategy_kinds.clear();
    spec.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::TpeKde,
        seed: 1,
        budget: 2,
        max_training_drawdown_bps: None,
        fold_local_search: false,
        neighborhood_radius: None,
        continuous_parameter_space: vec![ExperimentParameterRange {
            name: "risk_scale".into(),
            min: 1,
            max: 2,
            step: 1,
        }],
        early_pruning: None,
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
    });
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
    spec.split_plan = Some(ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: false,
        step_bars: 0,
        purge_bars: 0,
        embargo_bars: 0,
    });
    spec.search.as_mut().unwrap().early_pruning = Some(qx_app::ExperimentEarlyPruningPlan {
        min_training_bars: 10,
    });
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}
