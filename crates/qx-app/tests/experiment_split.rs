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
    run_experiment, AppErrorCategory, CallerCapability, ExperimentParameterSpace,
    ExperimentSplitPlan, RunContext, RunExperimentSpec,
};

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
    }
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
    };
    let spec = spec_with("split-repro", &bars, &root, Some(plan), Vec::new());
    let first = run_experiment(&spec, &research("split-repro")).expect("带切分的实验");
    let second = run_experiment(&spec, &research("split-repro")).expect("幂等重跑");
    assert_eq!(first, second, "同一切分计划两次运行必须给出同一份报告");

    let report = first.out_of_sample.expect("写了切分计划就必须有 OOS 报告");
    assert_eq!(report.folds.len(), 1, "未打开走查时只有一段切分");
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
fn walk_forward_rolls_by_step_bars() {
    let root = scratch("walk-forward-roll");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let plan = ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: true,
        step_bars: 10,
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
