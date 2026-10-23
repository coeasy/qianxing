//! 实验切分与基准的**行为**契约（V13 R28）。
//!
//! 静态判据只能证明"字段在盘、生成器在盘"，证明不了"训练窗选参、测试窗出分"这件事真的
//! 发生了。这一族把四个最贵的错各钉一条：
//!
//! * 同一份切分计划两次运行必须给出同一个 OOS 分数（**可复算**——否则走查只是噪声）；
//! * 滚动走查真的按 `step_bars` 折数滚动（折数是样本与步长的函数，不是常量）；
//! * 基准用的是**收盘价序列**而不是策略权益曲线（用错序列会给出一个永远等于候选的数）；
//! * 不写这两格的 spec **逐字节**保持原样（纯加法升级不许让旧产物读不回来）。

use qx_app::{
    run_experiment, AppErrorCategory, BacktestFillModel, CallerCapability,
    ExecutionCostSensitivityScenario, ExperimentEarlyPruningPlan, ExperimentParameterRange,
    ExperimentParameterSpace, ExperimentSearchAlgorithm, ExperimentSearchPlan, ExperimentSplitPlan,
    RunContext, RunExperimentSpec,
};
use qx_core::SCALE;
use qx_xingban::{AshareRuleConfig, ExecutionCostRules};

mod common;

use common::{backtest_spec, scratch, write_frame};

fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

/// 从 `<run_id>.equity.csv` 读回权益序列——判据读的是**产物**，与实现读的是同一份。
fn equity_series(path: &str) -> Vec<i128> {
    let text = std::fs::read_to_string(path).expect("读权益产物");
    text.lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|row| {
            row.split(',')
                .nth(1)
                .expect("两列")
                .trim()
                .parse::<i128>()
                .expect("定点整数")
        })
        .collect()
}

/// 窗口 `[start, end)` 的收益（基点），以窗口首点为基准——与实现独立算一遍。
fn window_bps(values: &[i128], start: usize, end: usize) -> i32 {
    let first = values[start];
    ((values[end - 1] - first) * 10_000 / first) as i32
}

fn window_drawdown_bps(values: &[i128], start: usize, end: usize) -> u32 {
    let mut peak = values[start];
    let mut worst = 0_i128;
    for value in &values[start..end] {
        peak = peak.max(*value);
        if peak > 0 {
            worst = worst.max((peak - *value) * 10_000 / peak);
        }
    }
    worst as u32
}

fn spec_with(
    experiment_id: &str,
    bars: &str,
    root: &std::path::Path,
    split_plan: Option<ExperimentSplitPlan>,
    baselines: Vec<String>,
) -> RunExperimentSpec {
    RunExperimentSpec {
        schema_version: 1,
        experiment_id: experiment_id.to_string(),
        base: backtest_spec("unused-base-run", bars, &root.join("out")),
        parameter_space: vec![ExperimentParameterSpace {
            name: "fast_window".into(),
            values: vec![1, 2],
        }],
        strategy_kinds: Vec::new(),
        split_plan,
        baselines,
        search: None,
        cost_sensitivity: Vec::new(),
    }
}

fn write_falling_frame(root: &std::path::Path, name: &str, instrument: &str) -> String {
    let mut prices = vec![100_i128; 22];
    prices.extend([101, 110, 60, 50, 45, 42, 40]);
    prices.resize(40, 40);
    let scale = SCALE;
    let frame = serde_json::json!({
        "schema_version": 1,
        "instrument": instrument,
        "source": "qx-app-test-early-pruning-v1",
        "ts": (0..prices.len()).map(|index| 1_000 + index as u64 * 1_000).collect::<Vec<_>>(),
        "open_raw": prices.iter().map(|price| price * scale).collect::<Vec<_>>(),
        "high_raw": prices.iter().map(|price| (price + 1) * scale).collect::<Vec<_>>(),
        "low_raw": prices.iter().map(|price| (price - 1) * scale).collect::<Vec<_>>(),
        "close_raw": prices.iter().map(|price| price * scale).collect::<Vec<_>>(),
        "volume_raw": vec![scale; prices.len()],
    });
    let path = root.join(name);
    std::fs::write(&path, frame.to_string()).expect("write falling BarFrame");
    path.to_string_lossy().into_owned()
}

#[test]
fn adaptive_search_early_pruning_records_drawdown_without_half_built_outcome() {
    let root = scratch("adaptive-search-early-pruning");
    let bars = write_falling_frame(&root, "falling-bars.json", "BTCUSDT.BINANCE");
    let split = ExperimentSplitPlan {
        train_bars: 30,
        test_bars: 10,
        walk_forward: false,
        step_bars: 0,
        purge_bars: 0,
        embargo_bars: 0,
    };
    let mut spec = spec_with("early-pruned", &bars, &root, Some(split), Vec::new());
    spec.base.strategy.fast_window = 1;
    spec.parameter_space[0].name = "fast_window".into();
    spec.parameter_space[0].values = vec![1];
    spec.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::Random,
        seed: 7,
        budget: 1,
        max_training_drawdown_bps: Some(0),
        fold_local_search: false,
        continuous_parameter_space: Vec::new(),
        early_pruning: Some(ExperimentEarlyPruningPlan {
            min_training_bars: 25,
        }),
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
    });

    let result =
        run_experiment(&spec, &research("early-pruned")).expect("剪枝候选仍形成完整实验报告");
    assert_eq!(result.completed_candidates, 1);
    assert_eq!(result.succeeded_candidates, 0);
    assert_eq!(result.failed_candidates, 0);
    assert_eq!(result.pruned_candidates, 1);
    assert!(result.out_of_sample.is_none());
    let pruning = result.candidates[0].pruning.as_ref().expect("剪枝证据");
    assert!(pruning.processed_bars >= 25);
    assert!(pruning.processed_bars <= 30, "不得观察训练窗之后的数据");
    assert!(pruning.observed_max_drawdown_bps > pruning.max_training_drawdown_bps);
    assert!(result.candidates[0].outcome.is_none());
    assert!(result.candidates[0].error.is_none());
    assert_eq!(
        result.search.as_ref().unwrap().early_pruning,
        spec.search.as_ref().unwrap().early_pruning
    );
}

#[test]
fn fold_local_all_pruned_candidates_keep_fold_diagnostics_without_fake_oos_score() {
    let root = scratch("fold-local-all-pruned");
    let bars = write_falling_frame(&root, "falling-bars.json", "BTCUSDT.BINANCE");
    let mut spec = spec_with(
        "fold-local-pruned",
        &bars,
        &root,
        Some(ExperimentSplitPlan {
            train_bars: 30,
            test_bars: 10,
            walk_forward: false,
            step_bars: 0,
            purge_bars: 0,
            embargo_bars: 0,
        }),
        Vec::new(),
    );
    spec.base.strategy.fast_window = 1;
    spec.parameter_space[0].values = vec![1];
    spec.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::Random,
        seed: 13,
        budget: 1,
        max_training_drawdown_bps: Some(0),
        fold_local_search: true,
        continuous_parameter_space: Vec::new(),
        early_pruning: Some(ExperimentEarlyPruningPlan {
            min_training_bars: 25,
        }),
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
    });

    let result =
        run_experiment(&spec, &research("fold-local-pruned")).expect("全剪枝仍保存实验与逐折诊断");
    assert!(
        result.out_of_sample.is_none(),
        "无选中候选就不应伪造 OOS 分数"
    );
    assert_eq!(result.pruned_candidates, 1);
    let fold = &result.search.as_ref().unwrap().fold_searches[0];
    assert_eq!(fold.selected_ordinal, None);
    assert_eq!(fold.selected_run_manifest, None);
    assert_eq!(fold.candidates.len(), 1);
    assert!(fold.candidates[0].pruning.is_some());
}

