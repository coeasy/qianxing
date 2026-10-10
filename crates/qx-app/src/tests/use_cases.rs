//! T2-2 的用例契约：`ValidateDataset` / `RunBacktest` / `VerifyRun` 的**端到端**行为。
//!
//! 这一族是 G1/G2 的底座：它证明"一次 Bar 回测"真的能从用例边界跑通、落四份产物、
//! 再被独立复核；也证明失败被翻成调用方看得懂的**类别**（而不是一串字符串）。
//!
//! 它刻意不碰 `qx-cli`、不碰 `qx-api`、不碰 `qx-python`——那三个门面各有自己的等价性用例。
//! 这里只回答一件事：**用例本身站不站得住**。

use crate::cases::{run_backtest, run_experiment, validate_dataset, verify_run};
use crate::cases::{ExperimentParameterSpace, RunExperimentSpec};
use crate::context::{CallerCapability, RunContext};
use crate::error::AppErrorCategory;
use crate::spec::{BacktestArtifacts, BacktestOutcome};
use crate::tests::fixtures::{backtest_spec, dataset_spec, frame_json, scratch, write_frame};
use std::path::PathBuf;

/// 三个用例都用 R 档（研究）：本切片没有外部副作用。
fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

#[test]
fn a_healthy_dataset_is_usable_and_carries_a_content_fingerprint() {
    let root = scratch("ds-healthy");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 40);
    let verdict = validate_dataset(&dataset_spec("ds-healthy", &bars), &research("ds-healthy"))
        .expect("校验");
    assert!(
        verdict.usable,
        "40 根 Bar 足够跑一次 Bar 回测: {:?}",
        verdict.gaps
    );
    assert!(
        verdict.gaps.is_empty(),
        "可用就不该有理由: {:?}",
        verdict.gaps
    );
    assert_eq!(verdict.rows, 40);
    assert_eq!(verdict.instrument, "BTCUSDT.BINANCE");
    assert!(
        verdict.content_fingerprint.starts_with("barframe:"),
        "内容指纹必须与 RunManifest 的合成身份同前缀: {}",
        verdict.content_fingerprint
    );
    assert_eq!(
        verdict.content_fingerprint.len(),
        "barframe:".len() + 16,
        "指纹是 16 位十六进制"
    );
}

#[test]
fn a_too_short_dataset_is_a_verdict_not_an_error() {
    let root = scratch("ds-short");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 1);
    let verdict = validate_dataset(&dataset_spec("ds-short", &bars), &research("ds-short"))
        .expect("数据不足是研究结论，不是故障");
    assert!(!verdict.usable);
    assert!(
        !verdict.gaps.is_empty(),
        "判了不可用就必须给出理由——空理由等于让调用方去猜"
    );
    assert_eq!(verdict.rows, 1, "裁决必须回报实际读到的根数");
}

#[test]
fn a_dataset_that_is_not_on_disk_is_a_data_unavailable_error() {
    let root = scratch("ds-missing");
    let bars = root.join("nope.json").to_string_lossy().into_owned();
    let error = validate_dataset(&dataset_spec("ds-missing", &bars), &research("ds-missing"))
        .expect_err("文件不在就没有判断可给");
    assert_eq!(error.category(), AppErrorCategory::DataUnavailable);
    assert_eq!(error.correlation_id(), Some("ds-missing"));
    assert!(!error.safe_to_retry(), "同一份数据重发必然再失败");
}

#[test]
fn a_frame_with_unordered_timestamps_is_rejected_by_the_frame_reader() {
    // 这条用例钉住 `validate_dataset` 模块文档里那句话：单调性由 `BarFrame` 单源负责，
    // 本层没有第二份检查。所以乱序不是"裁决里的一条 gap"，而是在读入处就被拒。
    let root = scratch("ds-unordered");
    let mut payload: serde_json::Value =
        serde_json::from_str(&frame_json("BTCUSDT.BINANCE", 3)).expect("夹具 JSON");
    payload["ts"] = serde_json::json!([3000, 1000, 2000]);
    let bars = root.join("bars.json").to_string_lossy().into_owned();
    std::fs::write(&bars, payload.to_string()).expect("写夹具");
    let error = validate_dataset(
        &dataset_spec("ds-unordered", &bars),
        &research("ds-unordered"),
    )
    .expect_err("乱序时间戳必须被拒");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
}

