mod common;

use common::{depth_spec as spec, perpetual_market_spec, scratch};
use qx_app::{
    run_depth_backtest, verify_depth_run, AppErrorCategory, BuiltinStrategySpec, CallerCapability,
    DepthBacktestSpec, PortfolioBacktestConfig, PortfolioStrategySpec, RunContext, RunStatus,
};
use qx_core::{MarginMode, OrderPolicy, PositionMode, PositionSide};
use qx_strategy::BuiltinStrategyKind;
use std::path::Path;

const SCALE: i128 = 1_000_000_000;

#[test]
fn l1_tick_use_case_runs_the_rust_engine_and_publishes_verifiable_artifacts() {
    let root = scratch("app-l1-tick");
    let mut spec = spec("l1-tick", &root, "l1", 1);
    spec.account_id = Some("acct.depth-test".into());
    let context = RunContext::new(CallerCapability::Research, "l1-tick");
    let first = run_depth_backtest(&spec, &context).expect("L1 Tick run");
    let second = run_depth_backtest(&spec, &context).expect("idempotent L1 Tick rerun");
    assert_eq!(first, second);
    assert_eq!(first.account_id.as_deref(), Some("acct.depth-test"));
    let summary: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&first.artifacts.summary).expect("summary"))
            .expect("summary JSON");
    assert!(summary.pointer("/metrics_exact/equity/final").is_some());
    let equity_csv = std::fs::read_to_string(&first.artifacts.equity).expect("equity CSV");
    assert!(equity_csv.starts_with("index,ts,equity_raw,equity_exact,position_raw\n"));
    let final_exact = equity_csv
        .lines()
        .last()
        .unwrap()
        .split(',')
        .nth(3)
        .unwrap();
    assert_eq!(
        final_exact,
        summary["metrics_exact"]["equity"]["final"]
            .as_str()
            .expect("exact final equity")
    );
    assert_eq!(first.display_summary, summary);
    let public_outcome = serde_json::to_value(&first).expect("stable depth outcome JSON");
    assert!(public_outcome.get("display_summary").is_none());
    let mut display_only_change = first.clone();
    display_only_change.display_summary["display_only_probe"] = serde_json::json!(true);
    assert_eq!(first, display_only_change);
    assert_eq!(summary["account_id"], "acct.depth-test");
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
fn depth_canonical_identity_binds_the_run_and_ignores_local_paths() {
    let first_root = scratch("depth-canonical-first");
    let second_root = scratch("depth-canonical-second");
    let first_spec = spec("depth-canonical-first", &first_root, "l1", 1);
    let second_spec = spec("depth-canonical-second", &second_root, "l1", 1);
    let first_context = RunContext::new(CallerCapability::Research, "depth-canonical-first");
    let second_context = RunContext::new(CallerCapability::Research, "depth-canonical-second");

    let first = run_depth_backtest(&first_spec, &first_context).expect("first depth run");
    let second = run_depth_backtest(&second_spec, &second_context).expect("second depth run");
    let first_manifest = qx_core::RunManifest::from_json(
        &std::fs::read_to_string(&first.artifacts.run_manifest).expect("first manifest"),
    )
    .expect("valid first manifest");
    let second_manifest = qx_core::RunManifest::from_json(
        &std::fs::read_to_string(&second.artifacts.run_manifest).expect("second manifest"),
    )
    .expect("valid second manifest");
    let first_spec = qx_spec::CanonicalRunSpec::from_json(
        first_manifest
            .canonical_run_spec_json
            .as_deref()
            .expect("canonical spec"),
    )
    .expect("valid canonical spec");
    let second_spec = qx_spec::CanonicalRunSpec::from_json(
        second_manifest
            .canonical_run_spec_json
            .as_deref()
            .expect("canonical spec"),
    )
    .expect("valid canonical spec");

    assert_eq!(
        first_spec.fingerprint().unwrap(),
        second_spec.fingerprint().unwrap()
    );
    assert_eq!(first_spec.datasets[0].role, "depth_snapshots");
    assert_eq!(first_spec.random_seed, "0");
    assert_eq!(
        first_spec.strategies[0].parameters["quantity_raw"],
        SCALE.to_string()
    );
    assert!(first_spec.datasets[0]
        .content_fingerprint
        .starts_with("depth-frame-fnv1a64:"));
    let canonical_json = first_spec.canonical_json().unwrap();
    assert!(!canonical_json.contains(&first_root.join("depth.json").to_string_lossy().to_string()));
    assert!(!canonical_json.contains(&first_root.join("runs").to_string_lossy().to_string()));
    assert!(first_spec.behavior.get("depth_path").is_none());
    assert!(first_spec.behavior.get("output_dir").is_none());
    assert!(first_spec.behavior.get("run_id").is_none());
    let first_verification = verify_depth_run(&first, &first_context).unwrap();
    let second_verification = verify_depth_run(&second, &second_context).unwrap();
    assert!(
        first_verification.verified,
        "{:?}",
        first_verification.mismatches
    );
    assert!(
        second_verification.verified,
        "{:?}",
        second_verification.mismatches
    );
}