#[test]
fn a_split_plan_yields_a_reproducible_out_of_sample_score() {
    let root = scratch("split-reproducible");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let plan = ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: false,
        step_bars: 0,
        purge_bars: 0,
        embargo_bars: 0,
    };
    let spec = spec_with("split-repro", &bars, &root, Some(plan), Vec::new());
    let first = run_experiment(&spec, &research("split-repro")).expect("带切分的实验");
    let second = run_experiment(&spec, &research("split-repro")).expect("幂等重跑");
    assert_eq!(first, second, "同一切分计划两次运行必须给出同一份报告");

    let report = first.out_of_sample.expect("写了切分计划就必须有 OOS 报告");
    assert_eq!(report.folds.len(), 1, "未打开走查时只有一段切分");
    let robustness = report
        .cross_fold_robustness
        .as_ref()
        .expect("OOS 报告给出逐折稳健性摘要");
    assert_eq!(robustness.folds_evaluated, 1);
    assert_eq!(
        robustness.positive_training_folds,
        usize::from(report.folds[0].in_sample_return_bps > 0)
    );
    assert_eq!(
        robustness.minimum_training_return_bps,
        report.folds[0].in_sample_return_bps
    );
    assert_eq!(
        robustness.minimum_out_of_sample_return_bps,
        report.folds[0].out_of_sample_return_bps
    );
    assert_eq!(report.folds[0].train_range, [0, 20]);
    assert_eq!(report.folds[0].test_range, [20, 30]);
    assert_eq!(
        report.in_sample_return_bps - report.out_of_sample_return_bps,
        report.degradation_bps,
        "退化度就是样本内减样本外，不是另算的一个数"
    );

    // 报告里的样本外分数必须等于**被选中那个候选**在测试窗上的真实收益。
    let selected = report.folds[0].selected_ordinal;
    let outcome = first
        .candidates
        .iter()
        .find(|candidate| candidate.ordinal == selected)
        .and_then(|candidate| candidate.outcome.as_ref())
        .expect("被选中的候选必须有产物");
    let values = equity_series(&outcome.artifacts.equity);
    assert_eq!(values.len(), 60, "权益曲线逐根对应");
    assert_eq!(
        report.folds[0].out_of_sample_return_bps,
        window_bps(&values, 20, 30),
        "样本外分数必须取自测试窗，而不是整段"
    );
    assert_eq!(
        report.folds[0].in_sample_return_bps,
        window_bps(&values, 0, 20),
        "样本内分数必须取自训练窗"
    );
}

#[test]
fn existing_experiment_report_is_bound_to_the_actual_barframe() {
    let root = scratch("experiment-input-identity");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let spec = spec_with("input-identity", &bars, &root, None, Vec::new());
    let first = run_experiment(&spec, &research("input-identity")).expect("initial experiment");
    let original_identity = first
        .input_data_fingerprint
        .as_deref()
        .expect("data identity");
    assert!(original_identity.starts_with("barframe:"));

    // Reports from before this field existed remain reusable when a successful candidate
    // contains the same canonical dataset identity.
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&first.artifact_path).expect("read report"))
            .expect("report JSON");
    legacy
        .as_object_mut()
        .unwrap()
        .remove("input_data_fingerprint");
    std::fs::write(&first.artifact_path, legacy.to_string()).expect("write legacy report");
    let reused = run_experiment(&spec, &research("input-identity")).expect("legacy reuse");
    assert_eq!(reused.input_data_fingerprint, None);

    let mut changed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bars).unwrap()).unwrap();
    changed["volume_raw"][1] = serde_json::json!(SCALE + 1);
    std::fs::write(&bars, changed.to_string()).expect("change valid BarFrame data");
    let error = run_experiment(&spec, &research("input-identity"))
        .expect_err("same experiment id cannot return stale results for changed bars");
    assert_eq!(error.category(), AppErrorCategory::Conflict);
}

#[test]
fn fold_local_search_resumes_only_verified_candidate_checkpoints() {
    let root = scratch("fold-checkpoint-recovery");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let mut spec = spec_with(
        "fold-checkpoint-recovery",
        &bars,
        &root,
        Some(ExperimentSplitPlan {
            train_bars: 20,
            test_bars: 10,
            walk_forward: false,
            step_bars: 0,
            purge_bars: 0,
            embargo_bars: 0,
        }),
        Vec::new(),
    );
    spec.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::Random,
        seed: 7,
        budget: 2,
        max_training_drawdown_bps: None,
        fold_local_search: true,
        continuous_parameter_space: Vec::new(),
        early_pruning: None,
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
    });
    let first = run_experiment(&spec, &research("fold-checkpoint-recovery")).unwrap();
    let checkpoint =
        root.join("out/fold-checkpoint-recovery/fold-local/fold-0001/trial-0001.checkpoint.json");
    let original = std::fs::read_to_string(&checkpoint).expect("successful candidate checkpoint");
    std::fs::remove_file(&first.artifact_path).expect("simulate report not published after crash");
    let mut corrupted: serde_json::Value = serde_json::from_str(&original).unwrap();
    corrupted["training_score_bps"] =
        serde_json::json!(corrupted["training_score_bps"].as_i64().unwrap() + 1);
    std::fs::write(&checkpoint, corrupted.to_string()).unwrap();
    let error = run_experiment(&spec, &research("fold-checkpoint-recovery"))
        .expect_err("checkpoint metric drift must not be reused");
    assert_eq!(error.category(), AppErrorCategory::StorageFailure);

    std::fs::write(&checkpoint, original).unwrap();
    let event_trace = first.search.as_ref().unwrap().fold_searches[0].candidates[0]
        .outcome
        .as_ref()
        .unwrap()
        .artifacts
        .event_trace
        .as_ref()
        .unwrap();
    let trace_body = std::fs::read_to_string(event_trace).unwrap();
    std::fs::write(event_trace, format!("{trace_body}\n")).unwrap();
    let error = run_experiment(&spec, &research("fold-checkpoint-recovery"))
        .expect_err("changed sidecar bytes must invalidate a checkpoint");
    assert_eq!(error.category(), AppErrorCategory::StorageFailure);
    std::fs::write(event_trace, trace_body).unwrap();
    let resumed = run_experiment(&spec, &research("fold-checkpoint-recovery"))
        .expect("verified candidate checkpoints resume safely");
    assert_eq!(first.out_of_sample, resumed.out_of_sample);
    assert_eq!(first.search, resumed.search);
}

