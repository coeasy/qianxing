//! T2-1 的规格契约：三个入口交出同一份 JSON 字节，所以 spec 的**形状**本身就是公共面。
//!
//! 这一族守两件事：
//!
//! 1. **严格性**——多一个键、schema 版本不对、身份串带路径分隔符，都必须当场拒，
//!    而不是"尽力而为"地猜。静默忽略一个拼错的字段名，等于让用户以为设上了。
//! 2. **稳定性**——每份 spec 与裁决的**键集**是三个入口逐字节比对的载体，
//!    加键删键都要有人同时改三侧，所以键集在这里被钉住。

use crate::error::AppErrorCategory;
use crate::spec::{
    BacktestArtifacts, BacktestOutcome, BacktestSpec, BuiltinStrategySpec, DatasetSpec,
    DatasetVerdict, VerificationResult, BACKTEST_SPEC_SCHEMA_VERSION, DATASET_SPEC_SCHEMA_VERSION,
    MIN_BACKTEST_BARS,
};
use crate::tests::fixtures::{backtest_spec, dataset_spec, scratch};
use qx_strategy::BuiltinStrategyKind;
use serde_json::Value;
use std::collections::BTreeSet;

/// 解析一份 JSON 对象的键集。
fn keys_of(payload: &str) -> BTreeSet<String> {
    serde_json::from_str::<Value>(payload)
        .expect("JSON 可解析")
        .as_object()
        .expect("JSON 是对象")
        .keys()
        .cloned()
        .collect()
}

fn key_set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

/// 断言一次形状自检**必须**被拒，并回它的类别。`context` 进 panic 文案，
/// 这样并行跑的时候能看出是哪一格先破的。
fn rejected(result: Result<(), crate::error::AppError>, context: &str) -> AppErrorCategory {
    match result {
        Ok(()) => panic!("{context} 必须被拒"),
        Err(error) => error.category(),
    }
}

#[test]
fn the_backtest_spec_round_trips_through_json_without_losing_a_field() {
    let root = scratch("spec-roundtrip");
    let bars = root.join("bars.json").to_string_lossy().into_owned();
    let spec = backtest_spec("run-roundtrip", &bars, &root);
    let payload = spec.to_json().expect("序列化");
    let back = BacktestSpec::from_json(&payload).expect("反序列化");
    assert_eq!(back, spec, "spec 过一遍 JSON 必须逐字段回来");
    assert_eq!(back.schema_version, BACKTEST_SPEC_SCHEMA_VERSION);
}

#[test]
fn the_backtest_spec_json_keys_are_pinned() {
    let root = scratch("spec-keys");
    let bars = root.join("bars.json").to_string_lossy().into_owned();
    let spec = backtest_spec("run-keys", &bars, &root);
    let payload = spec.to_json().expect("序列化");
    assert_eq!(
        keys_of(&payload),
        key_set(&[
            "schema_version",
            "run_id",
            "instrument",
            "bars_path",
            "settlement_currency",
            "initial_cash_raw",
            "seed",
            "output_dir",
            "strategy",
        ]),
        "BacktestSpec 的键集是三个入口逐字节比对的载体，改它必须同时改三侧"
    );
    let strategy = &serde_json::from_str::<Value>(&payload).expect("JSON")["strategy"];
    let strategy_keys = strategy
        .as_object()
        .expect("strategy 是对象")
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        strategy_keys,
        key_set(&[
            "kind",
            "strategy_id",
            "quantity_raw",
            "fast_window",
            "slow_window",
        ]),
        "内置策略声明只暴露确定会改结果的那几格"
    );
}

#[test]
fn an_unknown_field_is_rejected_rather_than_silently_ignored() {
    let payload = serde_json::json!({
        "schema_version": 1,
        "dataset_id": "ds-1",
        "bars_path": "bars.json",
        "intial_cash": 1,
    })
    .to_string();
    let error = DatasetSpec::from_json(&payload).expect_err("多一个键必须拒");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(
        error.message().contains("intial_cash"),
        "拒绝的理由要点名那个多余的键，否则调用方无从发现自己拼错了：{}",
        error.message()
    );
}

#[test]
fn a_schema_version_mismatch_is_rejected_not_best_effort_parsed() {
    let mut spec = dataset_spec("ds-v2", "bars.json");
    spec.schema_version = DATASET_SPEC_SCHEMA_VERSION + 1;
    let error = spec.validate().expect_err("版本不符必须拒");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(
        error.message().contains("schema_version"),
        "{}",
        error.message()
    );

    let mut backtest = backtest_spec("run-v2", "bars.json", &scratch("spec-version"));
    backtest.schema_version = BACKTEST_SPEC_SCHEMA_VERSION + 1;
    assert_eq!(
        backtest.validate().expect_err("版本不符必须拒").category(),
        AppErrorCategory::InvalidInput
    );
}

#[test]
fn identity_fields_that_could_escape_the_output_directory_are_rejected() {
    // 身份串会变成产物**文件名**的一部分，也会出现在三个入口的 URL/命令行里。
    for bad in ["../escape", "a/b", "a\\b", "", "名字"] {
        let spec = dataset_spec(bad, "bars.json");
        assert_eq!(
            rejected(spec.validate(), &format!("dataset_id={bad:?}")),
            AppErrorCategory::InvalidInput
        );
    }
    let root = scratch("spec-identity");
    for bad in ["../escape", "a/b", ""] {
        let mut spec = backtest_spec("run-ok", "bars.json", &root);
        spec.run_id = bad.to_string();
        assert_eq!(
            rejected(spec.validate(), &format!("run_id={bad:?}")),
            AppErrorCategory::InvalidInput
        );
    }
}