#[test]
fn l2_order_book_use_case_uses_depth_matching_and_replay_verification() {
    let root = scratch("app-l2-book");
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
fn l2_market_making_replaces_quotes_through_shared_matching_risk_oms_and_replay() {
    let root = scratch("app-l2-market-making");
    let mut request = spec("l2-market-making", &root, "l2", 2);
    request.fee_bps = Some(10);
    // WP-2 之后 fee_bps 是 Option：`Some(10)` = 显式点名 10bp 旗标，`None` 才走成本规则 / 内核默认。原 `= 10` 语义不变。
    request.market_making = Some(qx_xingban::MarketMakingConfig {
        spread_bps: 100,
        quote_qty_raw: SCALE,
        max_inventory_raw: 5 * SCALE,
        tick_size_raw: SCALE / 100,
        inventory_skew_bps: 50,
    });
    let outcome = run_depth_backtest(
        &request,
        &RunContext::new(CallerCapability::Research, "l2-market-making"),
    )
    .expect("market making uses the shared Rust L2 use case");
    assert!(outcome.fills > 0, "quotes must reach the existing matcher");
    let summary: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&outcome.artifacts.summary).expect("summary"),
    )
    .expect("summary JSON");
    assert_eq!(summary["market_making"]["spread_bps"], 100);
    assert!(summary["fees_raw"].as_i64().unwrap_or_default() > 0);
    let verification = verify_depth_run(
        &outcome,
        &RunContext::new(CallerCapability::Research, "l2-market-making-verify"),
    )
    .expect("market-making artifacts replay");
    assert!(verification.verified, "{:?}", verification.mismatches);

    // 在同一份 L2 数据与策略上打开 FIFO 估计：产物须披露估计参数，结果指纹须体现模型变化。
    let mut fifo_request = request.clone();
    fifo_request.run_id = "l2-market-making-fifo".into();
    fifo_request.output_dir = root.join("fifo-runs").to_string_lossy().into_owned();
    fifo_request.queue_position_bps = 5_000;
    fifo_request.queue_position_model = qx_xingban::QueuePositionModel::FifoVolumeEstimate {
        volume_advance_bps: 10_000,
    };
    let fifo_outcome = run_depth_backtest(
        &fifo_request,
        &RunContext::new(CallerCapability::Research, "l2-market-making-fifo"),
    )
    .expect("FIFO queue estimate runs on the same L2 market-making workload");
    let fifo_summary: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&fifo_outcome.artifacts.summary).expect("FIFO summary"),
    )
    .expect("FIFO summary JSON");
    assert!(fifo_summary["model_descriptors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| {
            item.as_str()
                .unwrap_or_default()
                .contains("queue_position=fifo_volume_estimate@v2")
                && item
                    .as_str()
                    .unwrap_or_default()
                    .contains("volume_advance_bps=10000")
                && item
                    .as_str()
                    .unwrap_or_default()
                    .contains("hidden_liquidity_and_cancellations_are_unobserved")
        }));
    assert!(fifo_summary["model_descriptors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item
            .as_str()
            .unwrap_or_default()
            .starts_with("limit_order_fill_rate_bps=")));
}

