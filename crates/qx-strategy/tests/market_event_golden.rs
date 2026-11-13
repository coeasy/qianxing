use qx_strategy::{MarketEvent, Strategy, StrategyContext, StrategyDecision};
use serde_json::Value;
use std::collections::BTreeMap;

struct MarketEventRecorder {
    callbacks: Vec<String>,
}

impl Strategy for MarketEventRecorder {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let kind = match event {
            MarketEvent::OrderBook { .. } => "OrderBook",
            MarketEvent::Timer { .. } => "Timer",
            other => return Err(format!("unexpected market event in golden: {other:?}")),
        };
        self.callbacks.push(format!("market:{kind}:{}", event.ts()));
        Ok(StrategyDecision {
            schema_version: 1,
            request_id: format!("market-event-golden:{}", event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 0,
            priority: 0,
            expires_at: event.ts(),
            intents: Vec::new(),
        })
    }
}

fn strategy_context() -> StrategyContext {
    StrategyContext {
        strategy_id: "market-event-golden".into(),
        strategy_version: "1".into(),
        account_id: "paper".into(),
        venue_id: "BINANCE".into(),
        data_fingerprint: "strategy-market-event-golden-v1".into(),
        as_of: 1_000,
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: None,
        risk_state: "clear".into(),
    }
}

#[test]
fn timer_market_event_matches_the_shared_wire_fixture() {
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/strategy-market-timer-v1.json"))
            .expect("shared Timer event fixture is valid JSON");
    let event: MarketEvent =
        serde_json::from_value(expected.clone()).expect("Rust MarketEvent accepts Timer fixture");

    event
        .validate()
        .expect("Timer event meets the common contract");
    assert_eq!(event.ts(), 1_000);
    assert_eq!(event.instrument(), None);
    let matrix: Value =
        serde_json::from_str(include_str!("fixtures/strategy-lifecycle-matrix-v1.json")).unwrap();
    assert_eq!(
        vec![format!("market:Timer:{}", event.ts())],
        matrix["expected_timer_callbacks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    );
    let mut strategy = MarketEventRecorder {
        callbacks: Vec::new(),
    };
    strategy
        .on_event(&strategy_context(), &event)
        .expect("Rust strategy dispatches Timer through on_event");
    assert_eq!(
        strategy.callbacks,
        matrix["expected_timer_callbacks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(serde_json::to_value(event).unwrap(), expected);
}

#[test]
fn exact_orderbook_market_event_matches_the_shared_wire_fixture() {
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/strategy-market-orderbook-v1.json"))
            .expect("shared precise OrderBook fixture is valid JSON");
    let event: MarketEvent = serde_json::from_value(expected.clone())
        .expect("Rust MarketEvent accepts precise OrderBook fixture");

    event
        .validate()
        .expect("precise OrderBook event meets the common contract");
    assert_eq!(event.ts(), 10);
    let matrix: Value =
        serde_json::from_str(include_str!("fixtures/strategy-lifecycle-matrix-v1.json")).unwrap();
    assert_eq!(
        vec![format!("market:OrderBook:{}", event.ts())],
        matrix["expected_market_order_book_callbacks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    );
    let mut strategy = MarketEventRecorder {
        callbacks: Vec::new(),
    };
    strategy
        .on_event(&strategy_context(), &event)
        .expect("Rust strategy dispatches exact OrderBook through on_event");
    assert_eq!(
        strategy.callbacks,
        matrix["expected_market_order_book_callbacks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(serde_json::to_value(event).unwrap(), expected);
}

#[test]
fn unsupported_market_event_kind_is_rejected_by_the_shared_contract() {
    let matrix: Value =
        serde_json::from_str(include_str!("fixtures/strategy-lifecycle-matrix-v1.json")).unwrap();
    let kind = matrix["unsupported_event_kind"].as_str().unwrap();
    let mut event = serde_json::Map::new();
    event.insert(kind.to_string(), serde_json::json!({"ts": 100}));
    let error = serde_json::from_value::<MarketEvent>(Value::Object(event))
        .expect_err("Rust MarketEvent must reject an event kind outside the contract");
    assert!(error.to_string().contains(kind));
}
