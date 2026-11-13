//! T2-1 的规格契约：三个入口交出同一份 JSON 字节，所以 spec 的**形状**本身就是公共面。
//!
//! 这一族守两件事：
//!
//! 1. **严格性**——多一个键、schema 版本不对、身份串带路径分隔符，都必须当场拒；静默忽略拼错字段会误导调用方。
//! 2. **稳定性**——三个入口逐字节比对 spec 键集；增删字段必须同时改三侧。

mod common;

use common::{backtest_spec, dataset_spec, scratch};
use qx_app::{
    AppError, AppErrorCategory, BacktestArtifacts, BacktestOutcome, BacktestSpec,
    BuiltinStrategySpec, DatasetSpec, DatasetVerdict, DepthBacktestOutcome, DepthBacktestSpec,
    MultiLegBacktestSpec, VerificationResult, BACKTEST_SPEC_EXACT_SCHEMA_VERSION,
    BACKTEST_SPEC_SCHEMA_VERSION, DATASET_SPEC_SCHEMA_VERSION, MIN_BACKTEST_BARS,
};
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
fn rejected(result: Result<(), AppError>, context: &str) -> AppErrorCategory {
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
fn exact_initial_capital_uses_v2_and_checks_its_legacy_projection() {
    let root = scratch("exact-capital-v2");
    let bars = root.join("bars.json").to_string_lossy().into_owned();
    let base = backtest_spec("exact-capital", &bars, &root);
    let exact = qx_core::ExactDecimal::parse("100000.0000000001").expect("decimal text");
    let spec = base
        .with_initial_cash_exact(exact)
        .expect("v2 capital projection fits");

    assert_eq!(spec.schema_version, BACKTEST_SPEC_EXACT_SCHEMA_VERSION);
    assert_eq!(spec.initial_cash_exact, Some(exact));
    assert_eq!(spec.initial_cash_raw, 100_000 * qx_core::SCALE);
    spec.validate().expect("v2 exact capital validates");

    let payload = spec.to_json().expect("serialize v2 spec");
    assert_eq!(
        BacktestSpec::from_json(&payload).expect("parse v2 spec"),
        spec
    );

    let mut mismatched = spec;
    mismatched.initial_cash_raw += 1;
    assert_eq!(
        rejected(mismatched.validate(), "mismatched exact capital projection"),
        AppErrorCategory::InvalidInput
    );
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
            "period",
            "threshold_bps",
        ]),
        "内置策略声明覆盖四类公开信号旋钮"
    );
}

#[test]
fn bar_market_spec_must_match_the_run_instrument_and_settlement_currency() {
    let mut spec = backtest_spec(
        "run-market-contract",
        "bars.json",
        &scratch("market-contract"),
    );
    let market = |instrument: &str, settlement: &str| qx_core::TradingInstrumentSpec {
        instrument: qx_core::InstrumentId::parse(instrument).unwrap(),
        product: qx_core::TradingProduct::Spot,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: settlement.into(),
        contract_size: qx_core::SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        min_notional_raw: 0,
        max_leverage: 1,
        maintenance_margin_bps: 0,
        valid_from: 0,
        valid_to: None,
        exact_rules: None,
    };

    spec.market_spec = Some(market("ETHUSDT.BINANCE", "USDT"));
    let error = spec.validate().expect_err("市场规格标的不一致必须前置拒绝");
    assert!(error.message().contains("instrument 必须"));

    spec.market_spec = Some(market("BTCUSDT.BINANCE", "USD"));
    let error = spec
        .validate()
        .expect_err("市场规格结算币种不一致必须前置拒绝");
    assert!(error.message().contains("settlement_currency 必须"));

    spec.market_spec = Some(market("BTCUSDT.BINANCE", "USDT"));
    let error = spec
        .validate()
        .expect_err("显式市场规格不得静默套用默认费率");
    assert!(error.message().contains("必须显式提供 fee_schedule"));
}

#[test]
fn bar_fee_schedule_rejects_rates_outside_basis_point_bounds() {
    let mut spec = backtest_spec("run-invalid-fees", "bars.json", &scratch("invalid-fees"));
    spec.fee_schedule = Some(qx_core::MakerTakerFeeSchedule {
        maker_fee_bps: 10_001,
        taker_fee_bps: 5,
    });
    let error = spec.validate().expect_err("超范围费用必须在回测前拒绝");
    assert!(error.message().contains("0..=10000 bps"));
}

#[test]
fn explicit_account_id_is_optional_backward_compatible_and_validated() {
    let root = scratch("spec-account");
    let mut spec = backtest_spec("run-account", "bars.json", &root);
    assert!(!spec.to_json().expect("legacy JSON").contains("account_id"));

    spec.account_id = Some("portfolio.main-1".into());
    spec.validate().expect("valid account token");
    let payload = spec.to_json().expect("explicit account JSON");
    assert_eq!(
        serde_json::from_str::<Value>(&payload).expect("JSON")["account_id"],
        "portfolio.main-1"
    );
    assert_eq!(BacktestSpec::from_json(&payload).expect("round trip"), spec);

    for bad in ["", "../escape", "account/1", "账户"] {
        spec.account_id = Some(bad.into());
        assert_eq!(
            rejected(spec.validate(), &format!("account_id={bad:?}")),
            AppErrorCategory::InvalidInput
        );
    }
}

