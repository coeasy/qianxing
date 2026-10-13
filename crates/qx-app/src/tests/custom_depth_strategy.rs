use super::depth_backtest::spec;
use crate::{
    BuiltinStrategySpec, CallerCapability, DepthBacktestSpec, RunContext, StrategyArtifactIdentity,
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
    let root = super::fixtures::scratch("app-injected-depth-strategy");
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
        let outcome = crate::run_depth_backtest_with_strategy(
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
            crate::verify_depth_run(
                &outcome,
                &RunContext::new(CallerCapability::Research, format!("verify-{tier}"))
            )
            .expect("replay verification")
            .verified
        );
    }
}