#[test]
fn depth_use_case_uses_market_product_and_explicit_derivative_order_policy() {
    let root = scratch("app-depth-product-spec");
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
fn depth_run_handle_uses_the_same_tick_application_workflow() {
    let root = scratch("app-depth-run-handle");
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
fn depth_portfolio_runs_multiple_strategies_through_the_shared_tick_and_book_engines() {
    let root = scratch("app-depth-portfolio");
    for tier in ["l1", "l2"] {
        let run_id = format!("depth-portfolio-{tier}");
        let mut request = spec(&run_id, &root, tier, if tier == "l1" { 1 } else { 2 });
        request.portfolio = Some(PortfolioBacktestConfig {
            primary_priority: 0,
            primary_reserved_capital_raw: 500_000 * SCALE,
            primary_reserved_capital_exact: None,
            additional_strategies: vec![PortfolioStrategySpec {
                priority: 1,
                reserved_capital_raw: 500_000 * SCALE,
                reserved_capital_exact: None,
                strategy: BuiltinStrategySpec {
                    kind: BuiltinStrategyKind::Momentum,
                    strategy_id: "depth-momentum".into(),
                    quantity_raw: SCALE,
                    fast_window: 2,
                    slow_window: 3,
                    ..request.strategy.clone()
                },
            }],
        });
        request.initial_cash_raw = 1_000_000 * SCALE;
        let outcome = run_depth_backtest(
            &request,
            &RunContext::new(CallerCapability::Research, &run_id),
        )
        .expect("same-account depth portfolio");
        assert!(outcome.fills > 0, "{tier} portfolio should execute orders");
        assert_eq!(outcome.equity_points, 48);
        let fills = std::fs::read_to_string(&outcome.artifacts.fills).expect("fills artifact");
        assert_eq!(
            fills.lines().next(),
            Some(
                "order_id,ts,qty_raw,price_raw,fee_raw,account_id,strategy_id,signal_id,intent_id,venue_id,venue_order_id,qty_exact,price_exact,exact_fees_json"
            )
        );
        let owners = fills
            .lines()
            .skip(1)
            .filter_map(|line| line.split(',').nth(6))
            .collect::<std::collections::BTreeSet<_>>();
        assert!(owners.contains("depth-sma"), "{tier} primary attribution");
        assert!(
            owners.contains("depth-momentum"),
            "{tier} child attribution"
        );
        let summary = std::fs::read_to_string(&outcome.artifacts.summary)
            .map(|text| serde_json::from_str::<serde_json::Value>(&text).expect("summary JSON"))
            .expect("summary artifact");
        assert_eq!(
            summary["strategy_ids"],
            serde_json::json!(["depth-sma", "depth-momentum"])
        );
        assert_eq!(
            summary["strategy_fills"]
                .as_object()
                .unwrap()
                .values()
                .map(|value| value.as_u64().unwrap())
                .sum::<u64>(),
            outcome.fills
        );
        verify_depth_run(
            &outcome,
            &RunContext::new(CallerCapability::Research, &run_id),
        )
        .expect("portfolio report remains verifiable");
    }
}

#[test]
fn depth_spec_refuses_tier_mismatch_and_research_run_requires_the_shared_use_case() {
    let root = scratch("app-depth-refusals");
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
fn market_making_contract_requires_l2_and_market_tick_alignment() {
    let root = scratch("app-market-making-contract");
    let maker = qx_xingban::MarketMakingConfig {
        spread_bps: 50,
        quote_qty_raw: SCALE,
        max_inventory_raw: 5 * SCALE,
        tick_size_raw: 10,
        inventory_skew_bps: 25,
    };
    let mut l1 = spec("market-making-l1", &root, "l1", 1);
    l1.market_making = Some(maker);
    assert_eq!(
        l1.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );

    let mut mismatched = spec("market-making-tick", &root, "l2", 2);
    mismatched.market_making = Some(maker);
    mismatched.market_spec = Some(perpetual_market_spec());
    mismatched.order_policy = Some(OrderPolicy {
        reduce_only: false,
        position_side: PositionSide::Net,
        margin_mode: MarginMode::Cross,
        position_mode: PositionMode::OneWay,
        leverage: 2,
        post_only: false,
    });
    assert_eq!(
        mismatched.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}

#[test]
fn depth_spec_keeps_legacy_json_shape_when_product_fields_are_absent() {
    let root = scratch("app-depth-legacy-schema");
    let request = spec("depth-legacy-shape", &root, "l1", 1);
    let mut legacy = serde_json::to_value(request).expect("encode current spec");
    let fields = legacy.as_object_mut().expect("spec JSON object");
    fields.remove("market_spec");
    fields.remove("order_policy");
    fields.remove("market_making");

    let parsed = DepthBacktestSpec::from_json(&legacy.to_string())
        .expect("existing v1 payloads remain readable");
    let round_trip: serde_json::Value =
        serde_json::from_str(&parsed.to_json().expect("encode parsed spec"))
            .expect("round-trip JSON");
    assert!(!round_trip.as_object().unwrap().contains_key("market_spec"));
    assert!(!round_trip.as_object().unwrap().contains_key("order_policy"));
    assert!(!round_trip
        .as_object()
        .unwrap()
        .contains_key("market_making"));
}
