mod common;

use common::{frame_json, scratch};
use qx_app::{
    run_multi_builtin_backtest, run_multi_builtin_backtest_with_inputs,
    validate_multi_builtin_frames, AppErrorCategory, BacktestDatasetInput, BacktestSpec,
    BuiltinStrategySpec, CallerCapability, MultiBuiltinBacktestInputs, MultiBuiltinBacktestSpec,
    RunContext, MULTI_BUILTIN_BACKTEST_SCHEMA_VERSION,
};
use qx_core::{InstrumentId, TradingInstrumentSpec, TradingProduct, SCALE};
use qx_datastruct::BarFrame;
use qx_strategy::{BuiltinStrategyConfig, BuiltinStrategyKind};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn deploy_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
}

fn leg_spec(
    run_id: &str,
    instrument: &str,
    bars_path: &Path,
    output_dir: &Path,
    initial_cash_raw: i128,
    account_id: &str,
) -> BacktestSpec {
    let mut spec = BacktestSpec::new(
        BacktestDatasetInput {
            run_id: run_id.into(),
            instrument: instrument.into(),
            bars_path: bars_path.to_string_lossy().into_owned(),
        },
        "USDT",
        initial_cash_raw,
        20260914,
        output_dir.to_string_lossy().into_owned(),
        BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, format!("{run_id}-scheduled")),
    );
    spec.account_id = Some(account_id.into());
    // The historical multi-builtin chain allows the short leg, including when a market spec is
    // omitted and the market therefore follows the spot multiplier-1 compatibility rule.
    spec.allow_short = Some(true);
    spec
}

fn spec_for(instrument: &str, product: TradingProduct) -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument: InstrumentId::parse(instrument).expect("valid test instrument"),
        product,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: product.is_derivative(),
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        min_notional_raw: 0,
        max_leverage: if product == TradingProduct::Spot {
            1
        } else {
            20
        },
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
    }
}

fn validation_spec() -> MultiBuiltinBacktestSpec {
    let root = Path::new("target/multi-builtin-spec-validation");
    MultiBuiltinBacktestSpec {
        schema_version: MULTI_BUILTIN_BACKTEST_SCHEMA_VERSION,
        strategy: BuiltinStrategySpec::new(
            BuiltinStrategyKind::PairsArbitrage,
            "builtin-pairs_arbitrage-multi",
        ),
        primary: leg_spec(
            "primary-validation",
            "BTCUSDT.BINANCE",
            Path::new("primary-bars.json"),
            &root.join("primary"),
            1_000_000,
            "primary-account",
        ),
        reference: leg_spec(
            "reference-validation",
            "ETHUSDT.BINANCE",
            Path::new("reference-bars.json"),
            &root.join("reference"),
            1_000_000,
            "reference-account",
        ),
        funding_bps: 0,
        declared_product: None,
    }
}

#[test]
fn multi_builtin_schema_covers_the_serialized_input_and_supported_strategy_domain() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../schemas/multi-builtin-backtest-spec-v1.json"
    ))
    .expect("schema is valid JSON");
    let spec = serde_json::to_value(validation_spec()).expect("serialize input spec");
    let root_properties = schema["properties"]
        .as_object()
        .expect("schema root properties");
    let root_keys = spec
        .as_object()
        .expect("serialized input object")
        .keys()
        .collect::<std::collections::BTreeSet<_>>();
    assert!(root_keys
        .iter()
        .all(|key| root_properties.contains_key(*key)));
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["$defs"]["MultiLegStrategySpec"]["properties"]["kind"]["enum"],
        serde_json::json!([
            "pairs_arbitrage",
            "basis_arbitrage",
            "cross_venue_arbitrage",
            "spot_futures_arbitrage"
        ])
    );
    let backtest_properties = schema["$defs"]["BacktestSpec"]["properties"]
        .as_object()
        .expect("embedded single-leg spec properties");
    let primary_keys = spec["primary"]
        .as_object()
        .expect("primary leg object")
        .keys()
        .collect::<std::collections::BTreeSet<_>>();
    assert!(primary_keys
        .iter()
        .all(|key| backtest_properties.contains_key(*key)));
}