#[test]
fn a_backtest_run_writes_four_artifacts_and_verifies() {
    let root = scratch("bt-run");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let output_dir = root.join("out");
    let spec = backtest_spec("run-1", &bars, &output_dir);
    let outcome = run_backtest(&spec, &research("run-1")).expect("回测跑通");
    assert_eq!(outcome.run_id, "run-1");
    assert_eq!(outcome.instrument, "BTCUSDT.BINANCE");
    assert_eq!(outcome.equity_points, 60, "权益曲线必须与样本逐根对应");
    assert!(
        outcome.fills > 0,
        "振荡夹具必须真跑出成交，否则'跑通'退化成'跑了个空'"
    );
    assert_eq!(outcome.result_hash.len(), 16, "结果哈希是 16 位十六进制");
    assert!(outcome.data_fingerprint.starts_with("barframe:"));
    for path in [
        &outcome.artifacts.run_manifest,
        &outcome.artifacts.summary,
        &outcome.artifacts.equity,
        &outcome.artifacts.fills,
    ] {
        assert!(PathBuf::from(path).is_file(), "产物缺失: {path}");
    }
    let verification = verify_run(&outcome, &research("run-1")).expect("复核跑得起来");
    assert!(
        verification.verified,
        "刚落的产物必须自洽: {:?}",
        verification.mismatches
    );
    assert_eq!(verification.result_hash, outcome.result_hash);
    assert_eq!(verification.data_fingerprint, outcome.data_fingerprint);
    assert!(!verification.checks.is_empty(), "核过的口径要列出来");
    assert!(verification.mismatches.is_empty());
}

#[test]
fn a_repeated_run_with_the_same_identity_is_idempotent() {
    let root = scratch("bt-idem");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let output_dir = root.join("out");
    let spec = backtest_spec("run-idem", &bars, &output_dir);
    let first = run_backtest(&spec, &research("run-idem")).expect("第一次");
    let second = run_backtest(&spec, &research("run-idem")).expect("第二次必须能重跑");
    assert_eq!(
        first.result_hash, second.result_hash,
        "同一身份重跑必须逐位同哈希——否则'可复现'这个词没有内容"
    );
    assert_eq!(first.artifacts, second.artifacts);
    assert_eq!(first.fills, second.fills);
}

#[test]
fn the_same_run_id_with_a_different_spec_is_a_conflict() {
    let root = scratch("bt-conflict");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let output_dir = root.join("out");
    let first = backtest_spec("run-conflict", &bars, &output_dir);
    run_backtest(&first, &research("run-conflict")).expect("第一次");
    let mut second = backtest_spec("run-conflict", &bars, &output_dir);
    second.initial_cash_raw = first.initial_cash_raw * 2;
    let error = run_backtest(&second, &research("run-conflict"))
        .expect_err("同一 run_id 不同身份必须冲突，不许悄悄覆盖");
    assert_eq!(error.category(), AppErrorCategory::Conflict);
    assert_eq!(error.correlation_id(), Some("run-conflict"));
    assert!(!error.safe_to_retry(), "重发同一份冲突的请求只会再撞一次");
}

#[test]
fn a_bar_frame_for_a_different_instrument_is_rejected() {
    let root = scratch("bt-instrument");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let output_dir = root.join("out");
    let mut spec = backtest_spec("run-mismatch", &bars, &output_dir);
    spec.instrument = "ETHUSDT.BINANCE".to_string();
    let error =
        run_backtest(&spec, &research("run-mismatch")).expect_err("用 A 的数据跑 B 的回测必须被拒");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(
        error.message().contains("ETHUSDT.BINANCE"),
        "拒绝理由要点名对不上的标的: {}",
        error.message()
    );
}

