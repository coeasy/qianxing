//! The published Rust facade accepts caller-authored strategy code without an internal-crate import.

use super::{scratch, spec};
use qx_sdk::{
    BuiltinStrategyKind, BuiltinStrategySpec, CallerCapability, Client, DepthBacktestSpec,
    MarketEvent, PortfolioBacktestConfig, PortfolioStrategySpec, Quantity, RunStatus, Side,
    Strategy, StrategyArtifactIdentity, StrategyContext, StrategyDecision, StrategyOrderIntent,
    DEPTH_BACKTEST_SPEC_SCHEMA_VERSION, STRATEGY_API_VERSION,
};

struct BuyOnce(bool);

impl Strategy for BuyOnce {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let intents = if self.0 {
            Vec::new()
        } else {
            self.0 = true;
            vec![StrategyOrderIntent {
                intent_id: 1,
                instrument: event
                    .instrument()
                    .cloned()
                    .ok_or_else(|| "market event has no instrument".to_string())?,
                side: Side::Buy,
                qty: Quantity::from_raw(1_000_000),
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,
            }]
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

fn depth_spec(run_id: &str, output_dir: &std::path::Path, tier: &str) -> DepthBacktestSpec {
    std::fs::create_dir_all(output_dir).expect("create consumer depth fixture directory");
    let snapshots = (0..8)
        .map(|index| {
            let mid = 100_i128 + index as i128;
            let index = index as i128;
            let levels = if tier == "l1" { 1 } else { 2 };
            serde_json::json!({
                "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                "ts": 1_000 + index as u64 * 1_000,
                "sequence": index + 1,
                "bids": (0..levels).map(|level| serde_json::json!({
                    "price": (mid - 1 - level) * 1_000_000_000,
                    "qty": 100_000_000_000_i128,
                })).collect::<Vec<_>>(),
                "asks": (0..levels).map(|level| serde_json::json!({
                    "price": (mid + 1 + level) * 1_000_000_000,
                    "qty": 100_000_000_000_i128,
                })).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let depth_path = output_dir.join("depth.json");
    std::fs::write(
        &depth_path,
        serde_json::json!({
            "schema_version": 1,
            "source": "qx-sdk-custom-depth-test",
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "snapshots": snapshots,
        })
        .to_string(),
    )
    .expect("write depth fixture");
    DepthBacktestSpec {
        schema_version: DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
        run_id: run_id.into(),
        depth_path: depth_path.to_string_lossy().into_owned(),
        tier: tier.into(),
        settlement_currency: "USDT".into(),
        initial_cash_raw: 1_000_000_000_000_000,
        fee_bps: 0,
        latency_snapshots: 0,
        queue_position_bps: 0,
        market_impact_bps: 0,
        output_dir: output_dir.join("runs").to_string_lossy().into_owned(),
        strategy: BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "sdk-depth-custom"),
        market_spec: None,
        order_policy: None,
        market_making: None,
        account_id: None,
        portfolio: None,
    }
}

#[test]
fn sdk_client_runs_custom_strategy_through_the_shared_replay_kernel() {
    let output_dir = scratch("custom-strategy");
    let mut backtest = spec("sdk-custom-strategy", &output_dir);
    backtest.strategy.fast_window = 5;
    backtest.strategy.slow_window = 20;
    let client = Client::new(CallerCapability::Research, "sdk-custom-strategy");
    let identity = StrategyArtifactIdentity {
        strategy_id: "sdk-sma".into(),
        strategy_version: "consumer-test-v1".into(),
        source_sha256: "a".repeat(64),
    };
    let outcome = client
        .run_backtest_with_strategy(&backtest, &identity, BuyOnce(false))
        .expect("custom strategy runs via qx-sdk");
    assert!(outcome.fills > 0);
    assert!(client.verify_run(&outcome).expect("verify").verified);
}

#[test]
fn sdk_client_starts_and_verifies_custom_strategy_through_run_handle() {
    let output_dir = scratch("custom-strategy-run-handle");
    let mut backtest = spec("sdk-custom-strategy-handle", &output_dir);
    backtest.strategy.fast_window = 5;
    backtest.strategy.slow_window = 20;
    let client = Client::new(CallerCapability::Research, "sdk-custom-strategy-handle");
    let identity = StrategyArtifactIdentity {
        strategy_id: "sdk-sma".into(),
        strategy_version: "consumer-test-v1".into(),
        source_sha256: "b".repeat(64),
    };
    let mut handle = client
        .start_backtest_with_strategy(backtest, identity, BuyOnce(false))
        .expect("custom strategy RunHandle");
    assert_eq!(handle.run_id(), "sdk-custom-strategy-handle");
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    let outcome = handle
        .try_take_result()
        .expect("completed strategy run has a result")
        .expect("strategy run succeeds");
    assert!(outcome.fills > 0);
    assert!(client.verify_run(&outcome).expect("verify").verified);
}

#[test]
fn sdk_client_runs_custom_strategy_on_tick_and_order_book_shared_engines() {
    for tier in ["l1", "l2"] {
        let output_dir = scratch(&format!("custom-depth-{tier}"));
        let run_id = format!("sdk-custom-depth-{tier}");
        let client = Client::new(CallerCapability::Research, &run_id);
        let identity = StrategyArtifactIdentity {
            strategy_id: "sdk-depth-custom".into(),
            strategy_version: "depth-consumer-v1".into(),
            source_sha256: "f".repeat(64),
        };
        let spec = depth_spec(&run_id, &output_dir, tier);
        let outcome = client
            .run_depth_backtest_with_strategy(&spec, &identity, BuyOnce(false))
            .expect("custom strategy runs through Rust SDK depth entry");
        assert!(outcome.fills > 0);
        assert!(client.verify_depth_run(&outcome).expect("verify").verified);

        let handle_run_id = format!("sdk-custom-depth-handle-{tier}");
        let handle_client = Client::new(CallerCapability::Research, &handle_run_id);
        let handle_spec = depth_spec(&handle_run_id, &output_dir.join("handle"), tier);
        let mut handle_identity = identity;
        handle_identity.source_sha256 = "e".repeat(64);
        let mut handle = handle_client
            .start_depth_backtest_with_strategy(handle_spec, handle_identity, BuyOnce(false))
            .expect("custom strategy depth RunHandle");
        assert_eq!(handle.wait(), RunStatus::Succeeded);
        let result = handle
            .try_take_result()
            .expect("completed depth strategy has result")
            .expect("depth callback run succeeds");
        assert!(result.fills > 0);
        assert!(
            handle_client
                .verify_depth_run(&result)
                .expect("verify")
                .verified
        );
    }
}

#[test]
fn sdk_client_composes_multiple_custom_callbacks_in_tick_and_order_book_portfolios() {
    for (tier, use_handle) in [("l1", false), ("l2", true)] {
        let output_dir = scratch(&format!("custom-depth-portfolio-{tier}"));
        let run_id = format!("sdk-custom-depth-portfolio-{tier}");
        let client = Client::new(CallerCapability::Research, &run_id);
        let mut spec = depth_spec(&run_id, &output_dir, tier);
        spec.strategy =
            BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "sdk-depth-primary");
        spec.portfolio = Some(PortfolioBacktestConfig {
            primary_priority: 0,
            primary_reserved_capital_raw: 500_000_000_000_000,
            additional_strategies: vec![PortfolioStrategySpec {
                priority: 1,
                reserved_capital_raw: 500_000_000_000_000,
                strategy: BuiltinStrategySpec::new(
                    BuiltinStrategyKind::Momentum,
                    "sdk-depth-child",
                ),
            }],
        });
        let callbacks = || {
            vec![
                (
                    StrategyArtifactIdentity {
                        strategy_id: "sdk-depth-primary".into(),
                        strategy_version: "consumer-primary-v1".into(),
                        source_sha256: "1".repeat(64),
                    },
                    Box::new(BuyOnce(false)) as Box<dyn Strategy>,
                ),
                (
                    StrategyArtifactIdentity {
                        strategy_id: "sdk-depth-child".into(),
                        strategy_version: "consumer-child-v1".into(),
                        source_sha256: "2".repeat(64),
                    },
                    Box::new(BuyOnce(false)) as Box<dyn Strategy>,
                ),
            ]
        };
        let outcome = if use_handle {
            let handle_spec = DepthBacktestSpec {
                run_id: format!("{run_id}-handle"),
                output_dir: output_dir.join("handle").to_string_lossy().into_owned(),
                ..spec
            };
            let mut handle = client
                .start_depth_backtest_with_strategy_slots(handle_spec, callbacks())
                .expect("multi-callback depth RunHandle");
            assert_eq!(handle.wait(), RunStatus::Succeeded);
            handle
                .try_take_result()
                .expect("depth callback result is ready")
                .expect("depth callback run succeeds")
        } else {
            client
                .run_depth_backtest_with_strategy_slots(&spec, callbacks())
                .expect("multi-callback depth sync run")
        };
        assert!(outcome.fills >= 2);
        assert!(client.verify_depth_run(&outcome).expect("replay").verified);
        let fills = std::fs::read_to_string(&outcome.artifacts.fills).expect("fills artifact");
        assert!(fills.contains("sdk-depth-primary"));
        assert!(fills.contains("sdk-depth-child"));
    }
}