#[test]
fn purged_and_embargoed_intervals_are_present_in_the_saved_experiment_report() {
    let root = scratch("purge-embargo-report");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let plan = ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: true,
        step_bars: 15,
        purge_bars: 2,
        embargo_bars: 5,
    };
    let spec = spec_with("purge-embargo", &bars, &root, Some(plan), Vec::new());
    let result =
        run_experiment(&spec, &research("purge-embargo")).expect("带 purge 与 embargo 的走查实验");
    let report = result.out_of_sample.expect("OOS 报告");
    assert_eq!(report.folds[0].train_range, [0, 20]);
    assert_eq!(report.folds[0].purge_range, Some([20, 22]));
    assert_eq!(report.folds[0].test_range, [22, 32]);
    assert_eq!(report.folds[0].embargo_range, Some([32, 37]));
    assert_eq!(report.folds[1].train_range, [15, 35]);
    assert_eq!(report.folds[1].purge_range, Some([35, 37]));
    assert_eq!(report.folds[1].test_range, [37, 47]);

    let artifact: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&result.artifact_path).expect("读回实验结果产物"))
            .expect("实验结果 JSON");
    assert_eq!(
        artifact["out_of_sample"]["folds"][0]["purge_range"],
        serde_json::json!([20, 22])
    );
    assert_eq!(
        artifact["out_of_sample"]["folds"][0]["embargo_range"],
        serde_json::json!([32, 37])
    );
}

#[test]
fn cost_sensitivity_reruns_the_training_selected_candidate_with_declared_costs() {
    let root = scratch("experiment-cost-sensitivity");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let plan = ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: false,
        step_bars: 0,
        purge_bars: 0,
        embargo_bars: 0,
    };
    let mut spec = spec_with("cost-sensitivity", &bars, &root, Some(plan), Vec::new());
    spec.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::Random,
        seed: 42,
        budget: 2,
        max_training_drawdown_bps: Some(10_000),
        fold_local_search: false,
        continuous_parameter_space: Vec::new(),
        early_pruning: None,
        robustness_subwindows: Some(3),
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
    });
    let mut baseline_spec = spec.clone();
    baseline_spec.experiment_id = "cost-sensitivity-robustness-baseline".into();
    baseline_spec.base.output_dir = root.join("baseline").to_string_lossy().into_owned();
    baseline_spec.cost_sensitivity.clear();
    let baseline = run_experiment(
        &baseline_spec,
        &research("cost-sensitivity-robustness-baseline"),
    )
    .expect("baseline search yields the training robust scores used to set the gate");
    let robustness_floor = baseline
        .search
        .as_ref()
        .unwrap()
        .trials
        .iter()
        .filter_map(|trial| trial.training_robustness_score_bps)
        .max()
        .expect("successful candidates have robustness scores");
    spec.search
        .as_mut()
        .unwrap()
        .minimum_training_robustness_score_bps = Some(robustness_floor);
    spec.cost_sensitivity = vec![ExecutionCostSensitivityScenario {
        name: "high-taker-one-bp".into(),
        execution_cost_rules: Some(ExecutionCostRules {
            name: "test-stress".into(),
            maker_bp: 10,
            taker_bp: 100,
            latency_base_ns: 0,
            latency_insert_ns: 0,
        }),
        ashare_rules: None,
        fill_model: BacktestFillModel::BestPrice,
        max_training_return_degradation_bps: Some(0),
        max_neighborhood_training_return_degradation_bps: Some(10_000),
    }];

    let result = run_experiment(&spec, &research("cost-sensitivity"))
        .expect("explicit cost sensitivity should run through the shared backtest path");
    let selected = result
        .search
        .as_ref()
        .unwrap()
        .trials
        .iter()
        .filter(|trial| {
            trial
                .training_robustness_score_bps
                .is_some_and(|score| score >= robustness_floor)
        })
        .max_by(|left, right| {
            left.selection_score_bps()
                .cmp(&right.selection_score_bps())
                .then_with(|| {
                    right
                        .training_max_drawdown_bps
                        .cmp(&left.training_max_drawdown_bps)
                })
                .then_with(|| right.ordinal.cmp(&left.ordinal))
        })
        .unwrap()
        .ordinal;
    assert_eq!(
        result
            .search
            .as_ref()
            .unwrap()
            .minimum_training_robustness_score_bps,
        Some(robustness_floor)
    );
    assert_eq!(result.cost_sensitivity.len(), 1);
    let stress = &result.cost_sensitivity[0];
    assert_eq!(stress.selected_ordinal, selected);
    assert!(
        result
            .search
            .as_ref()
            .unwrap()
            .trials
            .iter()
            .find(|trial| trial.ordinal == selected)
            .unwrap()
            .training_max_drawdown_bps
            .unwrap()
            <= 10_000
    );
    let outcome = stress.outcome.as_ref().expect("stress run outcome");
    assert_eq!(outcome.run_id, "cost-sensitivity-cost-01");
    assert!(stress.return_delta_bps.is_some());
    assert!(stress.drawdown_delta_bps.is_some());
    let selected_outcome = result.candidates[selected - 1]
        .outcome
        .as_ref()
        .expect("selected candidate baseline outcome");
    let baseline_equity = equity_series(&selected_outcome.artifacts.equity);
    let stressed_equity = equity_series(&outcome.artifacts.equity);
    assert_eq!(stress.fold_deltas.len(), 1);
    let split_delta = &stress.fold_deltas[0];
    assert_eq!(split_delta.train_range, [0, 20]);
    assert_eq!(split_delta.test_range, [20, 30]);
    assert_eq!(
        split_delta.training_return_delta_bps,
        i64::from(window_bps(&stressed_equity, 0, 20))
            - i64::from(window_bps(&baseline_equity, 0, 20))
    );
    assert_eq!(
        split_delta.out_of_sample_return_delta_bps,
        i64::from(window_bps(&stressed_equity, 20, 30))
            - i64::from(window_bps(&baseline_equity, 20, 30))
    );
    assert_eq!(
        stress.cost_robustness_passed,
        Some(split_delta.training_return_delta_bps >= 0)
    );
    assert_eq!(
        result.search.as_ref().unwrap().cost_robustness_passed,
        stress.cost_robustness_passed
    );
    let neighborhood = stress
        .neighborhood_cost_robustness
        .as_ref()
        .expect("configured neighborhood cost gate");
    assert_eq!(neighborhood.possible_neighbors, 1);
    assert_eq!(neighborhood.evaluated_neighbors, 1);
    assert_eq!(neighborhood.successful_neighbors, 1);
    assert_eq!(neighborhood.passed_neighbors, 1);
    assert!(neighborhood.complete && neighborhood.passed);
    assert!(neighborhood.candidates[0].outcome.is_some());
    assert_eq!(
        result
            .search
            .as_ref()
            .unwrap()
            .neighborhood_cost_robustness_passed,
        Some(true)
    );
    assert!(std::path::Path::new(&outcome.artifacts.run_manifest).is_file());
}