#[test]
fn a_run_with_too_few_bars_is_fidelity_insufficient() {
    let root = scratch("bt-short");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 1);
    let output_dir = root.join("out");
    let spec = backtest_spec("run-short", &bars, &output_dir);
    let error = run_backtest(&spec, &research("run-short")).expect_err("样本过短必须被拒");
    assert_eq!(
        error.category(),
        AppErrorCategory::FidelityInsufficient,
        "档位不够与输入非法是两件事：前者要调用方换数据，后者要调用方改参数"
    );
    assert!(!error.safe_to_retry());
    assert!(
        !PathBuf::from(&output_dir)
            .join("run-short.run.json")
            .is_file(),
        "被档位闸拦下的运行不许留下任何产物——半个产物比没有产物更坏"
    );
}

#[test]
fn a_tampered_summary_makes_verification_fail_without_erroring() {
    let root = scratch("bt-tamper");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let output_dir = root.join("out");
    let spec = backtest_spec("run-tamper", &bars, &output_dir);
    let outcome = run_backtest(&spec, &research("run-tamper")).expect("回测跑通");
    let summary_path = PathBuf::from(&outcome.artifacts.summary);
    let mut summary: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&summary_path).expect("读 summary"))
            .expect("解析 summary");
    summary["result_hash"] = serde_json::json!("0000000000000000");
    std::fs::write(&summary_path, summary.to_string()).expect("改写 summary");
    let verification =
        verify_run(&outcome, &research("run-tamper")).expect("复核本身不报错——不通过是结论");
    assert!(!verification.verified);
    assert!(
        verification
            .mismatches
            .iter()
            .any(|mismatch| mismatch.contains("result_hash")),
        "不一致的点要点名是哪条口径: {:?}",
        verification.mismatches
    );
    assert!(
        !verification.checks.is_empty(),
        "核过的口径仍要列出来，读者要能回答'到底核过哪几条'"
    );
}

#[test]
fn verification_rejects_tampered_outcome_metrics_and_identity() {
    let root = scratch("bt-outcome-tamper");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let spec = backtest_spec("run-outcome-tamper", &bars, &root.join("out"));
    let outcome = run_backtest(&spec, &research("run-outcome-tamper")).expect("回测跑通");

    let mut tampered = outcome.clone();
    tampered.instrument = "ETHUSDT.BINANCE".into();
    let verification = verify_run(&tampered, &research("verify-instrument")).unwrap();
    assert!(!verification.verified);
    assert!(verification
        .mismatches
        .iter()
        .any(|item| item.contains("instrument")));

    let mut tampered = outcome.clone();
    tampered.equity_points += 1;
    let verification = verify_run(&tampered, &research("verify-equity-points")).unwrap();
    assert!(!verification.verified);
    assert!(verification
        .mismatches
        .iter()
        .any(|item| item.contains("equity_points")));

    let mut tampered = outcome.clone();
    tampered.return_bps = tampered.return_bps.saturating_add(1);
    let verification = verify_run(&tampered, &research("verify-return")).unwrap();
    assert!(!verification.verified);
    assert!(verification
        .mismatches
        .iter()
        .any(|item| item.contains("return_bps")));

    let mut tampered = outcome;
    tampered.max_drawdown_bps = tampered.max_drawdown_bps.saturating_add(1);
    let verification = verify_run(&tampered, &research("verify-drawdown")).unwrap();
    assert!(!verification.verified);
    assert!(verification
        .mismatches
        .iter()
        .any(|item| item.contains("max_drawdown_bps")));
}