#[test]
fn old_strategy_documents_get_the_historical_signal_defaults() {
    let payload = r#"{"kind":"sma_cross","strategy_id":"compat","quantity_raw":1000000000,"fast_window":5,"slow_window":20}"#;
    let strategy: BuiltinStrategySpec =
        serde_json::from_str(payload).expect("旧 schema 文档仍可读");
    assert_eq!(strategy.period, 14);
    assert_eq!(strategy.threshold_bps, 100);
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
fn depth_backtest_spec_uses_the_registered_strict_v1_wire_contract() {
    let payload = r#"{
        "schema_version": 1,
        "run_id": "depth-v1",
        "depth_path": "depth.json",
        "tier": "l2",
        "settlement_currency": "USDT",
        "initial_cash_raw": 10000,
        "latency_snapshots": 0,
        "queue_position_bps": 0,
        "market_impact_bps": 0,
        "output_dir": "runs",
        "strategy": {
            "kind": "sma_cross",
            "strategy_id": "depth-v1",
            "quantity_raw": 1,
            "fast_window": 5,
            "slow_window": 20
        }
    }"#;
    let spec = DepthBacktestSpec::from_json(payload).expect("valid legacy v1 depth spec");
    spec.validate().expect("minimal depth spec validates");
    let serialized = spec.to_json().expect("serialize depth spec");
    assert!(!serialized.contains("queue_position_model"));
    assert!(!serialized.contains("fee_bps"));
    assert_eq!(
        DepthBacktestSpec::from_json(&serialized).expect("round trip"),
        spec
    );

    let unknown = payload.replace(
        "\"tier\": \"l2\",",
        "\"tier\": \"l2\", \"legacy_typo\": true,",
    );
    assert_eq!(
        DepthBacktestSpec::from_json(&unknown)
            .expect_err("unknown top-level field must be rejected")
            .category(),
        AppErrorCategory::InvalidInput
    );
}

#[test]
fn depth_outcome_keeps_manifest_identity_optional_for_legacy_payloads() {
    let legacy = serde_json::json!({
        "run_id": "friendly-run-id",
        "instrument": "BTCUSDT.BINANCE",
        "tier": "l2",
        "result_hash": "0123456789abcdef",
        "data_fingerprint": "depthframe:0123456789abcdef",
        "fills": 0,
        "equity_points": 1,
        "snapshots": 1,
        "return_bps": 0,
        "max_drawdown_bps": 0,
        "artifacts": {
            "run_manifest": "runs/depth.run.json",
            "summary": "runs/depth.summary.json",
            "equity": "runs/depth.equity.csv",
            "fills": "runs/depth.fills.csv"
        }
    });
    let legacy_outcome = DepthBacktestOutcome::from_json(&legacy.to_string())
        .expect("legacy outcome without manifest_run_id remains readable");
    assert_eq!(legacy_outcome.manifest_run_id, None);
    assert!(!legacy_outcome
        .to_json()
        .expect("serialize legacy outcome")
        .contains("manifest_run_id"));

    let current = serde_json::json!({
        "run_id": "friendly-run-id",
        "manifest_run_id": "depth-backtest:l2:sma_cross:BTCUSDT.BINANCE",
        "instrument": "BTCUSDT.BINANCE",
        "tier": "l2",
        "result_hash": "0123456789abcdef",
        "data_fingerprint": "depthframe:0123456789abcdef",
        "fills": 0,
        "equity_points": 1,
        "snapshots": 1,
        "return_bps": 0,
        "max_drawdown_bps": 0,
        "artifacts": {
            "run_manifest": "runs/depth.run.json",
            "summary": "runs/depth.summary.json",
            "equity": "runs/depth.equity.csv",
            "fills": "runs/depth.fills.csv"
        }
    });
    let current_outcome = DepthBacktestOutcome::from_json(&current.to_string())
        .expect("current outcome carries a distinct manifest identity");
    assert_eq!(
        current_outcome.manifest_run_id.as_deref(),
        Some("depth-backtest:l2:sma_cross:BTCUSDT.BINANCE")
    );
}