#[test]
fn multi_leg_strategy_config_projects_application_quantity_and_signal_fields() {
    let mut config = BuiltinStrategyConfig::new_multi_leg(
        BuiltinStrategyKind::PairsArbitrage,
        "builtin-pairs_arbitrage-multi",
        InstrumentId::parse("BTCUSDT.BINANCE").expect("valid primary instrument"),
        InstrumentId::parse("ETHUSDT.BINANCE").expect("valid reference instrument"),
        qx_core::Quantity::from_i64(7),
    )
    .expect("pair config is valid");
    config.fast_window = 8;
    config.slow_window = 24;
    config.period = 11;
    config.threshold_bps = 175;

    let projected = BuiltinStrategySpec::from_multi_leg_strategy_config(&config)
        .expect("validated pair config is represented by the app contract");
    assert_eq!(projected.kind, config.kind);
    assert_eq!(projected.strategy_id, config.strategy_id);
    assert_eq!(projected.quantity_raw, config.quantity.raw());
    assert_eq!(projected.fast_window, 8);
    assert_eq!(projected.slow_window, 24);
    assert_eq!(projected.period, 11);
    assert_eq!(projected.threshold_bps, 175);
}

#[test]
fn facade_preflight_uses_the_shared_multi_builtin_frame_contract() {
    let primary =
        BarFrame::from_json(&frame_json("BTCUSDT.BINANCE", 48)).expect("valid primary input frame");
    let reference =
        BarFrame::from_json(&frame_json("ETHUSDT.BINANCE", 48)).expect("valid reference frame");
    assert!(validate_multi_builtin_frames(
        BuiltinStrategyKind::PairsArbitrage,
        SCALE,
        &primary,
        &reference,
    )
    .is_ok());

    let mut misaligned = reference.clone();
    misaligned.ts[0] += 1;
    assert!(validate_multi_builtin_frames(
        BuiltinStrategyKind::PairsArbitrage,
        SCALE,
        &primary,
        &misaligned,
    )
    .unwrap_err()
    .to_string()
    .contains("时间戳完全对齐"));

    let short_primary = BarFrame::from_json(&frame_json("BTCUSDT.BINANCE", 2))
        .expect("short but structurally valid primary frame");
    let short_reference = BarFrame::from_json(&frame_json("ETHUSDT.BINANCE", 2))
        .expect("short but structurally valid reference frame");
    assert!(validate_multi_builtin_frames(
        BuiltinStrategyKind::PairsArbitrage,
        SCALE,
        &short_primary,
        &short_reference,
    )
    .unwrap_err()
    .to_string()
    .contains("至少需要 3 根对齐 Bar"));
}