#[test]
fn neighborhood_cost_gate_fails_closed_when_search_budget_misses_neighbors() {
    let root = scratch("incomplete-neighborhood-cost-gate");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let mut spec = spec_with(
        "incomplete-neighborhood-cost-gate",
        &bars,
        &root,
        Some(ExperimentSplitPlan {
            train_bars: 20,
            test_bars: 10,
            walk_forward: false,
            step_bars: 0,
            purge_bars: 0,
            embargo_bars: 0,
        }),
        Vec::new(),
    );
    spec.parameter_space[0].values = vec![1, 2, 3];
    spec.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::Random,
        seed: 19,
        budget: 1,
        max_training_drawdown_bps: None,
        fold_local_search: false,
        continuous_parameter_space: Vec::new(),
        early_pruning: None,
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
    });
    spec.cost_sensitivity = vec![ExecutionCostSensitivityScenario {
        name: "higher-fees".into(),
        execution_cost_rules: Some(ExecutionCostRules {
            name: "stress".into(),
            maker_bp: 10,
            taker_bp: 100,
            latency_base_ns: 0,
            latency_insert_ns: 0,
        }),
        ashare_rules: None,
        fill_model: BacktestFillModel::BestPrice,
        max_training_return_degradation_bps: None,
        max_neighborhood_training_return_degradation_bps: Some(10_000),
    }];

    let result = run_experiment(&spec, &research("incomplete-neighborhood-cost-gate"))
        .expect("the bounded search and center cost scenario run");
    let neighborhood = result.cost_sensitivity[0]
        .neighborhood_cost_robustness
        .as_ref()
        .expect("neighborhood gate is reported");
    assert!(neighborhood.possible_neighbors > 0);
    assert_eq!(neighborhood.evaluated_neighbors, 0);
    assert!(!neighborhood.complete);
    assert!(!neighborhood.passed);
    assert_eq!(
        result
            .search
            .as_ref()
            .unwrap()
            .neighborhood_cost_robustness_passed,
        Some(false)
    );
}

#[test]
fn fold_local_cost_sensitivity_replays_each_fold_selection_and_scores_its_test_window() {
    let root = scratch("fold-local-cost-sensitivity");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let mut spec = spec_with(
        "fold-local-cost-sensitivity",
        &bars,
        &root,
        Some(ExperimentSplitPlan {
            train_bars: 20,
            test_bars: 10,
            walk_forward: true,
            step_bars: 10,
            purge_bars: 0,
            embargo_bars: 0,
        }),
        Vec::new(),
    );
    spec.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::Random,
        seed: 2031,
        budget: 2,
        max_training_drawdown_bps: None,
        fold_local_search: true,
        continuous_parameter_space: Vec::new(),
        early_pruning: None,
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
    });
    spec.cost_sensitivity = vec![ExecutionCostSensitivityScenario {
        name: "higher-fees".into(),
        execution_cost_rules: Some(ExecutionCostRules {
            name: "fold-stress".into(),
            maker_bp: 10,
            taker_bp: 100,
            latency_base_ns: 0,
            latency_insert_ns: 0,
        }),
        ashare_rules: None,
        fill_model: BacktestFillModel::BestPrice,
        max_training_return_degradation_bps: Some(0),
        max_neighborhood_training_return_degradation_bps: Some(10_000),
    }];

    let result = run_experiment(&spec, &research("fold-local-cost-sensitivity"))
        .expect("each fold reruns its selected parameters under the stress costs");
    let fold_searches = &result.search.as_ref().unwrap().fold_searches;
    assert_eq!(fold_searches.len(), 4);
    for (index, fold) in fold_searches.iter().enumerate() {
        assert_eq!(fold.cost_sensitivity.len(), 1);
        let stress = &fold.cost_sensitivity[0];
        assert_eq!(fold.selected_ordinal, Some(stress.selected_ordinal));
        let stressed = stress.outcome.as_ref().expect("fold stress outcome");
        assert!(stressed.fills > 0);
        assert_eq!(
            stressed.run_id,
            format!("fold-local-cost-sensitivity-f{:04}-cost-01", index + 1)
        );
        let selected_ordinal = fold
            .selected_ordinal
            .expect("fold has a selected candidate");
        let baseline = fold.candidates[selected_ordinal - 1]
            .outcome
            .as_ref()
            .expect("selected fold baseline outcome");
        let baseline_equity = equity_series(&baseline.artifacts.equity);
        let stressed_equity = equity_series(&stressed.artifacts.equity);
        let slice_start = fold.train_range[0] as usize;
        let test_start = fold.test_range[0] as usize - slice_start;
        let test_end = fold.test_range[1] as usize - slice_start;
        let train_end = fold.train_range[1] as usize - slice_start;
        let split_delta = &stress.fold_deltas[0];
        assert_eq!(split_delta.train_range, fold.train_range);
        assert_eq!(split_delta.test_range, fold.test_range);
        assert_eq!(
            split_delta.training_return_delta_bps,
            i64::from(window_bps(&stressed_equity, 0, train_end))
                - i64::from(window_bps(&baseline_equity, 0, train_end)),
            "fold cost sensitivity keeps training return change separate"
        );
        assert_eq!(
            split_delta.training_drawdown_delta_bps,
            i64::from(window_drawdown_bps(&stressed_equity, 0, train_end))
                - i64::from(window_drawdown_bps(&baseline_equity, 0, train_end)),
            "fold cost sensitivity keeps training drawdown change separate"
        );
        assert_eq!(
            stress.cost_robustness_passed,
            Some(split_delta.training_return_delta_bps >= 0)
        );
        let neighborhood = stress
            .neighborhood_cost_robustness
            .as_ref()
            .expect("fold neighborhood cost gate");
        assert_eq!(neighborhood.possible_neighbors, 1);
        assert_eq!(neighborhood.evaluated_neighbors, 1);
        assert!(neighborhood.passed);
        assert_eq!(neighborhood.candidates[0].fold_deltas.len(), 1);
        let baseline_return = window_bps(&baseline_equity, test_start, test_end);
        let stressed_return = window_bps(&stressed_equity, test_start, test_end);
        let baseline_drawdown = window_drawdown_bps(&baseline_equity, test_start, test_end);
        let stressed_drawdown = window_drawdown_bps(&stressed_equity, test_start, test_end);
        assert_eq!(
            stress.return_delta_bps,
            Some(i64::from(stressed_return) - i64::from(baseline_return)),
            "fold cost sensitivity reports test-window, not train-window, return change"
        );
        assert_eq!(
            stress.drawdown_delta_bps,
            Some(i64::from(stressed_drawdown) - i64::from(baseline_drawdown)),
            "fold cost sensitivity reports test-window drawdown change"
        );
    }
    assert_eq!(
        result.search.as_ref().unwrap().cost_robustness_passed,
        Some(
            fold_searches
                .iter()
                .all(|fold| fold.cost_sensitivity[0].cost_robustness_passed == Some(true))
        )
    );
    assert_eq!(
        result
            .search
            .as_ref()
            .unwrap()
            .neighborhood_cost_robustness_passed,
        Some(true)
    );

    let experiment_root = std::path::Path::new(&result.artifact_path)
        .parent()
        .expect("experiment artifact has a parent");
    let cost_checkpoint =
        experiment_root.join("fold-local/fold-0001/cost-sensitivity/scenario-01.checkpoint.json");
    let original_checkpoint = std::fs::read_to_string(&cost_checkpoint).unwrap();
    let mut invalid_checkpoint: serde_json::Value =
        serde_json::from_str(&original_checkpoint).unwrap();
    invalid_checkpoint["scenario_name"] = serde_json::json!("wrong-scenario");
    std::fs::write(&cost_checkpoint, invalid_checkpoint.to_string()).unwrap();
    std::fs::remove_file(&result.artifact_path).unwrap();
    let error = run_experiment(&spec, &research("fold-local-cost-sensitivity"))
        .expect_err("fold cost checkpoint from another scenario must be rejected");
    assert_eq!(error.category(), AppErrorCategory::Conflict);

    std::fs::write(&cost_checkpoint, original_checkpoint).unwrap();
    let resumed = run_experiment(&spec, &research("fold-local-cost-sensitivity"))
        .expect("verified fold cost checkpoints resume");
    assert_eq!(
        result.search.as_ref().unwrap().fold_searches,
        resumed.search.as_ref().unwrap().fold_searches
    );
}

