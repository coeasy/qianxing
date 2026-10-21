mod common;

use common::{depth_spec as spec, scratch};
use qx_app::{
    run_depth_backtest_with_strategy, run_depth_backtest_with_strategy_slots, verify_depth_run,
    BuiltinStrategySpec, CallerCapability, DepthBacktestSpec, PortfolioBacktestConfig,
    PortfolioStrategySpec, RunContext, StrategyArtifactIdentity,
};
use qx_core::{Quantity, Side};
use qx_strategy::{
    BuiltinStrategyKind, MarketEvent, Strategy, StrategyContext, StrategyDecision,
    StrategyOrderIntent, STRATEGY_API_VERSION,
};

struct BuyOnFirstEvent(bool, std::sync::Arc<std::sync::Mutex<Option<&'static str>>>);

impl Strategy for BuyOnFirstEvent {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        *self.1.lock().expect("event lock") = Some(match event {
            MarketEvent::Tick { .. } => "tick",
            MarketEvent::OrderBook { .. } => "order_book",
            other => return Err(format!("unexpected depth event: {other:?}")),
        });
        let intents = if self.0 {
            Vec::new()
        } else {
            self.0 = true;
            vec![StrategyOrderIntent {
                intent_id: 1,
                instrument: event.instrument().cloned().expect("instrument"),
                side: Side::Buy,
                qty: Quantity::from_raw(1_000_000_000),
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

#[test]
fn injected_strategy_receives_tick_and_order_book_events_through_shared_depth_engines() {
    let root = scratch("app-injected-depth-strategy");
    let identity = StrategyArtifactIdentity {
        strategy_id: "depth-callback".into(),
        strategy_version: "test-v1".into(),
        source_sha256: "c".repeat(64),
    };
    for (tier, levels, expected_event) in [("l1", 1, "tick"), ("l2", 2, "order_book")] {
        let run_id = format!("custom-{tier}");
        let run_root = root.join(tier);
        std::fs::create_dir_all(&run_root).expect("create run directory");
        let mut request: DepthBacktestSpec = spec(&run_id, &run_root, tier, levels);
        request.strategy =
            BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "depth-callback");
        let seen_event = std::sync::Arc::new(std::sync::Mutex::new(None));
        let outcome = run_depth_backtest_with_strategy(
            &request,
            &identity,
            Box::new(BuyOnFirstEvent(false, seen_event.clone())),
            &RunContext::new(CallerCapability::Research, &run_id),
        )
        .expect("custom strategy runs on shared depth engine");
        assert_eq!(
            *seen_event.lock().expect("event lock"),
            Some(expected_event)
        );
        assert!(
            outcome.fills > 0,
            "custom order must pass shared Risk/OMS/matching"
        );
        assert!(
            verify_depth_run(
                &outcome,
                &RunContext::new(CallerCapability::Research, format!("verify-{tier}"))
            )
            .expect("replay verification")
            .verified
        );
    }
}

#[test]
fn multiple_custom_strategies_share_tick_and_order_book_portfolio_slots() {
    let root = scratch("app-custom-depth-portfolio");
    for (tier, levels, event_name) in [("l1", 1, "tick"), ("l2", 2, "order_book")] {
        let run_id = format!("custom-depth-portfolio-{tier}");
        let run_root = root.join(tier);
        std::fs::create_dir_all(&run_root).expect("create output");
        let mut request = spec(&run_id, &run_root, tier, levels);
        request.strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "depth-sma");
        request.initial_cash_raw = 1_000_000 * 1_000_000_000;
        request.portfolio = Some(PortfolioBacktestConfig {
            primary_priority: 0,
            primary_reserved_capital_raw: 500_000 * 1_000_000_000,
            additional_strategies: vec![PortfolioStrategySpec {
                priority: 1,
                reserved_capital_raw: 500_000 * 1_000_000_000,
                strategy: BuiltinStrategySpec::new(BuiltinStrategyKind::Momentum, "depth-momentum"),
            }],
        });
        let primary_event = std::sync::Arc::new(std::sync::Mutex::new(None));
        let child_event = std::sync::Arc::new(std::sync::Mutex::new(None));
        let identity = |strategy_id: &str, source: char| StrategyArtifactIdentity {
            strategy_id: strategy_id.into(),
            strategy_version: format!("{strategy_id}-v1"),
            source_sha256: source.to_string().repeat(64),
        };
        let outcome = run_depth_backtest_with_strategy_slots(
            &request,
            vec![
                (
                    identity("depth-sma", 'd'),
                    Box::new(BuyOnFirstEvent(false, primary_event.clone())) as Box<dyn Strategy>,
                ),
                (
                    identity("depth-momentum", 'e'),
                    Box::new(BuyOnFirstEvent(false, child_event.clone())) as Box<dyn Strategy>,
                ),
            ],
            &RunContext::new(CallerCapability::Research, &run_id),
        )
        .expect("multiple caller callbacks share the Rust portfolio");
        assert_eq!(
            *primary_event.lock().expect("primary event"),
            Some(event_name)
        );
        assert_eq!(*child_event.lock().expect("child event"), Some(event_name));
        assert!(outcome.fills >= 2, "both callback strategies must fill");
        let fills = std::fs::read_to_string(&outcome.artifacts.fills).expect("fills");
        let owners = fills
            .lines()
            .skip(1)
            .filter_map(|line| line.split(',').nth(6))
            .collect::<std::collections::BTreeSet<_>>();
        assert!(owners.contains("depth-sma"));
        assert!(owners.contains("depth-momentum"));
        verify_depth_run(
            &outcome,
            &RunContext::new(CallerCapability::Research, format!("verify-{run_id}")),
        )
        .expect("multi-callback Tick/OrderBook run replays");
        let rerun = run_depth_backtest_with_strategy_slots(
            &request,
            vec![
                (
                    identity("depth-momentum", 'e'),
                    Box::new(BuyOnFirstEvent(false, child_event.clone())) as Box<dyn Strategy>,
                ),
                (
                    identity("depth-sma", 'd'),
                    Box::new(BuyOnFirstEvent(false, primary_event.clone())) as Box<dyn Strategy>,
                ),
            ],
            &RunContext::new(CallerCapability::Research, format!("rerun-{run_id}")),
        )
        .expect("binding order does not affect deterministic portfolio identity");
        assert_eq!(outcome.result_hash, rerun.result_hash);
        let changed_identity = run_depth_backtest_with_strategy_slots(
            &request,
            vec![
                (
                    identity("depth-sma", '3'),
                    Box::new(BuyOnFirstEvent(false, primary_event.clone())) as Box<dyn Strategy>,
                ),
                (
                    identity("depth-momentum", 'e'),
                    Box::new(BuyOnFirstEvent(false, child_event.clone())) as Box<dyn Strategy>,
                ),
            ],
            &RunContext::new(CallerCapability::Research, format!("changed-{run_id}")),
        )
        .expect("changed strategy source is a distinct content-addressed run");
        assert_ne!(
            changed_identity.artifacts.run_manifest,
            outcome.artifacts.run_manifest
        );
    }
}