#[test]
fn multi_leg_backtest_spec_uses_the_registered_strict_v1_wire_contract() {
    let payload = r#"{
        "schema_version": 1,
        "run_id": "multi-leg-v1",
        "primary_bars_path": "primary.json",
        "reference_bars_path": "reference.json",
        "settlement_currency": "USDT",
        "primary_initial_cash_raw": 100000,
        "reference_initial_cash_raw": 100000,
        "seed": 0,
        "output_dir": "runs",
        "strategy": {
            "kind": "pairs_arbitrage",
            "strategy_id": "multi-leg-v1",
            "quantity_raw": 1,
            "fast_window": 2,
            "slow_window": 5
        }
    }"#;
    let spec = MultiLegBacktestSpec::from_json(payload).expect("valid v1 two-leg spec");
    let serialized = spec.to_json().expect("serialize multi-leg spec");
    assert_eq!(
        MultiLegBacktestSpec::from_json(&serialized).expect("round trip"),
        spec
    );
    assert!(!serialized.contains("funding_events"));
    assert!(serialized.contains("\"fill_model\":\"next_bar_open\""));

    let unknown = payload.replace("\"seed\": 0,", "\"seed\": 0, \"legacy_typo\": true,");
    assert_eq!(
        MultiLegBacktestSpec::from_json(&unknown)
            .expect_err("unknown top-level field must be rejected")
            .category(),
        AppErrorCategory::InvalidInput
    );
}

#[test]
fn multi_leg_backtest_spec_v2_preserves_subnano_principal_per_leg() {
    let spec = MultiLegBacktestSpec::new(
        qx_app::MultiLegDatasetInput {
            run_id: "multi-leg-v2".into(),
            primary_bars_path: "primary.json".into(),
            reference_bars_path: "reference.json".into(),
        },
        "USDT",
        1,
        1,
        0,
        "runs",
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "multi-leg-v2"),
    )
    .with_initial_cash_exact(
        qx_core::ExactDecimal::parse("0.0000000002").unwrap(),
        qx_core::ExactDecimal::parse("0.0000000002").unwrap(),
    )
    .expect("build exact v2 spec");
    assert_eq!(
        spec.schema_version,
        qx_app::MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION
    );
    assert_eq!(spec.primary_initial_cash_raw, 0);
    assert_eq!(spec.reference_initial_cash_raw, 0);
    let (total, raw) = spec.combined_initial_cash().unwrap();
    assert_eq!(total.to_string(), "0.0000000004");
    assert_eq!(raw, 0);
    let json = spec.to_json().unwrap();
    assert_eq!(MultiLegBacktestSpec::from_json(&json).unwrap(), spec);

    let mut mismatched_projection = spec;
    mismatched_projection.primary_initial_cash_raw = 1;
    assert!(mismatched_projection.validate().is_err());
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
    for bad in ["../escape", "..", ".", "a/b", ""] {
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

    let mut zero_notional = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "s");
    zero_notional.target_notional_raw = Some(0);
    assert_eq!(
        zero_notional
            .validate()
            .expect_err("名义金额必须为正")
            .category(),
        AppErrorCategory::InvalidInput
    );

    let mut invalid_margin_bps = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "s");
    invalid_margin_bps.target_available_margin_bps = Some(10_001);
    assert_eq!(
        invalid_margin_bps
            .validate()
            .expect_err("available margin bps 必须在范围内")
            .category(),
        AppErrorCategory::InvalidInput
    );

    let mut invalid_volatility_bps = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "s");
    invalid_volatility_bps.target_volatility_bps = Some(0);
    assert_eq!(
        invalid_volatility_bps
            .validate()
            .expect_err("波动率预算必须在范围内")
            .category(),
        AppErrorCategory::InvalidInput
    );

    let mut conflicting_sizing = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "s");
    conflicting_sizing.target_notional_raw = Some(100);
    conflicting_sizing.target_available_margin_bps = Some(1_000);
    assert_eq!(
        conflicting_sizing
            .validate()
            .expect_err("sizing 模式互斥")
            .category(),
        AppErrorCategory::InvalidInput
    );

    let mut conflicting_volatility = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "s");
    conflicting_volatility.target_available_margin_bps = Some(1_000);
    conflicting_volatility.target_volatility_bps = Some(100);
    assert_eq!(
        conflicting_volatility
            .validate()
            .expect_err("可用资金比例与波动率 sizing 互斥")
            .category(),
        AppErrorCategory::InvalidInput
    );

    let mut invalid_kelly_fraction = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "s");
    invalid_kelly_fraction.target_kelly_fraction_bps = Some(10_001);
    assert_eq!(
        invalid_kelly_fraction
            .validate()
            .expect_err("Kelly fraction 超范围")
            .category(),
        AppErrorCategory::InvalidInput
    );

    let mut invalid_kelly_lookback = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "s");
    invalid_kelly_lookback.target_kelly_fraction_bps = Some(5_000);
    invalid_kelly_lookback.kelly_lookback_bars = Some(29);
    assert_eq!(
        invalid_kelly_lookback
            .validate()
            .expect_err("Kelly lookback 至少 30 根")
            .category(),
        AppErrorCategory::InvalidInput
    );

    let mut conflicting_kelly = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "s");
    conflicting_kelly.target_available_margin_bps = Some(1_000);
    conflicting_kelly.target_kelly_fraction_bps = Some(5_000);
    assert_eq!(
        conflicting_kelly
            .validate()
            .expect_err("Kelly 与其他 sizing 互斥")
            .category(),
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
        event_trace: None,
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
        account_id: None,
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