#[test]
fn every_shape_rule_of_the_two_specs_reports_invalid_input() {
    let root = scratch("spec-shapes");
    let bars = root.join("bars.json").to_string_lossy().into_owned();

    let dataset = dataset_spec("ds-1", "   ");
    assert_eq!(
        dataset
            .validate()
            .expect_err("空 bars_path 必须拒")
            .category(),
        AppErrorCategory::InvalidInput
    );

    let mut empty_instrument = backtest_spec("run-1", &bars, &root);
    empty_instrument.instrument = "  ".to_string();
    assert_eq!(
        empty_instrument
            .validate()
            .expect_err("空 instrument")
            .category(),
        AppErrorCategory::InvalidInput
    );

    let mut bad_instrument = backtest_spec("run-2", &bars, &root);
    bad_instrument.instrument = "not a symbol".to_string();
    assert_eq!(
        bad_instrument
            .validate()
            .expect_err("非法 instrument")
            .category(),
        AppErrorCategory::InvalidInput
    );

    let mut no_cash = backtest_spec("run-3", &bars, &root);
    no_cash.initial_cash_raw = 0;
    assert_eq!(
        no_cash.validate().expect_err("本金必须为正").category(),
        AppErrorCategory::InvalidInput
    );

    let mut no_currency = backtest_spec("run-4", &bars, &root);
    no_currency.settlement_currency = String::new();
    assert_eq!(
        no_currency.validate().expect_err("结算币种必填").category(),
        AppErrorCategory::InvalidInput
    );

    // 策略形状：身份非空、数量为正、窗口不退化——三条都归调用方的输入问题。
    let mut no_strategy_id = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "");
    no_strategy_id.fast_window = 2;
    no_strategy_id.slow_window = 3;
    assert_eq!(
        no_strategy_id
            .validate()
            .expect_err("策略身份非空")
            .category(),
        AppErrorCategory::InvalidInput
    );

    let mut zero_qty = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "s");
    zero_qty.quantity_raw = 0;
    assert_eq!(
        zero_qty.validate().expect_err("数量必须为正").category(),
        AppErrorCategory::InvalidInput
    );

    let mut zero_window = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "s");
    zero_window.slow_window = 0;
    assert_eq!(
        zero_window.validate().expect_err("窗口不能为 0").category(),
        AppErrorCategory::InvalidInput
    );
}

#[test]
fn the_bar_floor_is_at_least_two_bars() {
    const {
        assert!(
            MIN_BACKTEST_BARS >= 2,
            "少于两根 Bar 就没有'上一根'，Bar 策略唯一被允许看到的东西不存在——\
         这个下限被降到 0/1 就说明档位闸被拆了"
        );
    }
}

#[test]
fn the_output_types_round_trip_and_keep_their_key_sets() {
    let verdict = DatasetVerdict {
        dataset_id: "ds-1".into(),
        instrument: "BTCUSDT.BINANCE".into(),
        rows: 20,
        content_fingerprint: "barframe:0000000000000001".into(),
        usable: true,
        gaps: Vec::new(),
    };
    let payload = verdict.to_json().expect("序列化");
    assert_eq!(
        keys_of(&payload),
        key_set(&[
            "dataset_id",
            "instrument",
            "rows",
            "content_fingerprint",
            "usable",
            "gaps",
        ])
    );
    let back: DatasetVerdict = serde_json::from_str(&payload).expect("反序列化");
    assert_eq!(back, verdict);

    let artifacts = BacktestArtifacts {
        run_manifest: "run.json".into(),
        summary: "summary.json".into(),
        equity: "equity.csv".into(),
        fills: "fills.csv".into(),
    };
    let outcome = BacktestOutcome {
        run_id: "run-1".into(),
        instrument: "BTCUSDT.BINANCE".into(),
        result_hash: "00000000000000ab".into(),
        data_fingerprint: "barframe:0000000000000001".into(),
        fills: 4,
        equity_points: 20,
        return_bps: -12,
        max_drawdown_bps: 34,
        artifacts: artifacts.clone(),
    };
    let payload = outcome.to_json().expect("序列化");
    assert_eq!(
        keys_of(&payload),
        key_set(&[
            "run_id",
            "instrument",
            "result_hash",
            "data_fingerprint",
            "fills",
            "equity_points",
            "return_bps",
            "max_drawdown_bps",
            "artifacts",
        ])
    );
    assert_eq!(
        keys_of(&serde_json::to_string(&artifacts).expect("序列化")),
        key_set(&["run_manifest", "summary", "equity", "fills"])
    );
    let back: BacktestOutcome = serde_json::from_str(&payload).expect("反序列化");
    assert_eq!(back, outcome);

    let result = VerificationResult {
        run_id: "run-1".into(),
        result_hash: "00000000000000ab".into(),
        data_fingerprint: "barframe:0000000000000001".into(),
        verified: false,
        checks: vec!["run_id 一致".into()],
        mismatches: vec!["fills.csv 行数对不上".into()],
    };
    let payload = result.to_json().expect("序列化");
    assert_eq!(
        keys_of(&payload),
        key_set(&[
            "run_id",
            "result_hash",
            "data_fingerprint",
            "verified",
            "checks",
            "mismatches",
        ])
    );
    let back: VerificationResult = serde_json::from_str(&payload).expect("反序列化");
    assert_eq!(back, result);
}
