use crate::cases::run_experiment;
use crate::context::{CallerCapability, RunContext};
use crate::error::AppErrorCategory;
use crate::tests::fixtures::{experiment_spec, scratch, write_frame};
use std::path::PathBuf;

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
    oos_selection.split_plan = Some(crate::cases::ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: false,
        step_bars: 0,
    });
    let oos = run_experiment(&oos_selection, &research("classic-strategy-selection-oos"))
        .expect("策略 kind 应参与同一共享样本外选优路径");
    assert_eq!(oos.succeeded_candidates, 3);
    let fold = &oos.out_of_sample.expect("requested OOS report").folds[0];
    let selected = &oos.candidates[fold.selected_ordinal - 1];
    assert_eq!(fold.selected_strategy_kind, selected.strategy_kind);
    assert!(fold.selected_strategy_kind.is_some());
}