#[test]
fn fold_local_run_limit_counts_cost_scenarios_before_candidate_side_effects() {
    let root = scratch("fold-local-cost-budget");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let mut spec = spec_with(
        "fold-local-cost-budget",
        &bars,
        &root,
        Some(ExperimentSplitPlan {
            train_bars: 20,
            test_bars: 2,
            walk_forward: true,
            step_bars: 2,
            purge_bars: 0,
            embargo_bars: 0,
        }),
        Vec::new(),
    );
    spec.parameter_space[0].values = (1..=256).collect();
    spec.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::Random,
        seed: 2032,
        budget: 256,
        max_training_drawdown_bps: None,
        fold_local_search: true,
        continuous_parameter_space: Vec::new(),
        early_pruning: None,
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
    });
    spec.cost_sensitivity = (0..16)
        .map(|index| ExecutionCostSensitivityScenario {
            name: format!("stress-{index:02}"),
            execution_cost_rules: Some(ExecutionCostRules {
                name: format!("stress-{index:02}"),
                maker_bp: 1,
                taker_bp: 2 + index,
                latency_base_ns: 0,
                latency_insert_ns: 0,
            }),
            ashare_rules: None,
            fill_model: BacktestFillModel::BestPrice,
            max_training_return_degradation_bps: Some(0),
            max_neighborhood_training_return_degradation_bps: None,
        })
        .collect();

    let error = run_experiment(&spec, &research("fold-local-cost-budget"))
        .expect_err("search and per-fold stress scenarios together exceed the run limit");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(
        error.message().contains("成本情景"),
        "unexpected early rejection: {}",
        error.message()
    );
    assert!(
        !root.join("out/fold-local-cost-budget").exists(),
        "the resource limit must reject the experiment before candidate runs start"
    );
}

#[test]
fn fold_local_neighborhood_cost_runs_count_against_limit_before_candidate_side_effects() {
    let root = scratch("fold-local-neighborhood-cost-budget");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let mut spec = spec_with(
        "fold-local-neighborhood-cost-budget",
        &bars,
        &root,
        Some(ExperimentSplitPlan {
            train_bars: 20,
            test_bars: 5,
            walk_forward: true,
            step_bars: 5,
            purge_bars: 0,
            embargo_bars: 0,
        }),
        Vec::new(),
    );
    spec.parameter_space[0].values = (1..=256).collect();
    spec.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::Random,
        seed: 2033,
        budget: 256,
        max_training_drawdown_bps: None,
        fold_local_search: true,
        continuous_parameter_space: Vec::new(),
        early_pruning: None,
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
    });
    spec.cost_sensitivity = vec![ExecutionCostSensitivityScenario {
        name: "neighborhood-stress".into(),
        execution_cost_rules: Some(ExecutionCostRules {
            name: "neighborhood-stress".into(),
            maker_bp: 1,
            taker_bp: 2,
            latency_base_ns: 0,
            latency_insert_ns: 0,
        }),
        ashare_rules: None,
        fill_model: BacktestFillModel::BestPrice,
        max_training_return_degradation_bps: None,
        max_neighborhood_training_return_degradation_bps: Some(100),
    }];

    let error = run_experiment(&spec, &research("fold-local-neighborhood-cost-budget"))
        .expect_err("neighbor cost replays must count against the run ceiling");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(error.message().contains("4096"));
    assert!(!root
        .join("out/fold-local-neighborhood-cost-budget")
        .exists());
}

#[test]
fn ashare_cost_sensitivity_replays_with_declared_rules_and_real_fills() {
    let root = scratch("ashare-cost-sensitivity");
    let bars = write_frame(&root, "bars.json", "600000.SSE", 60);
    let plan = ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: false,
        step_bars: 0,
        purge_bars: 0,
        embargo_bars: 0,
    };
    let mut spec = spec_with(
        "ashare-cost-sensitivity",
        &bars,
        &root,
        Some(plan),
        Vec::new(),
    );
    spec.base.instrument = "600000.SSE".into();
    spec.base.settlement_currency = "CNY".into();
    spec.base.strategy.quantity_raw = 100 * SCALE;
    spec.base.ashare_rules = Some(AshareRuleConfig {
        enabled: true,
        t_plus_one: false,
        commission_bp: 1,
        min_commission: 0,
        ..AshareRuleConfig::default()
    });
    spec.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::Random,
        seed: 43,
        budget: 2,
        max_training_drawdown_bps: Some(10_000),
        fold_local_search: false,
        continuous_parameter_space: Vec::new(),
        early_pruning: None,
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
    });
    spec.cost_sensitivity = vec![ExecutionCostSensitivityScenario {
        name: "higher-ashare-fees".into(),
        execution_cost_rules: None,
        ashare_rules: Some(AshareRuleConfig {
            enabled: true,
            t_plus_one: false,
            commission_bp: 100,
            min_commission: 0,
            stamp_duty_bp: 100,
            ..AshareRuleConfig::default()
        }),
        fill_model: BacktestFillModel::NextBarOpen,
        max_training_return_degradation_bps: Some(0),
        max_neighborhood_training_return_degradation_bps: None,
    }];

    let result = run_experiment(&spec, &research("ashare-cost-sensitivity"))
        .expect("A 股规则成本压力情景通过标准回测链路重算");
    let stress = &result.cost_sensitivity[0];
    let outcome = stress.outcome.as_ref().expect("A 股压力情景产物");
    assert!(outcome.fills > 0, "成本比较必须包含真实成交");
    assert!(stress.return_delta_bps.is_some_and(|delta| delta < 0));
    assert!(std::path::Path::new(&outcome.artifacts.run_manifest).is_file());
    let summary: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&outcome.artifacts.summary).unwrap()).unwrap();
    assert!(summary["fee_model"]
        .as_str()
        .is_some_and(|model| model.starts_with("AShareFee@v1[")));
}

