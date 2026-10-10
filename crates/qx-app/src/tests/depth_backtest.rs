use crate::{
    run_depth_backtest, verify_depth_run, AppErrorCategory, BuiltinStrategySpec, CallerCapability,
    DepthBacktestSpec, RunContext, RunStatus, DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
};
use qx_core::{
    InstrumentId, MarginMode, OrderPolicy, PositionMode, PositionSide, TradingInstrumentSpec,
    TradingProduct,
};
use qx_strategy::BuiltinStrategyKind;
use std::path::Path;

const SCALE: i128 = 1_000_000_000;

fn write_depth_frame(root: &Path, name: &str, levels: usize) -> String {
    let snapshots = (0..48)
        .map(|index| {
            let phase = index % 20;
            let mid = if phase < 10 {
                100 + phase * 2
            } else {
                100 + (20 - phase) * 2
            };
            let bids = (0..levels)
                .map(|level| {
                    serde_json::json!({
                        "price": (mid - 1 - level as i128) * SCALE,
                        "qty": 100 * SCALE
                    })
                })
                .collect::<Vec<_>>();
            let asks = (0..levels)
                .map(|level| {
                    serde_json::json!({
                        "price": (mid + 1 + level as i128) * SCALE,
                        "qty": 100 * SCALE
                    })
                })
                .collect::<Vec<_>>();
            serde_json::json!({
                "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                "ts": 1000 + index as u64 * 1000,
                "sequence": index as u64 + 1,
                "bids": bids,
                "asks": asks
            })
        })
        .collect::<Vec<_>>();
    let path = root.join(name);
    std::fs::write(
        &path,
        serde_json::json!({
            "schema_version": 1,
            "source": "qx-app-depth-test",
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "snapshots": snapshots
        })
        .to_string(),
    )
    .expect("write depth fixture");
    path.to_string_lossy().into_owned()
}

fn spec(run_id: &str, root: &Path, tier: &str, levels: usize) -> DepthBacktestSpec {
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "depth-sma");
    strategy.quantity_raw = SCALE;
    strategy.fast_window = 2;
    strategy.slow_window = 3;
    DepthBacktestSpec {
        schema_version: DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
        run_id: run_id.into(),
        depth_path: write_depth_frame(root, "depth.json", levels),
        tier: tier.into(),
        settlement_currency: "USDT".into(),
        initial_cash_raw: 1_000_000 * SCALE,
        fee_bps: 0,
        latency_snapshots: 0,
        queue_position_bps: 0,
        market_impact_bps: 0,
        output_dir: root.join("runs").to_string_lossy().into_owned(),
        strategy,
        market_spec: None,
        order_policy: None,
    }
}

fn perpetual_market_spec() -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        product: TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        max_leverage: 100,
        maintenance_margin_bps: 500,
        valid_from: 1_000,
        valid_to: Some(49_000),
    }
}

#[test]
fn l1_tick_use_case_runs_the_rust_engine_and_publishes_verifiable_artifacts() {
    let root = super::fixtures::scratch("app-l1-tick");
    let spec = spec("l1-tick", &root, "l1", 1);
    let context = RunContext::new(CallerCapability::Research, "l1-tick");
    let first = run_depth_backtest(&spec, &context).expect("L1 Tick run");
    let second = run_depth_backtest(&spec, &context).expect("idempotent L1 Tick rerun");
    assert_eq!(first, second);
    let verified = verify_depth_run(&first, &context).expect("verify L1 artifacts");
    assert!(verified.verified, "{:?}", verified.mismatches);
    std::fs::write(&first.artifacts.summary, "{}").expect("tamper summary");
    let tampered = verify_depth_run(&first, &context).expect("tampered artifact is a verdict");
    assert!(!tampered.verified);
    assert!(!tampered.mismatches.is_empty());
    assert_eq!(first.tier, "l1");
    assert_eq!(first.equity_points, 48);
    assert!(first.fills > 0);
    let manifest = std::fs::read_to_string(&first.artifacts.run_manifest).expect("manifest");
    assert!(manifest.contains(&first.result_hash));
    assert!(Path::new(&first.artifacts.summary).is_file());
    assert!(Path::new(&first.artifacts.equity).is_file());
    assert!(Path::new(&first.artifacts.fills).is_file());
}

#[test]
fn l2_order_book_use_case_uses_depth_matching_and_replay_verification() {
    let root = super::fixtures::scratch("app-l2-book");
    let spec = spec("l2-book", &root, "l2", 2);
    let outcome = run_depth_backtest(
        &spec,
        &RunContext::new(CallerCapability::Research, "l2-book"),
    )
    .expect("L2 order-book run");
    assert_eq!(outcome.tier, "l2");
    assert_eq!(outcome.equity_points, 48);
    let summary: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&outcome.artifacts.summary).expect("summary"),
    )
    .expect("summary JSON");
    assert!(summary["model_descriptors"]
        .as_array()
        .expect("descriptors")
        .iter()
        .any(|descriptor| descriptor.as_str().unwrap_or_default().contains("L2L3")));
    assert!(summary["replay_events"].as_u64().unwrap_or_default() > 0);
    let verification = verify_depth_run(
        &outcome,
        &RunContext::new(CallerCapability::Research, "l2-book-verify"),
    )
    .expect("verify L2 artifacts");
    assert!(verification.verified, "{:?}", verification.mismatches);
}

