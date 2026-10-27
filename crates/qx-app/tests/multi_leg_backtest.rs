mod common;

use common::{scratch, write_frame};
use qx_app::{
    run_multi_leg_backtest, AppErrorCategory, BuiltinStrategySpec, CallerCapability,
    MultiLegBacktestSpec, RunContext,
};
use qx_core::{InstrumentId, TradingInstrumentSpec, TradingProduct, SCALE};
use qx_core::{Quantity, Side};
use qx_strategy::{
    BuiltinStrategyKind, MarketEvent, Strategy, StrategyContext, StrategyDecision,
    StrategyOrderIntent, STRATEGY_API_VERSION,
};
use serde_json::Value;

#[path = "multi_leg_backtest/shared_execution.rs"]
mod shared_execution;
#[path = "multi_leg_backtest/sizing_rejection.rs"]
mod sizing_rejection;
#[path = "multi_leg_backtest/spec_validation.rs"]
mod spec_validation;
#[path = "multi_leg_backtest/unaligned_timestamps.rs"]
mod unaligned_timestamps;

struct BuyEachLeg(std::collections::BTreeSet<String>, i128);

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
        let intents = if self.0.insert(instrument.to_string()) {
            vec![StrategyOrderIntent {
                intent_id: self.0.len() as u64,
                instrument,
                side: Side::Buy,
                qty: Quantity::from_raw(self.1),
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

struct ObserveLegCash(std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, i128>>>);

impl Strategy for ObserveLegCash {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let instrument = event.instrument().ok_or("event instrument missing")?;
        if context.venue_id != instrument.venue.to_string() {
            return Err("strategy context venue differs from event instrument".into());
        }
        let instrument = instrument.to_string();
        let cash = context
            .cash
            .get("USDT")
            .copied()
            .ok_or("settlement cash missing")?;
        self.0
            .lock()
            .map_err(|_| "cash observer poisoned")?
            .insert(instrument, cash);
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

struct NoOrders;

impl Strategy for NoOrders {
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

fn context(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

#[test]
fn multi_leg_quality_manifests_are_verified_and_bound_into_the_artifact() {
    let root = scratch("multi-leg-quality");
    let primary = write_frame(&root, "primary.json", "BTCUSDT.BINANCE", 80);
    let reference = write_frame(&root, "reference.json", "ETHUSDT.KRAKEN", 80);
    let primary_manifest_path = root.join("primary.quality.json");
    let reference_manifest_path = root.join("reference.quality.json");
    let primary_manifest = qx_app::validate_dataset_manifest(
        &qx_app::DatasetManifestValidationSpec::new(
            &primary,
            "research:multi-leg-primary",
            "daily-v1",
            1_000,
            "UTC",
            primary_manifest_path.to_string_lossy(),
        ),
        &context("multi-leg-primary-manifest"),
    )
    .expect("publish primary manifest");
    let reference_manifest = qx_app::validate_dataset_manifest(
        &qx_app::DatasetManifestValidationSpec::new(
            &reference,
            "research:multi-leg-reference",
            "daily-v1",
            1_000,
            "UTC",
            reference_manifest_path.to_string_lossy(),
        ),
        &context("multi-leg-reference-manifest"),
    )
    .expect("publish reference manifest");
    let mut spec = MultiLegBacktestSpec::new(
        qx_app::MultiLegDatasetInput {
            run_id: "multi-leg-quality".into(),
            primary_bars_path: primary,
            reference_bars_path: reference,
        },
        "USDT",
        1_000_000 * SCALE,
        1_000_000 * SCALE,
        7,
        root.join("out").to_string_lossy(),
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "quality-pair"),
    );
    spec.primary_dataset_manifest_path = Some(primary_manifest_path.to_string_lossy().into_owned());
    spec.reference_dataset_manifest_path =
        Some(reference_manifest_path.to_string_lossy().into_owned());
    let outcome = run_multi_leg_backtest(&spec, &context("multi-leg-quality"))
        .expect("run with verified manifests");
    let report_facts =
        qx_app::read_multi_leg_report_facts_v1(&outcome, &context("multi-leg-quality-report"))
            .expect("report reader verifies both quality manifests through the artifact port");
    assert_eq!(
        report_facts.total_fills,
        outcome.primary_fills + outcome.reference_fills
    );
    let artifact: Value = serde_json::from_str(
        &std::fs::read_to_string(outcome.attribution_artifact).expect("artifact exists"),
    )
    .expect("artifact JSON");
    assert_eq!(
        artifact.pointer("/dataset_quality/primary/manifest_fingerprint"),
        Some(&Value::String(
            primary_manifest.manifest.fingerprint().unwrap()
        ))
    );
    assert_eq!(
        artifact.pointer("/dataset_quality/reference/manifest_fingerprint"),
        Some(&Value::String(
            reference_manifest.manifest.fingerprint().unwrap()
        ))
    );
    let primary_frame = qx_datastruct::BarFrame::from_json(
        &std::fs::read_to_string(&spec.primary_bars_path).expect("read primary frame"),
    )
    .expect("valid primary frame");
    let primary_evidence = &artifact["dataset_quality"]["primary"];
    let primary_fingerprint = qx_data::fingerprint_bar_frame(&primary_frame).unwrap();
    assert_eq!(
        qx_app::verify_multi_leg_report_quality_evidence_v1(
            primary_manifest_path.to_str().unwrap(),
            &spec.primary_bars_path,
            &primary_frame,
            &primary_fingerprint,
            primary_manifest_path.to_str().unwrap(),
            primary_evidence,
        )
        .expect("application report verifier accepts the published quality evidence"),
        primary_manifest.manifest.fingerprint().unwrap()
    );
    let mut changed_evidence = primary_evidence.clone();
    changed_evidence["manifest_sha256"] = Value::String("0".repeat(64));
    assert!(qx_app::verify_multi_leg_report_quality_evidence_v1(
        primary_manifest_path.to_str().unwrap(),
        &spec.primary_bars_path,
        &primary_frame,
        &primary_fingerprint,
        primary_manifest_path.to_str().unwrap(),
        &changed_evidence,
    )
    .is_err());
    assert!(artifact["data_fingerprint"]
        .as_str()
        .unwrap()
        .contains(":quality:"));

    let mut tampered = primary_manifest.manifest;
    tampered.quality_report.missing_intervals = 1;
    std::fs::write(&primary_manifest_path, tampered.to_json().unwrap())
        .expect("tamper quality claim");
    spec.run_id = "multi-leg-quality-tampered".into();
    spec.output_dir = root.join("tampered-out").to_string_lossy().into_owned();
    let error = run_multi_leg_backtest(&spec, &context("multi-leg-quality-tampered"))
        .expect_err("tampered quality report is rejected");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(!root.join("tampered-out").exists());
}

#[test]
fn multi_leg_exact_principal_runs_and_report_reader_keeps_subnano_cash() {
    let root = scratch("multi-leg-exact-principal");
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let primary = fixtures.join("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixtures.join("qianxing.bar-frame.pairs-reference.example.json");
    let spec = MultiLegBacktestSpec::new(
        qx_app::MultiLegDatasetInput {
            run_id: "multi-leg-exact-principal".into(),
            primary_bars_path: primary.to_string_lossy().into_owned(),
            reference_bars_path: reference.to_string_lossy().into_owned(),
        },
        "USDT",
        1,
        1,
        7,
        root.join("out").to_string_lossy(),
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "exact-cash"),
    )
    .with_initial_cash_exact(
        qx_core::ExactDecimal::parse("0.0000000002").unwrap(),
        qx_core::ExactDecimal::parse("0.0000000002").unwrap(),
    )
    .expect("valid exact principal v2");
    let identity = qx_app::StrategyArtifactIdentity {
        strategy_id: "exact-cash".into(),
        strategy_version: "test-v1".into(),
        source_sha256: "a".repeat(64),
    };
    let outcome = qx_app::run_multi_leg_backtest_with_strategy(
        &spec,
        &identity,
        Box::new(NoOrders),
        &context("multi-leg-exact-principal"),
    )
    .expect("exact v2 run succeeds with zero raw projection");
    let artifact: Value =
        serde_json::from_str(&std::fs::read_to_string(&outcome.attribution_artifact).unwrap())
            .unwrap();
    assert_eq!(artifact["schema_version"], 6);
    assert_eq!(artifact["initial_cash_exact"], "0.0000000004");
    assert_eq!(
        artifact["leg_initial_cash_exact"]["primary"],
        "0.0000000002"
    );
    assert_eq!(artifact["equity_curve"][0][1], "0");
    assert_eq!(artifact["equity_curve_exact"][0][2], "0.0000000004");
    let report = qx_app::read_multi_leg_report_facts_v1(
        &outcome,
        &context("multi-leg-exact-principal-report"),
    )
    .expect("reader verifies exact principal, ledger, and equity sidecar");
    assert_eq!(report.initial_cash_raw, 0);
    assert_eq!(report.initial_cash_exact.as_deref(), Some("0.0000000004"));
    assert_eq!(report.primary.return_bps, 0);
    assert_eq!(report.reference.return_bps, 0);
    let research_report = qx_app::read_multi_leg_research_report_v1(
        &qx_app::MultiLegResearchReportReadRequestV1 {
            schema_version: 1,
            outcome,
            page: None,
        },
        &context("multi-leg-exact-principal-interactive-report"),
    )
    .expect("interactive research report includes exact principal");
    assert_eq!(
        research_report.summary.initial_cash_exact.as_deref(),
        Some("0.0000000004")
    );
    assert_eq!(
        research_report.summary.account_ledger_summary["initial_cash_exact"],
        "0.0000000004"
    );
}

#[test]
fn caller_strategy_runs_both_legs_and_cancellable_sdk_surface_can_be_shared() {
    let root = scratch("multi-leg-custom-strategy");
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let primary = fixtures.join("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixtures.join("qianxing.bar-frame.pairs-reference.example.json");
    let strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "custom-pair");
    let spec = MultiLegBacktestSpec::new(
        qx_app::MultiLegDatasetInput {
            run_id: ("custom-pair-run").into(),
            primary_bars_path: (primary.to_string_lossy()).into(),
            reference_bars_path: (reference.to_string_lossy()).into(),
        },
        "USDT",
        1_000_000 * SCALE,
        1_000_000 * SCALE,
        7,
        root.join("out").to_string_lossy(),
        strategy,
    );
    let identity = qx_app::StrategyArtifactIdentity {
        strategy_id: "custom-pair".into(),
        strategy_version: "user-v1".into(),
        source_sha256: "d".repeat(64),
    };
    let outcome = qx_app::run_multi_leg_backtest_with_strategy(
        &spec,
        &identity,
        Box::new(BuyEachLeg(Default::default(), SCALE)),
        &context("custom-pair-run"),
    )
    .expect("caller strategy goes through both shared leg engines");
    assert!(outcome.primary_fills > 0 && outcome.reference_fills > 0);
    let artifact = std::fs::read_to_string(outcome.attribution_artifact).expect("artifact");
    assert!(artifact.contains("\"strategy_version\":\"user-v1\""));
    assert!(artifact.contains(&format!(
        "\"strategy_source_sha256\":\"{}\"",
        "d".repeat(64)
    )));
    let mut changed_identity = identity.clone();
    changed_identity.source_sha256 = "e".repeat(64);
    let conflict = qx_app::run_multi_leg_backtest_with_strategy(
        &spec,
        &changed_identity,
        Box::new(BuyEachLeg(Default::default(), SCALE)),
        &context("custom-pair-run"),
    )
    .expect_err("changed callback code cannot overwrite the same run identity");
    assert_eq!(conflict.category(), AppErrorCategory::Conflict);

    let handle_spec = MultiLegBacktestSpec::new(
        qx_app::MultiLegDatasetInput {
            run_id: ("custom-pair-handle").into(),
            primary_bars_path: (primary.to_string_lossy()).into(),
            reference_bars_path: (reference.to_string_lossy()).into(),
        },
        "USDT",
        1_000_000 * SCALE,
        1_000_000 * SCALE,
        7,
        root.join("handle").to_string_lossy(),
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "custom-pair"),
    );
    let mut handle = handle_spec
        .start_with_strategy(
            identity,
            BuyEachLeg(Default::default(), SCALE),
            context("custom-pair-handle"),
        )
        .expect("cancellable callback run starts");
    assert_eq!(handle.wait(), qx_app::RunStatus::Succeeded);
    let result = handle
        .try_take_result()
        .expect("result available")
        .expect("successful run");
    assert!(result.primary_fills > 0 && result.reference_fills > 0);
}

#[test]
fn multi_leg_strategy_context_uses_shared_cash_and_market_venue() {
    let root = scratch("multi-leg-context-cash");
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let primary = fixtures.join("qianxing.bar-frame.pairs-primary.example.json");
    let reference_fixture = fixtures.join("qianxing.bar-frame.pairs-reference.example.json");
    let reference_content = std::fs::read_to_string(&reference_fixture)
        .expect("reference fixture is readable")
        .replace("ETHUSDT.BINANCE", "ETHUSDT.KRAKEN");
    let reference = root.join("reference-kraken.json");
    std::fs::write(&reference, reference_content).expect("cross-venue fixture is writable");
    let primary_cash = 1_000_000 * SCALE;
    let reference_cash = 2_000_000 * SCALE;
    let spec = MultiLegBacktestSpec::new(
        qx_app::MultiLegDatasetInput {
            run_id: ("multi-leg-cash-context").into(),
            primary_bars_path: (primary.to_string_lossy()).into(),
            reference_bars_path: (reference.to_string_lossy()).into(),
        },
        "USDT",
        primary_cash,
        reference_cash,
        7,
        root.join("out").to_string_lossy(),
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "cash-context"),
    );
    let seen_cash = std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let identity = qx_app::StrategyArtifactIdentity {
        strategy_id: "cash-context".into(),
        strategy_version: "test-v1".into(),
        source_sha256: "f".repeat(64),
    };
    qx_app::run_multi_leg_backtest_with_strategy(
        &spec,
        &identity,
        Box::new(ObserveLegCash(seen_cash.clone())),
        &context("multi-leg-cash-context"),
    )
    .expect("context observer run");
    let seen_cash = seen_cash.lock().unwrap();
    let shared_cash = primary_cash + reference_cash;
    assert_eq!(seen_cash.get("BTCUSDT.BINANCE"), Some(&shared_cash));
    assert_eq!(seen_cash.get("ETHUSDT.KRAKEN"), Some(&shared_cash));
}

#[test]
fn multi_leg_sizing_validates_market_semantics_before_run() {
    let mut fixed_notional = MultiLegBacktestSpec::new(
        qx_app::MultiLegDatasetInput {
            run_id: ("multi-leg-fixed-notional").into(),
            primary_bars_path: ("primary.json").into(),
            reference_bars_path: ("reference.json").into(),
        },
        "USDT",
        1_000_000,
        1_000_000,
        7,
        "out",
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "pair-sizing"),
    );
    fixed_notional.strategy.target_notional_raw = Some(100 * SCALE);
    let error = fixed_notional
        .validate()
        .expect_err("fixed notional sizing needs product specs for both legs");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(error
        .to_string()
        .contains("必须配置两腿永续/交割 market spec"));

    fixed_notional.primary_market_spec = Some(derivative_spec("BTCUSDT.BINANCE"));
    fixed_notional.reference_market_spec = Some(derivative_spec("ETHUSDT.BINANCE"));
    fixed_notional
        .validate()
        .expect("linear derivatives have enough frozen terms for quote sizing");

    let mut spot = derivative_spec("BTCUSDT.BINANCE");
    spot.product = TradingProduct::Spot;
    spot.max_leverage = 1;
    spot.maintenance_margin_bps = 0;
    fixed_notional.primary_market_spec = Some(spot);
    let error = fixed_notional
        .validate()
        .expect_err("spot short leg needs borrow and inventory semantics");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(error.to_string().contains("仅支持永续/交割合约"));

    let mut dynamic_sizing = fixed_notional;
    dynamic_sizing.primary_market_spec = Some(derivative_spec("BTCUSDT.BINANCE"));
    dynamic_sizing.strategy.target_notional_raw = None;
    dynamic_sizing.strategy.target_available_margin_bps = Some(2_500);
    let error = dynamic_sizing
        .validate()
        .expect_err("dynamic sizing requires independent, event-current buying power per leg");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(error.to_string().contains("暂不支持共享账户 sizing"));
}

#[test]
fn multi_leg_backtest_runs_shared_strategy_and_matching_and_is_idempotent() {
    let root = scratch("multi-leg-app");
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let primary = fixtures
        .join("qianxing.bar-frame.pairs-primary.example.json")
        .to_string_lossy()
        .into_owned();
    let reference = fixtures
        .join("qianxing.bar-frame.pairs-reference.example.json")
        .to_string_lossy()
        .into_owned();
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "pair-test");
    strategy.quantity_raw = qx_core::SCALE;
    let mut spec = MultiLegBacktestSpec::new(
        qx_app::MultiLegDatasetInput {
            run_id: ("pair-run").into(),
            primary_bars_path: primary,
            reference_bars_path: reference,
        },
        "USDT",
        1_000_000 * qx_core::SCALE,
        1_000_000 * qx_core::SCALE,
        7,
        root.join("out").to_string_lossy(),
        strategy,
    );
    spec.account_id = Some("acct.pair-test".into());
    let first = run_multi_leg_backtest(&spec, &context("pair-run")).expect("shared multi-leg run");
    let second = run_multi_leg_backtest(&spec, &context("pair-run")).expect("idempotent rerun");
    assert_eq!(first, second);
    assert_eq!(first.account_id.as_deref(), Some("acct.pair-test"));
    assert_ne!(first.primary_result_hash, first.reference_result_hash);
    assert!(first.primary_fills > 0 && first.reference_fills > 0);
    let artifact =
        std::fs::read_to_string(&first.attribution_artifact).expect("multi-leg artifact");
    assert!(artifact.contains("pending_reconcile"));
    assert!(artifact.contains("replay_ledger_entries"));
    assert!(artifact.contains("acct.pair-test"));
    let artifact: serde_json::Value = serde_json::from_str(&artifact).expect("valid artifact");
    assert_eq!(artifact["account_ledger"]["account_id"], "acct.pair-test");
    assert_eq!(
        artifact["account_ledger"]["allocation_policy"],
        "shared_buying_power_deterministic_market_priority"
    );
    assert_eq!(artifact["account_ledger"]["cross_leg_buying_power"], true);
    assert_eq!(artifact["account_ledger"]["replay_verified"], true);
    assert_eq!(
        artifact["account_ledger"]["initial_cash_raw"],
        (2_000_000_i128 * qx_core::SCALE).to_string()
    );
    assert_eq!(
        artifact["account_ledger"]["positions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        artifact["account_ledger"]["ledger_entry_count"]
            .as_u64()
            .unwrap()
            > 0
    );
}

fn derivative_spec(instrument: &str) -> TradingInstrumentSpec {
    let (base, quote) = instrument.split_once('.').unwrap().0.split_at(3);
    TradingInstrumentSpec {
        instrument: InstrumentId::parse(instrument).unwrap(),
        product: TradingProduct::Perpetual,
        base_currency: base.into(),
        quote_currency: quote.into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: 1_000,
        qty_step: 1,
        min_qty: 1,
        min_notional_raw: 0,
        max_leverage: 20,
        maintenance_margin_bps: 500,
        valid_from: 0,
        valid_to: None,
        exact_rules: None,
    }
}

fn inverse_btc_spec(instrument: &str) -> TradingInstrumentSpec {
    let mut spec = derivative_spec(instrument);
    spec.base_currency = "BTC".into();
    spec.quote_currency = "USD".into();
    spec.settlement_currency = "BTC".into();
    spec.contract_size = 1_000 * SCALE;
    spec.qty_step = 10_000_000;
    spec.min_qty = 10_000_000;
    spec.linear = false;
    spec.inverse = true;
    spec
}

fn retag_bar_frame(source: &std::path::Path, output: &std::path::Path, instrument: &str) {
    let mut frame: serde_json::Value =
        serde_json::from_slice(&std::fs::read(source).expect("read source BarFrame"))
            .expect("valid source BarFrame");
    frame["instrument"] = serde_json::json!(instrument);
    frame["source"] = serde_json::json!(format!("retagged-{instrument}"));
    std::fs::write(output, frame.to_string()).expect("write retagged BarFrame");
}

#[test]
fn multi_leg_spec_applies_market_specs_fees_funding_and_shared_fill_choice() {
    let root = scratch("multi-leg-configured");
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let primary_path = root.join("primary.json");
    retag_bar_frame(
        &fixtures.join("qianxing.bar-frame.pairs-primary.example.json"),
        &primary_path,
        "BTCUSD.BINANCE",
    );
    let reference_path = root.join("reference.json");
    retag_bar_frame(
        &fixtures.join("qianxing.bar-frame.pairs-reference.example.json"),
        &reference_path,
        "BTCUSD.OKX",
    );
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "pair-costs");
    strategy.quantity_raw = SCALE;
    strategy.target_notional_raw = Some(100 * SCALE);
    let mut spec = MultiLegBacktestSpec::new(
        qx_app::MultiLegDatasetInput {
            run_id: ("pair-configured").into(),
            primary_bars_path: (primary_path.to_string_lossy()).into(),
            reference_bars_path: (reference_path.to_string_lossy()).into(),
        },
        "BTC",
        10 * SCALE,
        10 * SCALE,
        7,
        root.join("out").to_string_lossy(),
        strategy,
    );
    spec.primary_market_spec = Some(inverse_btc_spec("BTCUSD.BINANCE"));
    spec.reference_market_spec = Some(inverse_btc_spec("BTCUSD.OKX"));
    spec.maker_fee_bps = 3;
    spec.taker_fee_bps = 10;
    spec.primary_taker_fee_bps = Some(12);
    spec.funding_bps = 100;
    spec.fill_model = qx_app::MultiLegFillModel::OneTickSlippage;

    let outcome = run_multi_leg_backtest(&spec, &context("pair-configured"))
        .expect("fixed quote notional converts through both derivative market specs");
    let artifact: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&outcome.attribution_artifact).unwrap())
            .unwrap();
    assert!(artifact["attribution_totals"]["funding_raw"] != "0");
    assert!(!artifact["execution_funding_events"]
        .as_array()
        .expect("execution funding is listed as replayable events")
        .is_empty());
    assert!(artifact["shared_event_log"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| { event["kind"]["LedgerApplied"]["entry"]["kind"] == "Funding" }));
    assert!(artifact["attribution_totals"]["margin_peak_raw"] != "0");
    assert_eq!(artifact["fill_model"], "one_tick_slippage");
    assert_eq!(artifact["fee_model"]["primary"]["maker_fee_bps"], 3);
    assert_eq!(artifact["fee_model"]["primary"]["taker_fee_bps"], 12);
    assert_eq!(artifact["fee_model"]["reference"]["taker_fee_bps"], 10);
    assert!(outcome.primary_fills > 0 && outcome.reference_fills > 0);
}

#[test]
fn multi_leg_funding_and_one_tick_fill_fail_closed_without_market_specs() {
    let root = scratch("multi-leg-missing-spec");
    let primary = common::write_frame(&root, "primary.json", "BTCUSDT.BINANCE", 40);
    let reference = common::write_frame(&root, "reference.json", "ETHUSDT.OKX", 40);
    let strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "pair-test");
    let mut spec = MultiLegBacktestSpec::new(
        qx_app::MultiLegDatasetInput {
            run_id: ("pair-missing-spec").into(),
            primary_bars_path: primary,
            reference_bars_path: reference,
        },
        "USDT",
        1_000_000,
        1_000_000,
        0,
        root.join("out").to_string_lossy(),
        strategy,
    );
    spec.funding_bps = 1;
    let error = run_multi_leg_backtest(&spec, &context("pair-missing-spec"))
        .expect_err("funding cannot guess products or contract size");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);

    spec.funding_bps = 0;
    spec.fill_model = qx_app::MultiLegFillModel::OneTickSlippage;
    let error = run_multi_leg_backtest(&spec, &context("pair-missing-spec"))
        .expect_err("one-tick slippage cannot guess a tick size");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
}