#[test]
fn walk_forward_rolls_by_step_bars() {
    let root = scratch("walk-forward-roll");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let plan = ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: true,
        step_bars: 10,
        purge_bars: 0,
        embargo_bars: 0,
    };
    let spec = spec_with("wfa-roll", &bars, &root, Some(plan), Vec::new());
    let result = run_experiment(&spec, &research("wfa-roll")).expect("走查实验");
    let report = result.out_of_sample.expect("走查报告");
    // 60 根、训练 20 + 测试 10、步长 10 → 起点 0/10/20/30，四折；起点 40 时 40+30 > 60。
    assert_eq!(report.folds.len(), 4, "折数由样本与步长决定");
    let starts: Vec<u64> = report
        .folds
        .iter()
        .map(|fold| fold.train_range[0])
        .collect();
    assert_eq!(starts, [0, 10, 20, 30], "起点按 step_bars 递推");
    assert_eq!(report.folds[0].test_range, [20, 30]);
    assert_eq!(report.folds[3].test_range, [50, 60]);
}

#[test]
fn baselines_use_the_close_series_and_zero_signal_is_flat() {
    let root = scratch("baselines");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 40);
    let spec = spec_with(
        "baseline-columns",
        &bars,
        &root,
        None,
        vec!["buy-and-hold".into(), "zero-signal".into(), "none".into()],
    );
    let result = run_experiment(&spec, &research("baseline-columns")).expect("基准对照");
    let names: Vec<&str> = result
        .baselines
        .iter()
        .map(|row| row.name.as_str())
        .collect();
    assert_eq!(names, ["buy-and-hold", "zero-signal"], "none 不产出列");

    // 夹具价格：`phase = index % 20`，`phase < 10` 时 `100 + phase*2`，否则 `100 + (20-phase)*2`。
    // 40 根 → 首根 `phase 0 = 100`、末根 `phase 19 = 102` → 买入持有 = +200 bps。
    // 这个数**只可能**来自收盘价序列：策略权益曲线在同一段上给的是另一个数。
    assert_eq!(
        result.baselines[0].return_bps, 200,
        "买入持有用的是收盘价序列，不是策略的权益曲线"
    );
    assert_eq!(result.baselines[1].return_bps, 0, "不交易就是 0 收益");
    assert_eq!(result.baselines[1].max_drawdown_bps, 0, "不交易就没有回撤");
}

#[test]
fn an_unknown_baseline_name_is_rejected() {
    let root = scratch("baseline-unknown");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 40);
    let spec = spec_with("baseline-bad", &bars, &root, None, vec!["momentum".into()]);
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput,
        "基线是闭集，写一个表外的名字不该被静默忽略"
    );
}

#[test]
fn tpe_search_is_seeded_and_never_uses_test_window_scores_to_propose_candidates() {
    let root = scratch("tpe-training-only");
    let first_bars = write_frame(&root, "first.json", "BTCUSDT.BINANCE", 60);
    let second_bars = write_frame(&root, "second.json", "BTCUSDT.BINANCE", 60);
    let mut changed_test: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&second_bars).unwrap()).unwrap();
    for column in ["open_raw", "high_raw", "low_raw", "close_raw"] {
        let values = changed_test[column].as_array_mut().unwrap();
        for value in &mut values[20..] {
            *value = serde_json::json!(value.as_i64().unwrap() * 2);
        }
    }
    std::fs::write(&second_bars, serde_json::to_vec(&changed_test).unwrap()).unwrap();

    let plan = ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: true,
        step_bars: 10,
        purge_bars: 0,
        embargo_bars: 0,
    };
    let mut first = spec_with(
        "tpe-first",
        &first_bars,
        &root.join("first"),
        Some(plan.clone()),
        Vec::new(),
    );
    first.base.strategy.kind = qx_strategy::BuiltinStrategyKind::Momentum;
    first.parameter_space[0].name = "period".into();
    first.parameter_space[0].values = (2..=10).collect();
    first.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::Tpe,
        seed: 2026,
        budget: 6,
        max_training_drawdown_bps: Some(10_000),
        fold_local_search: false,
        continuous_parameter_space: Vec::new(),
        early_pruning: None,
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
    });
    let mut second = first.clone();
    second.experiment_id = "tpe-second".into();
    second.base.bars_path = second_bars;
    second.base.output_dir = root.join("second").to_string_lossy().into_owned();

    let a = run_experiment(&first, &research("tpe-first")).expect("TPE training-only search");
    let b = run_experiment(&second, &research("tpe-second")).expect("TPE with changed test window");
    let search_a = a.search.expect("search trace");
    let search_b = b.search.expect("search trace");
    assert_eq!(a.total_candidates, 6, "budget bounds the 9-point grid");
    assert_eq!(search_a.budget, 6);
    assert_eq!(search_a.trials.len(), 6);
    assert!(search_a.trials[..4]
        .iter()
        .all(|trial| trial.proposal == "tpe_random_startup"));
    assert!(search_a.trials[4..]
        .iter()
        .all(|trial| trial.proposal == "tpe"));
    assert_eq!(
        search_a.trials.iter().map(|trial| (&trial.parameters, trial.training_score_bps)).collect::<Vec<_>>(),
        search_b.trials.iter().map(|trial| (&trial.parameters, trial.training_score_bps)).collect::<Vec<_>>(),
        "changing every test-window price cannot change candidate proposals or their training scores"
    );
    let folds = a.out_of_sample.unwrap().folds;
    assert_eq!(folds.len(), 4);
    assert!(folds
        .iter()
        .all(|fold| fold.max_training_drawdown_bps == Some(10_000)
            && fold.selected_training_max_drawdown_bps.unwrap() <= 10_000));
}

#[test]
fn adaptive_search_runs_fixed_point_sizing_parameters() {
    let root = scratch("adaptive-sizing-parameters");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let plan = ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: false,
        step_bars: 0,
        purge_bars: 0,
        embargo_bars: 0,
    };
    for (parameter, min, max) in [
        ("target_notional_raw", 1_000, 2_000),
        ("target_available_margin_bps", 1_000, 2_000),
        ("target_volatility_bps", 1_000, 2_000),
    ] {
        let experiment_id = format!("adaptive-sizing-{parameter}");
        let mut spec = spec_with(
            &experiment_id,
            &bars,
            &root.join(parameter),
            Some(plan.clone()),
            Vec::new(),
        );
        spec.parameter_space.clear();
        spec.search = Some(ExperimentSearchPlan {
            algorithm: ExperimentSearchAlgorithm::Random,
            seed: 20261023,
            budget: 2,
            max_training_drawdown_bps: None,
            fold_local_search: false,
            continuous_parameter_space: vec![ExperimentParameterRange {
                name: parameter.into(),
                min,
                max,
                step: 1_000,
            }],
            early_pruning: None,
            robustness_subwindows: None,
            minimum_training_robustness_score_bps: None,
            neighborhood_radius: None,
        });

        let result = run_experiment(&spec, &research(&experiment_id))
            .expect("fixed-point sizing dimensions run through adaptive search");
        let trials = result.search.expect("search trace").trials;
        assert_eq!(trials.len(), 2, "{parameter}");
        assert_eq!(
            trials
                .iter()
                .map(|trial| trial.parameters[parameter])
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([min, max]),
            "{parameter} proposals are preserved in the trace"
        );
        assert!(
            result
                .candidates
                .iter()
                .all(|candidate| candidate.outcome.is_some()),
            "{parameter} candidates complete the training and OOS path"
        );
    }
}