#[test]
fn multi_builtin_spec_validates_market_and_funding_rules_for_every_facade() {
    let mut legacy_document = serde_json::to_value(validation_spec()).expect("serialize spec");
    legacy_document
        .as_object_mut()
        .expect("spec serializes as an object")
        .remove("declared_product");
    let legacy_spec: MultiBuiltinBacktestSpec =
        serde_json::from_value(legacy_document).expect("read v1 spec without optional product");
    assert_eq!(legacy_spec.declared_product, None);

    let mut declared_derivative_without_primary_spec = validation_spec();
    declared_derivative_without_primary_spec.declared_product = Some(TradingProduct::Perpetual);
    assert!(declared_derivative_without_primary_spec
        .validate()
        .unwrap_err()
        .to_string()
        .contains("必须提供 primary 腿的 market spec"));

    let mut funding_without_specs = validation_spec();
    funding_without_specs.funding_bps = 25;
    assert!(funding_without_specs
        .validate()
        .unwrap_err()
        .to_string()
        .contains("primary 腿没有 market spec"));

    let mut funding_on_spot_legs = validation_spec();
    funding_on_spot_legs.funding_bps = 25;
    funding_on_spot_legs.primary.market_spec =
        Some(spec_for("BTCUSDT.BINANCE", TradingProduct::Spot));
    funding_on_spot_legs.reference.market_spec =
        Some(spec_for("ETHUSDT.BINANCE", TradingProduct::Spot));
    funding_on_spot_legs.primary.execution_cost_rules =
        Some(qx_xingban::ExecutionCostRules::default());
    funding_on_spot_legs.reference.execution_cost_rules =
        Some(qx_xingban::ExecutionCostRules::default());
    let spot_error = funding_on_spot_legs.validate().unwrap_err().to_string();
    assert!(spot_error.contains("没有一条腿是衍生品"), "{spot_error}");

    let mut mixed_derivative_legs = validation_spec();
    mixed_derivative_legs.funding_bps = 25;
    mixed_derivative_legs.declared_product = Some(TradingProduct::Perpetual);
    mixed_derivative_legs.primary.market_spec =
        Some(spec_for("BTCUSDT.BINANCE", TradingProduct::Perpetual));
    mixed_derivative_legs.reference.market_spec =
        Some(spec_for("ETHUSDT.BINANCE", TradingProduct::Spot));
    mixed_derivative_legs.primary.execution_cost_rules =
        Some(qx_xingban::ExecutionCostRules::default());
    mixed_derivative_legs.reference.execution_cost_rules =
        Some(qx_xingban::ExecutionCostRules::default());
    assert!(mixed_derivative_legs.validate().is_ok());
}

#[test]
fn app_independent_multi_builtin_default_leg_hashes_are_stable() {
    let deploy = deploy_root();
    let primary_path = deploy.join("qianxing.bar-frame.pairs-primary.example.json");
    let reference_path = deploy.join("qianxing.bar-frame.pairs-reference.example.json");
    let root = scratch("independent-multi-builtin");
    let mut strategy = BuiltinStrategySpec::new(
        BuiltinStrategyKind::PairsArbitrage,
        "builtin-pairs_arbitrage-multi",
    );
    strategy.quantity_raw = 1_000_000_000;
    let mut spec = MultiBuiltinBacktestSpec {
        schema_version: MULTI_BUILTIN_BACKTEST_SCHEMA_VERSION,
        strategy,
        primary: leg_spec(
            "multi-builtin-primary",
            "BTCUSDT.BINANCE",
            &primary_path,
            &root.join("primary"),
            126_064_000_000_000,
            "multi-leg-primary",
        ),
        reference: leg_spec(
            "multi-builtin-reference",
            "ETHUSDT.BINANCE",
            &reference_path,
            &root.join("reference"),
            100_000_000_000_000,
            "multi-leg-reference",
        ),
        funding_bps: 0,
        declared_product: None,
    };
    spec.primary.market_spec = Some(spec_for("BTCUSDT.BINANCE", TradingProduct::Perpetual));
    spec.reference.market_spec = Some(spec_for("ETHUSDT.BINANCE", TradingProduct::Perpetual));
    spec.primary.execution_cost_rules = Some(qx_xingban::ExecutionCostRules::default());
    spec.reference.execution_cost_rules = Some(qx_xingban::ExecutionCostRules::default());
    spec.declared_product = Some(TradingProduct::Perpetual);
    let context = RunContext::new(CallerCapability::Research, "multi-builtin-parity");
    let result = run_multi_builtin_backtest(&spec, &context).expect("run app multi-builtin");
    assert_eq!(
        format!("{:016x}", result.primary.report.result_hash()),
        "01d18d51c0dba9b5"
    );
    assert_eq!(
        format!("{:016x}", result.reference.report.result_hash()),
        "ceb83a6d1494e660"
    );
    assert_eq!(result.primary.report.fills.len(), 3);
    assert_eq!(result.reference.report.fills.len(), 3);
    for leg in [&result.primary, &result.reference] {
        let payload = std::fs::read_to_string(&leg.outcome.artifacts.summary)
            .expect("read persisted multi-builtin leg summary");
        let summary: serde_json::Value =
            serde_json::from_str(&payload).expect("decode persisted leg summary");
        assert_eq!(
            summary["risk_rule_set_version"], leg.risk_rule_set_version,
            "app result exposes the exact risk version written to its summary"
        );
    }
    std::fs::remove_dir_all(root).expect("remove independent multi-builtin artifacts");
}