#[test]
fn depth_use_case_uses_market_product_and_explicit_derivative_order_policy() {
    let root = super::fixtures::scratch("app-depth-product-spec");
    let mut request = spec("depth-perpetual", &root, "l1", 1);
    request.market_spec = Some(perpetual_market_spec());
    request.order_policy = Some(OrderPolicy {
        reduce_only: false,
        position_side: PositionSide::Net,
        margin_mode: MarginMode::Cross,
        position_mode: PositionMode::OneWay,
        leverage: 2,
        post_only: false,
    });

    let outcome = run_depth_backtest(
        &request,
        &RunContext::new(CallerCapability::Research, "depth-perpetual"),
    )
    .expect("derivative depth run uses the existing margin-aware kernel");
    assert!(outcome.fills > 0, "orders must carry the configured policy");
    let summary: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&outcome.artifacts.summary).expect("summary"),
    )
    .expect("summary JSON");
    assert_eq!(summary["market_spec"]["product"], "perpetual");
    assert_eq!(summary["order_policy"]["margin_mode"], "cross");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&outcome.artifacts.run_manifest).expect("manifest"),
    )
    .expect("manifest JSON");
    assert_eq!(
        manifest["instrument_spec_version"],
        "trading-instrument-spec-v1"
    );
    assert!(
        verify_depth_run(
            &outcome,
            &RunContext::new(CallerCapability::Research, "depth-perpetual-verify")
        )
        .unwrap()
        .verified
    );
}

#[test]
fn depth_market_spec_and_order_policy_fail_closed_on_missing_or_out_of_range_contracts() {
    let root = super::fixtures::scratch("app-depth-product-guards");
    let mut request = spec("depth-perpetual-guards", &root, "l1", 1);
    request.market_spec = Some(perpetual_market_spec());
    assert_eq!(
        request
            .validate()
            .expect_err("derivative contract must require explicit order policy")
            .category(),
        AppErrorCategory::InvalidInput
    );
    request.order_policy = Some(OrderPolicy {
        reduce_only: false,
        position_side: PositionSide::Net,
        margin_mode: MarginMode::Cross,
        position_mode: PositionMode::OneWay,
        leverage: 2,
        post_only: false,
    });
    request.market_spec.as_mut().unwrap().valid_to = Some(47_000);
    assert_eq!(
        run_depth_backtest(
            &request,
            &RunContext::new(CallerCapability::Research, "depth-perpetual-guards"),
        )
        .expect_err("market spec must cover every input timestamp")
        .category(),
        AppErrorCategory::InvalidInput
    );
}

#[test]
fn depth_run_handle_uses_the_same_tick_application_workflow() {
    let root = super::fixtures::scratch("app-depth-run-handle");
    let mut depth_spec = spec("depth-run-handle", &root, "l1", 1);
    depth_spec.initial_cash_raw = 1_000_000 * SCALE;
    let mut handle = depth_spec.start(RunContext::new(
        CallerCapability::Research,
        "depth-run-handle",
    ));
    assert_eq!(handle.run_id(), "depth-run-handle");
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    let outcome = handle
        .try_take_result()
        .expect("completed Tick run must have a result")
        .expect("shared Tick use case");
    assert_eq!(outcome.tier, "l1");
    assert!(outcome.fills > 0);
}

#[test]
fn depth_spec_refuses_tier_mismatch_and_research_run_requires_the_shared_use_case() {
    let root = super::fixtures::scratch("app-depth-refusals");
    let invalid_tier = spec("bad-tier", &root, "l0", 1);
    assert_eq!(
        invalid_tier
            .validate()
            .expect_err("unknown tier")
            .category(),
        AppErrorCategory::InvalidInput
    );
    let l1_with_depth = spec("bad-l1-depth", &root, "l1", 2);
    assert_eq!(
        run_depth_backtest(
            &l1_with_depth,
            &RunContext::new(CallerCapability::Research, "bad-l1-depth")
        )
        .expect_err("L1 must not silently discard depth")
        .category(),
        AppErrorCategory::FidelityInsufficient
    );
    let mut ineffective_queue = spec("ineffective-queue", &root, "l2", 2);
    ineffective_queue.queue_position_bps = 500;
    assert_eq!(
        ineffective_queue
            .validate()
            .expect_err("market strategy cannot claim limit queue position")
            .category(),
        AppErrorCategory::InvalidInput
    );
    let mut impossible_latency = spec("impossible-latency", &root, "l1", 1);
    impossible_latency.latency_snapshots = 47;
    assert_eq!(
        run_depth_backtest(
            &impossible_latency,
            &RunContext::new(CallerCapability::Research, "impossible-latency")
        )
        .expect_err("latency must mature within the data window")
        .category(),
        AppErrorCategory::FidelityInsufficient
    );
}

#[test]
fn depth_spec_keeps_legacy_json_shape_when_product_fields_are_absent() {
    let root = super::fixtures::scratch("app-depth-legacy-schema");
    let request = spec("depth-legacy-shape", &root, "l1", 1);
    let mut legacy = serde_json::to_value(request).expect("encode current spec");
    let fields = legacy.as_object_mut().expect("spec JSON object");
    fields.remove("market_spec");
    fields.remove("order_policy");

    let parsed = DepthBacktestSpec::from_json(&legacy.to_string())
        .expect("existing v1 payloads remain readable");
    let round_trip: serde_json::Value =
        serde_json::from_str(&parsed.to_json().expect("encode parsed spec"))
            .expect("round-trip JSON");
    assert!(!round_trip.as_object().unwrap().contains_key("market_spec"));
    assert!(!round_trip.as_object().unwrap().contains_key("order_policy"));
}