#[test]
fn continuous_ranges_run_adaptive_global_and_fold_local_searches() {
    let root = scratch("continuous-search");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let plan = ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: true,
        step_bars: 10,
        purge_bars: 0,
        embargo_bars: 0,
    };
    let mut spec = spec_with(
        "continuous-global",
        &bars,
        &root.join("global"),
        Some(plan),
        Vec::new(),
    );
    spec.parameter_space.clear();
    spec.base.strategy.kind = qx_strategy::BuiltinStrategyKind::Momentum;
    spec.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::TpeKde,
        seed: 20261023,
        budget: 6,
        max_training_drawdown_bps: Some(10_000),
        fold_local_search: false,
        continuous_parameter_space: vec![ExperimentParameterRange {
            name: "period".into(),
            min: 2,
            max: 60,
            step: 1,
        }],
        early_pruning: None,
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
    });
    let global =
        run_experiment(&spec, &research("continuous-global")).expect("连续数值域的全局搜索");
    assert_eq!(global.total_candidates, 6);
    let global_search = global.search.expect("连续搜索轨迹");
    assert_eq!(global_search.trials.len(), 6);
    let global_neighborhood = global_search
        .neighborhood
        .as_ref()
        .expect("continuous global search reports its observed integer neighborhood");
    assert!(global_neighborhood.possible_neighbors > 0);
    assert_eq!(global_neighborhood.center_parameters.len(), 1);
    assert!(global_neighborhood.assessment.is_some());
    assert!(global_search.trials[..4]
        .iter()
        .all(|trial| trial.proposal == "tpe_kde_continuous_random_startup"));
    assert_eq!(global_search.trials[4].proposal, "tpe_kde_continuous");
    let global_values = global_search
        .trials
        .iter()
        .map(|trial| trial.parameters["period"])
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(global_values.len(), 6);
    assert!(global_values.iter().all(|value| (2..=60).contains(value)));

    let mut fold_spec = spec;
    fold_spec.experiment_id = "continuous-fold-local".into();
    fold_spec.base.output_dir = root.join("fold-local").to_string_lossy().into_owned();
    let search = fold_spec.search.as_mut().unwrap();
    search.budget = 5;
    search.fold_local_search = true;
    let fold_result = run_experiment(&fold_spec, &research("continuous-fold-local"))
        .expect("连续数值域的折内搜索");
    let folds = fold_result.search.unwrap().fold_searches;
    assert!(!folds.is_empty());
    for fold in folds {
        assert_eq!(fold.trials.len(), 5);
        let neighborhood = fold
            .neighborhood
            .as_ref()
            .expect("fold-local search reports its training-only neighborhood");
        assert!(neighborhood.possible_neighbors > 0);
        assert_eq!(neighborhood.center_parameters.len(), 1);
        assert!(neighborhood.assessment.is_some());
        assert_eq!(fold.trials[4].proposal, "tpe_kde_continuous");
        let values = fold
            .trials
            .iter()
            .map(|trial| trial.parameters["period"])
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(values.len(), 5, "fold {} repeats a proposal", fold.fold);
        assert!(values.iter().all(|value| (2..=60).contains(value)));
    }
}

#[test]
fn fold_local_tpe_restarts_from_each_training_slice_and_ignores_later_bars() {
    let root = scratch("fold-local-tpe");
    let first_bars = write_frame(&root, "first.json", "BTCUSDT.BINANCE", 60);
    let second_bars = write_frame(&root, "second.json", "BTCUSDT.BINANCE", 60);
    let mut changed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&second_bars).unwrap()).unwrap();
    for column in ["open_raw", "high_raw", "low_raw", "close_raw"] {
        for value in &mut changed[column].as_array_mut().unwrap()[30..] {
            *value = serde_json::json!(value.as_i64().unwrap() * 2);
        }
    }
    std::fs::write(&second_bars, serde_json::to_vec(&changed).unwrap()).unwrap();
    let plan = ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: true,
        step_bars: 10,
        purge_bars: 0,
        embargo_bars: 0,
    };
    let mut first = spec_with(
        "fold-local-first",
        &first_bars,
        &root.join("first"),
        Some(plan),
        Vec::new(),
    );
    first.base.strategy.kind = qx_strategy::BuiltinStrategyKind::Momentum;
    first.parameter_space[0].name = "period".into();
    first.parameter_space[0].values = (2..=10).collect();
    first.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::Tpe,
        seed: 2026,
        budget: 5,
        max_training_drawdown_bps: Some(10_000),
        fold_local_search: true,
        continuous_parameter_space: Vec::new(),
        early_pruning: None,
        robustness_subwindows: Some(3),
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
    });
    let mut second = first.clone();
    second.experiment_id = "fold-local-second".into();
    second.base.bars_path = second_bars;
    second.base.output_dir = root.join("second").to_string_lossy().into_owned();

    let a = run_experiment(&first, &research("fold-local-first")).expect("fold-local search");
    let b = run_experiment(&second, &research("fold-local-second"))
        .expect("fold-local search with later bars changed");
    let searches_a = a.search.unwrap().fold_searches;
    let searches_b = b.search.unwrap().fold_searches;
    let folds = a.out_of_sample.unwrap().folds;
    assert_eq!(searches_a.len(), 4);
    assert_eq!(folds.len(), 4);
    for (search, fold) in searches_a.iter().zip(&folds) {
        assert_eq!(search.trials.len(), 5);
        assert_eq!(search.candidates.len(), 5);
        assert_eq!(search.train_range, fold.train_range);
        assert_eq!(search.test_range, fold.test_range);
        assert_eq!(search.selected_ordinal, Some(fold.selected_ordinal));
        assert!(fold.selected_training_robustness_score_bps.is_some());
        assert!(search
            .trials
            .iter()
            .all(|trial| trial.training_robustness_score_bps.is_some()));
        let selected_run_manifest = search
            .selected_run_manifest
            .as_deref()
            .expect("selected fold candidate has a RunManifest");
        assert!(std::path::Path::new(selected_run_manifest).is_file());
        let selected = &search.candidates[search.selected_ordinal.unwrap() - 1];
        assert_eq!(selected.parameters, fold.selected_parameters);
        let equity = equity_series(&selected.outcome.as_ref().unwrap().artifacts.equity);
        assert_eq!(equity.len(), 30, "折内重跑只接收训练、purge 与测试切片");
    }
    for fold_index in 0..2 {
        let left = &searches_a[fold_index].trials;
        let right = &searches_b[fold_index].trials;
        assert_eq!(
            left.iter()
                .map(|trial| (
                    &trial.parameters,
                    trial.training_score_bps,
                    trial.training_robustness_score_bps
                ))
                .collect::<Vec<_>>(),
            right
                .iter()
                .map(|trial| (
                    &trial.parameters,
                    trial.training_score_bps,
                    trial.training_robustness_score_bps
                ))
                .collect::<Vec<_>>(),
            "晚于本折训练窗的 Bar 不得改变该折的 TPE 提议和训练分数"
        );
    }

    let mut gated = first.clone();
    gated.experiment_id = "fold-local-robustness-floor".into();
    gated.base.output_dir = root.join("gated").to_string_lossy().into_owned();
    gated
        .search
        .as_mut()
        .unwrap()
        .minimum_training_robustness_score_bps = Some(i32::MAX);
    let gated_result = run_experiment(&gated, &research("fold-local-robustness-floor"))
        .expect("a floor with no eligible candidate still produces auditable fold reports");
    assert!(gated_result.out_of_sample.is_none());
    let gated_searches = gated_result.search.unwrap().fold_searches;
    assert!(gated_searches.iter().all(|fold| {
        fold.selected_ordinal.is_none()
            && fold.trials.iter().all(|trial| {
                trial
                    .training_robustness_score_bps
                    .is_some_and(|score| score < i32::MAX)
            })
    }));
}

