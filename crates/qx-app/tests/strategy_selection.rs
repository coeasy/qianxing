mod common;

use common::{experiment_spec, scratch, write_frame};
use qx_app::{
    run_experiment, AppErrorCategory, BacktestFillModel, BacktestSpec, CallerCapability,
    ExecutionCostSensitivityScenario, ExperimentSearchAlgorithm, ExperimentSearchPlan,
    ExperimentSplitPlan, RunContext, StrategyArtifactIdentity,
};
use qx_core::{Quantity, Side, SCALE};
use qx_strategy::{
    MarketEvent, Strategy, StrategyContext, StrategyDecision, StrategyOrderIntent,
    STRATEGY_API_VERSION,
};
use qx_xingban::ExecutionCostRules;
use std::path::PathBuf;

struct ParameterizedBuy {
    quantity_raw: i128,
    submitted: bool,
}

impl Strategy for ParameterizedBuy {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let intents = if !self.submitted {
            self.submitted = true;
            vec![StrategyOrderIntent {
                intent_id: 1,
                instrument: event
                    .instrument()
                    .cloned()
                    .ok_or_else(|| "event instrument missing".to_string())?,
                side: Side::Buy,
                qty: Quantity::from_raw(self.quantity_raw),
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

fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

#[test]
fn strategy_selection_experiment_runs_classic_strategies_in_the_shared_rust_kernel() {
    let root = scratch("classic-strategy-selection");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 90);
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
    let mut spec = experiment_spec(
        "classic-strategy-selection",
        &bars,
        &root.join("out"),
        vec![2],
    );
    spec.parameter_space.clear();
    spec.strategy_kinds = kinds.to_vec();
    let result = run_experiment(&spec, &research("classic-strategy-selection"))
        .expect("十种经典策略都应走共享 Rust 回测用例");
    assert_eq!(result.total_candidates, 10);
    assert_eq!(result.succeeded_candidates, 10, "{:?}", result.candidates);
    assert!(result
        .candidates
        .iter()
        .all(|candidate| candidate.error.is_none()));
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
            "sma_cross",
        ],
        "候选顺序按策略名稳定排序"
    );
    assert_eq!(result.comparison.as_ref().unwrap().runs.len(), 10);
    assert!(PathBuf::from(&result.artifact_path).is_file());