#[test]
fn normalized_multi_builtin_runs_the_supplied_frame_snapshots() {
    let primary =
        BarFrame::from_json(&frame_json("BTCUSDT.BINANCE", 48)).expect("valid primary input frame");
    let reference = BarFrame::from_json(&frame_json("ETHUSDT.BINANCE", 48))
        .expect("valid reference input frame");
    let mut strategy = BuiltinStrategySpec::new(
        BuiltinStrategyKind::PairsArbitrage,
        "builtin-pairs-arbitrage-snapshot",
    );
    strategy.quantity_raw = SCALE;
    let context = RunContext::new(CallerCapability::Research, "multi-builtin-input-snapshot");
    let result = run_multi_builtin_backtest_with_inputs(
        MultiBuiltinBacktestInputs {
            strategy,
            primary_frame: &primary,
            reference_frame: &reference,
            initial_cash_raw: [100_000 * SCALE, 100_000 * SCALE],
            declared_initial_cash_override_raw: None,
            market_specs: [None, None],
            margin_models: [None, None],
            funding_bps: 0,
            declared_product: None,
            execution_cost_rules: qx_xingban::ExecutionCostRules::default(),
            risk_rules: None,
            allow_short: true,
            fill_model: None,
            max_bar_volume_participation_bps: None,
        },
        &context,
    )
    .expect("run from the normalized frame snapshots");
    let expected_primary = format!(
        "barframe:{}",
        qx_data::fingerprint_bar_frame(&primary).expect("fingerprint primary frame")
    );
    let expected_reference = format!(
        "barframe:{}",
        qx_data::fingerprint_bar_frame(&reference).expect("fingerprint reference frame")
    );
    assert_eq!(result.primary.outcome.data_fingerprint, expected_primary);
    assert_eq!(
        result.reference.outcome.data_fingerprint,
        expected_reference
    );
}

#[test]
fn normalized_multi_builtin_rejects_single_account_cash_override_before_running() {
    let primary =
        BarFrame::from_json(&frame_json("BTCUSDT.BINANCE", 48)).expect("valid primary input frame");
    let reference = BarFrame::from_json(&frame_json("ETHUSDT.BINANCE", 48))
        .expect("valid reference input frame");
    let mut strategy = BuiltinStrategySpec::new(
        BuiltinStrategyKind::PairsArbitrage,
        "builtin-pairs-arbitrage-single-cash-rejection",
    );
    strategy.quantity_raw = SCALE;
    let context = RunContext::new(CallerCapability::Research, "multi-builtin-single-cash");
    let error = match run_multi_builtin_backtest_with_inputs(
        MultiBuiltinBacktestInputs {
            strategy,
            primary_frame: &primary,
            reference_frame: &reference,
            initial_cash_raw: [100_000 * SCALE, 100_000 * SCALE],
            declared_initial_cash_override_raw: Some(500_000 * SCALE),
            market_specs: [None, None],
            margin_models: [None, None],
            funding_bps: 0,
            declared_product: None,
            execution_cost_rules: qx_xingban::ExecutionCostRules::default(),
            risk_rules: None,
            allow_short: true,
            fill_model: None,
            max_bar_volume_participation_bps: None,
        },
        &context,
    ) {
        Err(error) => error,
        Ok(_) => panic!("one balance cannot size two independent accounts"),
    };
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(error
        .message()
        .contains("strategy.initial_cash_raw=500000000000000"));
    assert!(error.message().contains("改用 quantity 表达规模"));
}