#[test]
fn kde_tpe_search_trace_is_reproducible_and_test_window_isolated() {
    let root = scratch("kde-tpe-oos");
    let first_bars = write_frame(&root, "first.json", "BTCUSDT.BINANCE", 60);
    let second_bars = write_frame(&root, "second.json", "BTCUSDT.BINANCE", 60);
    let mut changed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&second_bars).unwrap()).unwrap();
    for column in ["open_raw", "high_raw", "low_raw", "close_raw"] {
        for value in &mut changed[column].as_array_mut().unwrap()[20..] {
            *value = serde_json::json!(value.as_i64().unwrap() * 2);
        }
    }
    std::fs::write(&second_bars, serde_json::to_vec(&changed).unwrap()).unwrap();
    let plan = ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: false,
        step_bars: 0,
        purge_bars: 0,
        embargo_bars: 0,
    };
    let mut first = spec_with(
        "kde-tpe-first",
        &first_bars,
        &root.join("first"),
        Some(plan),
        Vec::new(),
    );
    first.base.strategy.kind = qx_strategy::BuiltinStrategyKind::Momentum;
    first.parameter_space[0].name = "period".into();
    first.parameter_space[0].values = (2..=10).collect();
    first.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::TpeKde,
        seed: 20261022,
        budget: 6,
        max_training_drawdown_bps: None,
        fold_local_search: false,
        continuous_parameter_space: Vec::new(),
        early_pruning: None,
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
    });
    let mut second = first.clone();
    second.experiment_id = "kde-tpe-second".into();
    second.base.bars_path = second_bars;
    second.base.output_dir = root.join("second").to_string_lossy().into_owned();

    let a = run_experiment(&first, &research("kde-tpe-first")).unwrap();
    let b = run_experiment(&second, &research("kde-tpe-second")).unwrap();
    let a_trials = a.search.unwrap().trials;
    let b_trials = b.search.unwrap().trials;
    assert_eq!(a_trials.len(), 6);
    assert!(a_trials[..4]
        .iter()
        .all(|trial| trial.proposal == "tpe_kde_random_startup"));
    assert!(a_trials[4..]
        .iter()
        .all(|trial| trial.proposal == "tpe_kde"));
    assert_eq!(
        a_trials
            .iter()
            .map(|trial| (&trial.parameters, trial.training_score_bps))
            .collect::<Vec<_>>(),
        b_trials
            .iter()
            .map(|trial| (&trial.parameters, trial.training_score_bps))
            .collect::<Vec<_>>()
    );
}

#[test]
fn adaptive_search_requires_oos_plan_and_enforces_budget() {
    let root = scratch("tpe-validation");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let mut spec = spec_with("tpe-validation", &bars, &root, None, Vec::new());
    spec.search = Some(ExperimentSearchPlan {
        algorithm: ExperimentSearchAlgorithm::Tpe,
        seed: 1,
        budget: 5,
        max_training_drawdown_bps: None,
        fold_local_search: false,
        continuous_parameter_space: Vec::new(),
        early_pruning: None,
        robustness_subwindows: None,
        minimum_training_robustness_score_bps: None,
        neighborhood_radius: None,
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
    spec.search
        .as_mut()
        .unwrap()
        .minimum_training_robustness_score_bps = Some(0);
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput,
        "robustness floor must require robustness_subwindows"
    );
    spec.search
        .as_mut()
        .unwrap()
        .minimum_training_robustness_score_bps = None;
    spec.search.as_mut().unwrap().max_training_drawdown_bps = Some(10_001);
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
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );

    spec.search.as_mut().unwrap().max_training_drawdown_bps = None;
    spec.search.as_mut().unwrap().robustness_subwindows = Some(9);
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
    spec.search.as_mut().unwrap().robustness_subwindows = Some(8);
    spec.split_plan.as_mut().unwrap().train_bars = 8;
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput,
        "configured robustness windows must each fit the training interval"
    );
    spec.search.as_mut().unwrap().robustness_subwindows = None;
    spec.search.as_mut().unwrap().neighborhood_radius = Some(0);
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput,
        "neighborhood radius must be positive"
    );
    spec.search.as_mut().unwrap().neighborhood_radius = Some(9);
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput,
        "neighborhood radius is bounded"
    );
}

#[test]
fn duplicate_baseline_names_are_rejected_instead_of_emitting_duplicate_columns() {
    let root = scratch("baseline-duplicate");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 40);
    let spec = spec_with(
        "baseline-duplicate",
        &bars,
        &root,
        None,
        vec!["zero-signal".into(), "zero-signal".into()],
    );
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}

#[test]
fn a_split_plan_that_does_not_fit_the_sample_is_a_fidelity_gap() {
    let root = scratch("split-too-short");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 20);
    let plan = ExperimentSplitPlan {
        train_bars: 15,
        test_bars: 10,
        walk_forward: false,
        step_bars: 0,
        purge_bars: 0,
        embargo_bars: 0,
    };
    let spec = spec_with("split-short", &bars, &root, Some(plan), Vec::new());
    assert_eq!(
        run_experiment(&spec, &research("split-short"))
            .unwrap_err()
            .category(),
        AppErrorCategory::FidelityInsufficient,
        "样本装不下训练+测试是样本不够，不是输入拼错"
    );
    assert!(
        !root.join("out").join("split-short").exists(),
        "切分规格必须在任何候选产物创建前预检"
    );
}

#[test]
fn omitting_the_new_knobs_keeps_the_spec_and_report_byte_identical() {
    let root = scratch("split-pure-addition");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 40);
    let spec = spec_with("pure-addition", &bars, &root, None, Vec::new());
    let spec_json = spec.to_json().expect("序列化");
    assert!(
        !spec_json.contains("split_plan") && !spec_json.contains("baselines"),
        "没写这两格的 spec 必须序列化回与加字段之前逐字节相同的形状，\
         否则任何旧 `.experiment.json` 都会因为规格指纹变了而报 Conflict: {spec_json}"
    );
    let result = run_experiment(&spec, &research("pure-addition")).expect("不带切分的实验");
    let result_json = result.to_json().expect("序列化");
    assert!(
        !result_json.contains("out_of_sample") && !result_json.contains("\"baselines\""),
        "没请求就不该在报告里留下空壳字段: {result_json}"
    );
}
