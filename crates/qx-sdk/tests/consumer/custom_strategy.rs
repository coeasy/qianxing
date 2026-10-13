//! The published Rust facade accepts caller-authored strategy code without an internal-crate import.

use super::{scratch, spec};
use qx_sdk::{
    CallerCapability, Client, MarketEvent, Quantity, Side, Strategy, StrategyArtifactIdentity,
    StrategyContext, StrategyDecision, StrategyOrderIntent, STRATEGY_API_VERSION,
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
