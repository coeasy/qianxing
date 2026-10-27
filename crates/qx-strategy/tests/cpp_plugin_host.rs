use qx_core::{ExactDecimal, InstrumentId, MarginMode, PositionMode, PositionSide, Side};
use qx_strategy::contract::StrategyAccountUpdateV1;
use qx_strategy::{
    sha256_hex, DynamicCAbiLoadPolicy, DynamicCAbiStrategy, MarketEvent, Strategy, StrategyContext,
    StrategyErrorStage, StrategyErrorV1, StrategyFillUpdateV2, StrategyOrderStatusUpdate,
    StrategyStopContextV1,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[test]
#[ignore = "requires a C++ shared-library fixture; exercised by the C++ SDK CI matrix"]
fn loads_and_runs_cpp_strategy_through_the_rust_c_abi_host() {
    let library = PathBuf::from(
        std::env::var_os("QX_CPP_STRATEGY_LIBRARY")
            .expect("CI must provide the C++ strategy shared-library path"),
    );
    let bytes = std::fs::read(&library).expect("read the C++ strategy shared library");
    let policy = DynamicCAbiLoadPolicy::new(sha256_hex(&bytes))
        .with_max_library_bytes(u64::try_from(bytes.len()).expect("library length fits u64"));
    let mut strategy = unsafe {
        DynamicCAbiStrategy::load_verified(&library, "{}", &policy)
            .expect("load the C++ strategy through the verified C ABI host")
    };
    assert!(strategy.supports_order_policy_overrides());
    assert!(strategy.supports_order_status_updates());
    assert!(strategy.supports_account_updates());
    assert!(strategy.supports_fill_updates());
    assert!(strategy.supports_exact_market_data());
    assert!(strategy.supports_exact_orderbook());
    assert!(strategy.supports_exact_context());
    assert!(strategy.supports_state_checkpoint());
    strategy
        .restore_strategy_checkpoint(&serde_json::json!({ "next_intent_id": 17 }))
        .expect("restore C++ strategy-owned state");
    assert_eq!(
        strategy
            .snapshot_strategy_checkpoint()
            .expect("snapshot C++ strategy-owned state"),
        serde_json::json!({ "next_intent_id": 17 })
    );
    strategy
        .restore_strategy_checkpoint(&serde_json::json!({ "next_intent_id": 1 }))
        .expect("restore initial intent sequence for the callback fixture");
    let warmup_fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/strategy-warmup-complete-v1.json"))
            .expect("shared warmup lifecycle fixture is valid JSON");
    let lifecycle_matrix: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/strategy-lifecycle-matrix-v1.json"))
            .expect("shared normalized strategy lifecycle matrix is valid JSON");
    let declared_warmup_bars = warmup_fixture["warmup_bars"].as_u64().unwrap();
    assert_eq!(
        strategy.history_requirements().unwrap().warmup_bars,
        declared_warmup_bars
    );
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/strategy-execution-sequence-v1.json"))
            .expect("shared cross-host execution update fixture is valid JSON");
    let exact_context_fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/strategy-context-v2.json"))
            .expect("shared exact StrategyContext fixture is valid JSON");
    let exact_context_v1 = &exact_context_fixture["context_v1"];
    let exact_map = |field: &str| {
        exact_context_fixture[field]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                (
                    item["key"].as_str().unwrap().to_string(),
                    ExactDecimal::from_parts(
                        item["coefficient"]
                            .as_str()
                            .unwrap()
                            .parse::<i128>()
                            .unwrap(),
                        u8::try_from(item["scale"].as_u64().unwrap()).unwrap(),
                    )
                    .unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    let exact_margin = &exact_context_fixture["available_margin_exact"];
    let context = StrategyContext {
        strategy_id: fixture["strategy_id"].as_str().unwrap().into(),
        strategy_version: exact_context_v1["strategy_version"]
            .as_str()
            .unwrap()
            .into(),
        account_id: fixture["account_id"].as_str().unwrap().into(),
        venue_id: exact_context_v1["venue_id"].as_str().unwrap().into(),
        data_fingerprint: "cpp-plugin-ci".into(),
        as_of: exact_context_v1["as_of"].as_u64().unwrap(),
        positions_exact: exact_map("positions_exact"),
        cash_exact: exact_map("cash_exact"),
        available_margin_exact: Some(
            ExactDecimal::from_parts(
                exact_margin["coefficient"]
                    .as_str()
                    .unwrap()
                    .parse::<i128>()
                    .unwrap(),
                u8::try_from(exact_margin["scale"].as_u64().unwrap()).unwrap(),
            )
            .unwrap(),
        ),
        positions: serde_json::from_value(exact_context_v1["positions"].clone()).unwrap(),
        cash: serde_json::from_value(exact_context_v1["cash"].clone()).unwrap(),
        available_margin_raw: Some(i128::from(
            exact_context_v1["available_margin_raw"].as_i64().unwrap(),
        )),
        risk_state: exact_context_v1["risk_state"].as_str().unwrap().into(),
    };
    strategy.on_init(&context).expect("C++ on_init callback");
    strategy.on_start(&context).expect("C++ on_start callback");
    let warmup_event = &warmup_fixture["warmup_complete"];
    let warmup_instrument = InstrumentId::parse(warmup_event["instrument"].as_str().unwrap())
        .expect("warmup fixture instrument parses");
    let first_warmup_bar = MarketEvent::Bar {
        instrument: warmup_instrument.clone(),
        ts: 9,
        open_raw: 100,
        high_raw: 101,
        low_raw: 99,
        close_raw: 100,
        volume_raw: 1,
        exact_ohlcv: None,
    };
    strategy
        .on_event(&context, &first_warmup_bar)
        .expect("C++ first warmup Bar callback");
    strategy
        .on_warmup_complete(
            &context,
            Some(&warmup_instrument),
            warmup_event["observed_bars"].as_u64().unwrap(),
        )
        .expect("C++ warmup-complete callback");
    let event = MarketEvent::Bar {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").expect("valid test instrument"),
        ts: 10,
        open_raw: 100,
        high_raw: 101,
        low_raw: 99,
        close_raw: 100,
        volume_raw: 1,
        exact_ohlcv: None,
    };
    let decision = strategy
        .on_event(&context, &event)
        .expect("C++ on_event callback");
    assert_eq!(decision.schema_version, 1);
    assert_eq!(decision.request_id, "update-test:10");
    assert_eq!(decision.strategy_id, context.strategy_id);
    assert_eq!(decision.signal_id, 10);
    assert_eq!(decision.confidence, 500);
    assert_eq!(decision.intents.len(), 1);
    let intent = &decision.intents[0];
    assert_eq!(intent.instrument.symbol, "BTCUSDT");
    assert_eq!(intent.instrument.venue.as_str(), "BINANCE");
    assert_eq!(intent.side, Side::Buy);
    assert_eq!(intent.qty.raw(), 1_000_000_000);
    let order_policy = intent.policy.expect("C++ policy extension is loaded");
    assert_eq!(order_policy.position_side, PositionSide::Long);
    assert_eq!(order_policy.margin_mode, MarginMode::Cross);
    assert_eq!(order_policy.position_mode, PositionMode::Hedge);
    assert_eq!(order_policy.leverage, 3);

    let exact_event = MarketEvent::Bar {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").expect("valid test instrument"),
        ts: 10,
        open_raw: 100,
        high_raw: 101,
        low_raw: 99,
        close_raw: 100,
        volume_raw: 1,
        exact_ohlcv: Some(qx_core::ExactOhlcv {
            open: qx_core::ExactDecimal::parse("0.0000001005").unwrap(),
            high: qx_core::ExactDecimal::parse("0.0000001015").unwrap(),
            low: qx_core::ExactDecimal::parse("0.0000000995").unwrap(),
            close: qx_core::ExactDecimal::parse("0.0000001005").unwrap(),
            volume: qx_core::ExactDecimal::parse("0.0000000015").unwrap(),
        }),
    };
    let exact_decision = strategy
        .on_event(&context, &exact_event)
        .expect("C++ v2 market-event callback receives source precision");
    assert_eq!(exact_decision.intents.len(), 1);

    let exact_book_fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/strategy-market-orderbook-v1.json"))
            .expect("shared exact OrderBook fixture is valid JSON");
    let exact_book_event: MarketEvent = serde_json::from_value(exact_book_fixture)
        .expect("Rust MarketEvent accepts exact OrderBook fixture");
    let exact_book_decision = strategy
        .on_event(&context, &exact_book_event)
        .expect("C++ v3 callback receives exact source book values");
    assert!(exact_book_decision.intents.is_empty());

    let timer_fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/strategy-market-timer-v1.json"))
            .expect("shared Timer event fixture is valid JSON");
    let timer_event: MarketEvent =
        serde_json::from_value(timer_fixture).expect("Rust MarketEvent accepts Timer fixture");
    let timer_decision = strategy
        .on_event(&context, &timer_event)
        .expect("C++ dynamic plugin receives the shared Timer event");
    assert_eq!(timer_decision.request_id, "update-test:1000:scheduler");
    assert_eq!(timer_decision.signal_id, 1_000);
    assert!(timer_decision.intents.is_empty());

    let callback_error = StrategyErrorV1::new(
        StrategyErrorStage::MarketEvent,
        &context,
        Some(10),
        Some(InstrumentId::parse("BTCUSDT.BINANCE").unwrap()),
        "test callback failed",
    );
    strategy
        .on_error(&callback_error)
        .expect("C++ optional StrategyErrorV1 callback");

    let fixture_events = fixture["events"].as_array().unwrap();
    for event in fixture_events {
        match event["kind"].as_str().unwrap() {
            "order_status" => {
                let update: StrategyOrderStatusUpdate =
                    serde_json::from_value(event["update"].clone())
                        .expect("decode shared order status golden fact");
                update
                    .validate_for(&context)
                    .expect("golden order status binds to strategy context");
                assert!(strategy
                    .on_strategy_order_status(&context, &update)
                    .expect("C++ typed order-status callback")
                    .is_none());
            }
            "fill" => {
                let update: StrategyFillUpdateV2 = serde_json::from_value(event["update"].clone())
                    .expect("decode shared fill golden fact");
                update
                    .validate_for(&context)
                    .expect("golden fill binds to strategy context");
                assert!(strategy
                    .on_strategy_fill_update_v2(&context, &update)
                    .expect("C++ sequence-bound fill callback")
                    .is_none());
            }
            kind => panic!("unknown shared execution event kind: {kind}"),
        }
    }
    let account_fixture: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/strategy-account-update-sequence-v1.json"
    ))
    .expect("shared account/position update fixture is valid JSON");
    for value in account_fixture["events"].as_array().unwrap() {
        let update: StrategyAccountUpdateV1 = serde_json::from_value(value.clone())
            .expect("Rust decodes shared account/position update fixture");
        update
            .validate_for(&context)
            .expect("account/position fact binds to strategy context");
        assert!(strategy
            .on_strategy_account_update(&context, &update)
            .expect("C++ account/position snapshot callback")
            .is_none());
    }
    let expected_cpp_callbacks = lifecycle_matrix["expected_cpp_plugin_host_callback_order"]
        .as_array()
        .unwrap();
    let expected_cpp_callbacks_before_stop =
        expected_cpp_callbacks[..expected_cpp_callbacks.len() - 1].to_vec();
    assert_eq!(
        strategy.diagnostic_state(),
        Some(serde_json::json!({
            "next_intent_id": 4,
            "exact_context_updates": 16,
            "exact_position_coefficient": 1,
            "exact_position_scale": 18,
            "exact_cash_coefficient": 7,
            "exact_cash_scale": 18,
            "exact_margin_coefficient": 9,
            "exact_margin_scale": 18,
            "order_status_events": 3,
            "order_status_v1_events": 0,
            "order_status_v2_events": 3,
            "account_update_events": 2,
            "last_account_update_event_seq": 23,
            "fill_update_v2_events": 3,
            "last_fill_event_seq": 24,
            "exact_market_events": 1,
            "last_exact_close_coefficient": 1005,
            "last_exact_close_scale": 10,
            "exact_orderbook_events": 1,
            "last_exact_book_price_coefficient": 12345,
            "last_exact_book_price_scale": 12,
            "last_exact_book_qty_coefficient": 5,
            "last_exact_book_qty_scale": 18,
            "warmup_complete_events": 1,
            "last_warmup_instrument": "BTCUSDT.BINANCE",
            "last_warmup_observed_bars": 2,
            "last_order_status_event_seq": 24,
            "has_request": true,
            "started": true,
            "stopped": false,
            "stop_context_seen": false,
            "strategy_error_events": 1,
            "callback_order": expected_cpp_callbacks_before_stop
        })),
        "the optional C++ diagnostic export exposes a bounded read-only strategy snapshot"
    );
    decision
        .validate_for(&context, event.ts())
        .expect("C++ decision passes the shared Rust strategy contract");
    exact_decision
        .validate_for(&context, exact_event.ts())
        .expect("C++ precise-market decision passes the shared Rust strategy contract");
    exact_book_decision
        .validate_for(&context, exact_book_event.ts())
        .expect("C++ exact-book decision passes the shared Rust strategy contract");
    let stop = StrategyStopContextV1::host_stop(Some(context.clone()));
    strategy
        .on_stop_with_context(&stop)
        .expect("C ABI versioned stop callback");
    let stop_diagnostic = strategy.diagnostic_state().unwrap();
    assert_eq!(
        stop_diagnostic["stopped"], true,
        "the C++ stop callback must run after the full event sequence"
    );
    assert_eq!(
        stop_diagnostic["stop_context_seen"], true,
        "the C++ plugin must receive the versioned reason and final context"
    );
    assert_eq!(
        stop_diagnostic["callback_order"],
        lifecycle_matrix["expected_cpp_plugin_host_callback_order"],
        "the C++ callback trace must follow the shared normalized lifecycle matrix"
    );
    let normalized_warmup = lifecycle_matrix["expected_normalized_callback_order"]
        .as_array()
        .unwrap();
    let cpp_trace = stop_diagnostic["callback_order"].as_array().unwrap();
    assert_eq!(
        &cpp_trace[..normalized_warmup.len() - 1],
        &normalized_warmup[..normalized_warmup.len() - 1],
        "the C++ Bar lifecycle prefix must match the cross-host warmup matrix"
    );
}