#[test]
fn verification_rejects_manifest_metadata_drift_and_nondeterministic_runs() {
    for (label, mutate) in [
        (
            "strategy-version",
            Box::new(|manifest: &mut serde_json::Value| {
                manifest["strategy_version"] = serde_json::json!("tampered");
            }) as Box<dyn Fn(&mut serde_json::Value)>,
        ),
        (
            "determinism-mode",
            Box::new(|manifest: &mut serde_json::Value| {
                manifest["determinism_mode"] = serde_json::json!(false);
            }),
        ),
    ] {
        let root = scratch(&format!("bt-manifest-{label}"));
        let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
        let spec = backtest_spec(&format!("run-manifest-{label}"), &bars, &root.join("out"));
        let outcome =
            run_backtest(&spec, &research(&format!("run-manifest-{label}"))).expect("回测跑通");
        let manifest_path = PathBuf::from(&outcome.artifacts.run_manifest);
        let mut manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).expect("读 manifest"))
                .expect("解析 manifest");
        mutate(&mut manifest);
        std::fs::write(&manifest_path, manifest.to_string()).expect("改写 manifest");

        let verification = verify_run(&outcome, &research(&format!("verify-{label}")))
            .expect("复核结论不应转成调用错误");
        assert!(!verification.verified, "{label} 篡改必须拒绝");
    }
}

#[test]
fn parameter_experiment_reuses_backtest_and_comparison_use_cases() {
    let root = scratch("experiment-grid");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let spec = RunExperimentSpec {
        schema_version: 1,
        experiment_id: "sma-grid".into(),
        base: backtest_spec("unused-base-run", &bars, &root.join("out")),
        parameter_space: vec![ExperimentParameterSpace {
            name: "fast_window".into(),
            values: vec![1, 2],
        }],
    };
    let result = run_experiment(&spec, &research("sma-grid")).expect("参数实验");
    assert_eq!(result.total_candidates, 2);
    assert_eq!(result.completed_candidates, 2, "{:?}", result.candidates);
    assert_eq!(result.candidates.len(), 2);
    assert!(result.candidates.iter().all(|run| run.error.is_none()));
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|run| run.outcome.as_ref().unwrap().run_id.as_str())
            .collect::<Vec<_>>(),
        ["sma-grid-0001", "sma-grid-0002"]
    );
    assert_eq!(result.comparison.as_ref().unwrap().runs.len(), 2);
    assert!(PathBuf::from(&result.artifact_path).is_file());

    let repeated = run_experiment(&spec, &research("sma-grid")).expect("幂等重跑");
    assert_eq!(repeated, result);
    let changed = RunExperimentSpec {
        parameter_space: vec![ExperimentParameterSpace {
            name: "fast_window".into(),
            values: vec![1, 3],
        }],
        ..spec
    };
    assert_eq!(
        run_experiment(&changed, &research("sma-grid"))
            .unwrap_err()
            .category(),
        AppErrorCategory::Conflict,
        "同一实验身份不能悄悄绑定另一份参数规格"
    );
}