    let mut reordered = spec.clone();
    reordered.strategy_kinds.reverse();
    assert_eq!(
        run_experiment(&reordered, &research("classic-strategy-selection"))
            .expect("排列不同的同一策略集合应幂等"),
        result,
        "策略选择集合按 kind 规范化后绑定同一实验身份"
    );
    let mut duplicate = spec.clone();
    duplicate.strategy_kinds.push(kinds[0]);
    assert_eq!(
        duplicate.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
    let mut multi_leg = spec.clone();
    multi_leg.strategy_kinds = vec![qx_strategy::BuiltinStrategyKind::PairsArbitrage];
    assert_eq!(
        multi_leg.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
    let mut oos_selection = spec;
    oos_selection.experiment_id = "classic-strategy-selection-oos".into();
    oos_selection.strategy_kinds = kinds[..3].to_vec();
    oos_selection.split_plan = Some(ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: true,
        step_bars: 10,
        purge_bars: 0,
        embargo_bars: 0,
    });
    let oos = run_experiment(&oos_selection, &research("classic-strategy-selection-oos"))
        .expect("策略 kind 应参与同一共享样本外选优路径");
    assert_eq!(oos.succeeded_candidates, 3);
    let folds = &oos.out_of_sample.expect("requested OOS report").folds;
    assert_eq!(folds.len(), 7);
    for fold in folds {
        let selected = &oos.candidates[fold.selected_ordinal - 1];
        assert_eq!(fold.selected_strategy_kind, selected.strategy_kind);
        assert!(fold.selected_strategy_kind.is_some());
    }
}

#[test]
fn caller_strategy_parameter_experiment_creates_a_fresh_strategy_per_candidate() {
    let root = scratch("caller-strategy-parameter-experiment");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let mut spec = experiment_spec(
        "caller-strategy-parameter-experiment",
        &bars,
        &root.join("out"),
        vec![2],
    );
    spec.parameter_space[0].name = "quantity_raw".into();
    spec.parameter_space[0].values = vec![SCALE, 2 * SCALE];
    let identity = StrategyArtifactIdentity {
        strategy_id: spec.base.strategy.strategy_id.clone(),
        strategy_version: "python-parameterized-v1".into(),
        source_sha256: "a".repeat(64),
    };
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut factory = {
        let seen = seen.clone();
        move |candidate: &BacktestSpec| {
            let quantity_raw = candidate.strategy.quantity_raw;
            seen.lock().unwrap().push(quantity_raw);
            Ok(Box::new(ParameterizedBuy {
                quantity_raw,
                submitted: false,
            }) as Box<dyn Strategy>)
        }
    };
    let result = spec
        .run_with_strategy_factory(
            &identity,
            &mut factory,
            &research("caller-strategy-parameter-experiment"),
        )
        .expect("custom strategies run through every parameter candidate");
    assert_eq!(*seen.lock().unwrap(), vec![SCALE, 2 * SCALE]);
    assert_eq!(result.total_candidates, 2);
    assert_eq!(result.succeeded_candidates, 2, "{:?}", result.candidates);
    assert!(result.candidates.iter().all(|candidate| candidate
        .outcome
        .as_ref()
        .is_some_and(|outcome| outcome.fills > 0)));
    assert_ne!(
        result.candidates[0].outcome.as_ref().unwrap().result_hash,
        result.candidates[1].outcome.as_ref().unwrap().result_hash,
        "parameter values must bind each candidate run identity"
    );

    let mut changed = identity;
    changed.source_sha256 = "b".repeat(64);
    let mut fresh_factory = |_: &BacktestSpec| {
        Ok(Box::new(ParameterizedBuy {
            quantity_raw: SCALE,
            submitted: false,
        }) as Box<dyn Strategy>)
    };
    let conflict = spec
        .run_with_strategy_factory(
            &changed,
            &mut fresh_factory,
            &research("caller-strategy-parameter-experiment"),
        )
        .expect_err("changing strategy source cannot reuse the experiment identity");
    assert_eq!(conflict.category(), AppErrorCategory::Conflict);
}

#[test]
fn global_adaptive_search_resumes_only_verified_candidate_checkpoints() {
    let root = scratch("global-search-checkpoint-recovery");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let mut spec = experiment_spec(
        "global-search-checkpoint-recovery",
        &bars,
        &root.join("out"),
        vec![SCALE, 2 * SCALE],
    );
    spec.parameter_space[0].name = "quantity_raw".into();
    spec.split_plan = Some(ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: false,
        step_bars: 0,
        purge_bars: 0,
        embargo_bars: 0,
    });
    spec.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::Random,
        seed: 20261022,
        budget: 2,
        max_training_drawdown_bps: None,
        fold_local_search: false,
        continuous_parameter_space: Vec::new(),
        early_pruning: None,
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
    });
    spec.cost_sensitivity = vec![ExecutionCostSensitivityScenario {
        name: "checkpoint-stress".into(),
        execution_cost_rules: Some(ExecutionCostRules {
            name: "checkpoint-stress-rules".into(),
            maker_bp: 1,
            taker_bp: 2,
            latency_base_ns: 0,
            latency_insert_ns: 0,
        }),
        ashare_rules: None,
        fill_model: BacktestFillModel::BestPrice,
        max_training_return_degradation_bps: None,
        max_neighborhood_training_return_degradation_bps: None,
    }];
    let identity = StrategyArtifactIdentity {
        strategy_id: spec.base.strategy.strategy_id.clone(),
        strategy_version: "checkpoint-test-v1".into(),
        source_sha256: "c".repeat(64),
    };
    let factory_calls = std::sync::Arc::new(std::sync::Mutex::new(0));
    let mut factory = {
        let factory_calls = factory_calls.clone();
        move |candidate: &BacktestSpec| {
            *factory_calls.lock().unwrap() += 1;
            Ok(Box::new(ParameterizedBuy {
                quantity_raw: candidate.strategy.quantity_raw,
                submitted: false,
            }) as Box<dyn Strategy>)
        }
    };
    let first = spec
        .run_with_strategy_factory(
            &identity,
            &mut factory,
            &research("global-search-checkpoint-recovery"),
        )
        .expect("global adaptive search runs each candidate once");
    assert_eq!(
        *factory_calls.lock().unwrap(),
        3,
        "two search candidates and one selected cost scenario run"
    );
    let checkpoint_path =
        root.join("out/global-search-checkpoint-recovery/search-trial-0001.checkpoint.json");
    let original_checkpoint = std::fs::read_to_string(&checkpoint_path).unwrap();
    std::fs::remove_file(&first.artifact_path).unwrap();

    let mut invalid_checkpoint: serde_json::Value =
        serde_json::from_str(&original_checkpoint).unwrap();
    invalid_checkpoint["training_score_bps"] =
        serde_json::json!(invalid_checkpoint["training_score_bps"].as_i64().unwrap() + 1);
    std::fs::write(&checkpoint_path, invalid_checkpoint.to_string()).unwrap();
    let error = spec
        .run_with_strategy_factory(
            &identity,
            &mut factory,
            &research("global-search-checkpoint-recovery"),
        )
        .expect_err("metric drift must invalidate a global candidate checkpoint");
    assert_eq!(error.category(), AppErrorCategory::StorageFailure);

    std::fs::write(&checkpoint_path, &original_checkpoint).unwrap();
    let event_trace = first.candidates[0]
        .outcome
        .as_ref()
        .unwrap()
        .artifacts
        .event_trace
        .as_ref()
        .unwrap();
    let original_trace = std::fs::read_to_string(event_trace).unwrap();
    std::fs::write(event_trace, format!("{original_trace}\n")).unwrap();
    let error = spec
        .run_with_strategy_factory(
            &identity,
            &mut factory,
            &research("global-search-checkpoint-recovery"),
        )
        .expect_err("changed event sidecar bytes must invalidate a global checkpoint");
    assert_eq!(error.category(), AppErrorCategory::StorageFailure);

    std::fs::write(event_trace, original_trace).unwrap();
    let cost_checkpoint_path = root
        .join("out/global-search-checkpoint-recovery/cost-sensitivity/scenario-01.checkpoint.json");
    let original_cost_checkpoint = std::fs::read_to_string(&cost_checkpoint_path).unwrap();
    let mut invalid_cost_checkpoint: serde_json::Value =
        serde_json::from_str(&original_cost_checkpoint).unwrap();
    invalid_cost_checkpoint["scenario_name"] = serde_json::json!("wrong-scenario");
    std::fs::write(&cost_checkpoint_path, invalid_cost_checkpoint.to_string()).unwrap();
    let error = spec
        .run_with_strategy_factory(
            &identity,
            &mut factory,
            &research("global-search-checkpoint-recovery"),
        )
        .expect_err("a checkpoint from another cost scenario must be refused");
    assert_eq!(error.category(), AppErrorCategory::Conflict);
    std::fs::write(&cost_checkpoint_path, &original_cost_checkpoint).unwrap();

    let resumed = spec
        .run_with_strategy_factory(
            &identity,
            &mut factory,
            &research("global-search-checkpoint-recovery"),
        )
        .expect("verified global candidate checkpoints resume");
    assert_eq!(
        *factory_calls.lock().unwrap(),
        3,
        "verified candidate and cost-scenario strategies are not rebuilt"
    );
    assert_eq!(first.search, resumed.search);
    assert_eq!(first.candidates, resumed.candidates);
    assert_eq!(first.cost_sensitivity, resumed.cost_sensitivity);
    assert!(root
        .join("out/global-search-checkpoint-recovery/cost-sensitivity/scenario-01.checkpoint.json")
        .is_file());
}