#[test]
fn parameter_experiment_isolates_invalid_candidates_and_bounds_the_grid() {
    let root = scratch("experiment-failure-isolation");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let spec = RunExperimentSpec {
        schema_version: 1,
        experiment_id: "fast-grid".into(),
        base: backtest_spec("unused-base-run", &bars, &root.join("out")),
        parameter_space: vec![ExperimentParameterSpace {
            name: "fast_window".into(),
            values: vec![0, 2],
        }],
    };
    let result = run_experiment(&spec, &research("fast-grid")).expect("单候选失败不阻断整组");
    assert_eq!(result.completed_candidates, 2);
    assert_eq!(result.succeeded_candidates, 1);
    assert_eq!(result.failed_candidates, 1);
    assert!(result.candidates[0].error.is_some());
    assert!(result.candidates[1].outcome.is_some());
    assert!(result.comparison.is_none());

    let oversized = RunExperimentSpec {
        experiment_id: "oversized-grid".into(),
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: (1..=257).collect(),
        }],
        ..spec
    };
    assert_eq!(
        run_experiment(&oversized, &research("oversized-grid"))
            .unwrap_err()
            .category(),
        AppErrorCategory::InvalidInput
    );

    let mut ignored_knob = RunExperimentSpec {
        experiment_id: "macd-ignored-knob".into(),
        parameter_space: vec![ExperimentParameterSpace {
            name: "fast_window".into(),
            values: vec![2, 3],
        }],
        ..oversized
    };
    ignored_knob.base.strategy.kind = qx_strategy::BuiltinStrategyKind::Macd;
    assert_eq!(
        run_experiment(&ignored_knob, &research("macd-ignored-knob"))
            .unwrap_err()
            .category(),
        AppErrorCategory::InvalidInput,
        "网格不能接受策略内核不读取的旋钮"
    );

    let traversal = RunExperimentSpec {
        experiment_id: "..".into(),
        ..ignored_knob.clone()
    };
    assert_eq!(
        traversal.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput,
        "实验 ID 不能作为路径段逃出服务端产物目录"
    );

    let overflow = RunExperimentSpec {
        schema_version: 1,
        experiment_id: "overflow-grid".into(),
        base: backtest_spec("unused-overflow", &bars, &root.join("overflow")),
        parameter_space: vec![ExperimentParameterSpace {
            name: "fast_window".into(),
            values: vec![1, i128::MAX],
        }],
    };
    let overflow_result =
        run_experiment(&overflow, &research("overflow-grid")).expect("单候选溢出应被隔离");
    assert_eq!(overflow_result.completed_candidates, 2);
    assert_eq!(overflow_result.succeeded_candidates, 1);
    assert_eq!(overflow_result.failed_candidates, 1);
}

#[test]
fn a_missing_artifact_is_a_verification_conclusion_not_an_error() {
    let root = scratch("bt-missing-artifact");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let output_dir = root.join("out");
    let spec = backtest_spec("run-missing", &bars, &output_dir);
    let outcome = run_backtest(&spec, &research("run-missing")).expect("回测跑通");
    std::fs::remove_file(&outcome.artifacts.fills).expect("删掉 fills.csv");
    let verification =
        verify_run(&outcome, &research("run-missing")).expect("产物缺失是可核验的结论，不是故障");
    assert!(!verification.verified);
    assert!(
        verification
            .mismatches
            .iter()
            .any(|mismatch| mismatch.contains("fills")),
        "缺的是哪一份要说清楚: {:?}",
        verification.mismatches
    );
}

#[test]
fn every_use_case_failure_carries_the_callers_correlation_id() {
    let root = scratch("bt-cid");
    let missing = root.join("nope.json").to_string_lossy().into_owned();

    let spec = backtest_spec("run-cid", &missing, &root.join("out"));
    let error = run_backtest(&spec, &research("run-cid")).expect_err("缺文件必须报错");
    assert_eq!(error.category(), AppErrorCategory::DataUnavailable);
    assert_eq!(
        error.correlation_id(),
        Some("run-cid"),
        "每条出口都必须挂上调用方的 id，否则用户拿到的错误无从与日志、产物对齐"
    );

    let error = validate_dataset(&dataset_spec("ds-cid", &missing), &research("ds-cid"))
        .expect_err("缺文件必须报错");
    assert_eq!(error.correlation_id(), Some("ds-cid"));

    let headless = BacktestOutcome {
        run_id: "   ".into(),
        instrument: "BTCUSDT.BINANCE".into(),
        result_hash: String::new(),
        data_fingerprint: String::new(),
        fills: 0,
        equity_points: 0,
        return_bps: 0,
        max_drawdown_bps: 0,
        artifacts: BacktestArtifacts {
            run_manifest: "run.json".into(),
            summary: "summary.json".into(),
            equity: "equity.csv".into(),
            fills: "fills.csv".into(),
        },
    };
    let error = verify_run(&headless, &research("verify-cid")).expect_err("没有 run_id 就没有对象");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert_eq!(
        error.correlation_id(),
        Some("verify-cid"),
        "复核的失败也要能对齐到这次调用"
    );
}