#[test]
fn failed_search_candidate_and_cost_scenario_are_retried_on_resume() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    let root = scratch("global-search-retry-failures");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let mut spec = experiment_spec(
        "global-search-retry-failures",
        &bars,
        &root.join("out"),
        vec![SCALE, 2 * SCALE],
    );
    spec.parameter_space[0].name = "quantity_raw".into();
    spec.split_plan = Some(ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: false,
        step_bars: 0,
        purge_bars: 0,
        embargo_bars: 0,
    });
    spec.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::Random,
        seed: 20261022,
        budget: 2,
        max_training_drawdown_bps: None,
        fold_local_search: false,
        continuous_parameter_space: Vec::new(),
        early_pruning: None,
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
    });
    spec.cost_sensitivity = vec![ExecutionCostSensitivityScenario {
        name: "retry-stress".into(),
        execution_cost_rules: Some(ExecutionCostRules {
            name: "retry-stress-rules".into(),
            maker_bp: 1,
            taker_bp: 2,
            latency_base_ns: 0,
            latency_insert_ns: 0,
        }),
        ashare_rules: None,
        fill_model: BacktestFillModel::BestPrice,
        max_training_return_degradation_bps: None,
        max_neighborhood_training_return_degradation_bps: Some(10_000),
    }];
    let identity = StrategyArtifactIdentity {
        strategy_id: spec.base.strategy.strategy_id.clone(),
        strategy_version: "retry-test-v1".into(),
        source_sha256: "d".repeat(64),
    };
    let candidate_failure_once = Arc::new(AtomicBool::new(true));
    let cost_failure_once = Arc::new(AtomicBool::new(true));
    let neighbor_failure_once = Arc::new(AtomicBool::new(true));
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let mut factory = {
        let candidate_failure_once = candidate_failure_once.clone();
        let cost_failure_once = cost_failure_once.clone();
        let neighbor_failure_once = neighbor_failure_once.clone();
        let factory_calls = factory_calls.clone();
        move |candidate: &BacktestSpec| {
            factory_calls.fetch_add(1, Ordering::SeqCst);
            if candidate.run_id.ends_with("-0001")
                && candidate_failure_once.swap(false, Ordering::SeqCst)
            {
                return Err("transient candidate worker startup failure".into());
            }
            if candidate.run_id.ends_with("-cost-01")
                && cost_failure_once.swap(false, Ordering::SeqCst)
            {
                return Err("transient cost worker startup failure".into());
            }
            if candidate.run_id.contains("-cost-01-neighbor-")
                && neighbor_failure_once.swap(false, Ordering::SeqCst)
            {
                return Err("transient neighborhood cost worker startup failure".into());
            }
            Ok(Box::new(ParameterizedBuy {
                quantity_raw: candidate.strategy.quantity_raw,
                submitted: false,
            }) as Box<dyn Strategy>)
        }
    };

    let first = spec
        .run_with_strategy_factory(
            &identity,
            &mut factory,
            &research("global-search-retry-failures"),
        )
        .expect("transient candidate and cost failures are recorded in the experiment result");
    assert!(first.candidates[0].outcome.is_none());
    assert!(first.candidates[0].error.is_some());
    assert!(first.candidates[1].outcome.is_some());
    assert!(first.cost_sensitivity[0].outcome.is_none());
    assert!(first.cost_sensitivity[0].error.is_some());
    assert!(first.cost_sensitivity[0]
        .neighborhood_cost_robustness
        .as_ref()
        .unwrap()
        .candidates[0]
        .error
        .is_some());
    assert!(!root
        .join("out/global-search-retry-failures/search-trial-0001.checkpoint.json")
        .exists());
    assert!(!root
        .join("out/global-search-retry-failures/cost-sensitivity/scenario-01.checkpoint.json")
        .exists());
    assert!(root
        .join("out/global-search-retry-failures/search-trial-0002.checkpoint.json")
        .is_file());
    assert_eq!(factory_calls.load(Ordering::SeqCst), 3);

    let resumed = spec
        .run_with_strategy_factory(
            &identity,
            &mut factory,
            &research("global-search-retry-failures"),
        )
        .expect("resume retries only the candidate and cost scenario without verified checkpoints");
    assert!(resumed.candidates[0].outcome.is_some());
    assert!(resumed.candidates[0].error.is_none());
    assert!(resumed.candidates[1].outcome.is_some());
    assert!(resumed.cost_sensitivity[0].outcome.is_some());
    assert!(resumed.cost_sensitivity[0].error.is_none());
    assert!(resumed.cost_sensitivity[0]
        .neighborhood_cost_robustness
        .as_ref()
        .unwrap()
        .candidates
        .iter()
        .any(|candidate| candidate.error.is_some()));
    assert!(root
        .join("out/global-search-retry-failures/search-trial-0001.checkpoint.json")
        .is_file());
    assert!(root
        .join("out/global-search-retry-failures/cost-sensitivity/scenario-01.checkpoint.json")
        .is_file());
    let neighborhood_checkpoint = root.join(
        "out/global-search-retry-failures/cost-sensitivity/scenario-01/neighbors/candidate-0001.checkpoint.json",
    );
    assert!(!neighborhood_checkpoint.exists());

    let resumed_again = spec
        .run_with_strategy_factory(
            &identity,
            &mut factory,
            &research("global-search-retry-failures"),
        )
        .expect(
            "a failed neighbor stress is retried while verified neighbor successes remain reusable",
        );
    let neighborhood = resumed_again.cost_sensitivity[0]
        .neighborhood_cost_robustness
        .as_ref()
        .unwrap();
    assert!(neighborhood.passed);
    assert!(neighborhood_checkpoint.is_file());
    assert_eq!(factory_calls.load(Ordering::SeqCst), 7);
}
