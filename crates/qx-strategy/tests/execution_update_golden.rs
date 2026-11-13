use qx_strategy::{
    MarketEvent, Strategy, StrategyContext, StrategyDecision, StrategyFillUpdateV2,
    StrategyOrderStatusUpdate,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
struct ExecutionSequenceFixture {
    strategy_id: String,
    account_id: String,
    instrument: String,
    expected_callback_order: Vec<String>,
    events: Vec<ExecutionSequenceEvent>,
}

#[derive(Deserialize)]
struct ExecutionSequenceEvent {
    kind: String,
    update: Value,
}

struct UpdateRecorder {
    callbacks: Vec<String>,
}

impl Strategy for UpdateRecorder {
    fn on_event(
        &mut self,
        _context: &StrategyContext,
        _event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        Err("execution-update golden does not dispatch market events".into())
    }

    fn on_strategy_order_status(
        &mut self,
        _context: &StrategyContext,
        update: &StrategyOrderStatusUpdate,
    ) -> Result<Option<StrategyDecision>, String> {
        self.callbacks.push(format!(
            "{}:{}:{}:order_status",
            update.order.account_id, update.order.instrument, update.event_seq
        ));
        Ok(None)
    }

    fn on_strategy_fill_update_v2(
        &mut self,
        _context: &StrategyContext,
        update: &StrategyFillUpdateV2,
    ) -> Result<Option<StrategyDecision>, String> {
        self.callbacks.push(format!(
            "{}:{}:{}:fill",
            update.order.account_id, update.order.instrument, update.event_seq
        ));
        Ok(None)
    }
}

#[test]
fn shared_partial_fill_sequence_is_canonical_for_rust_and_all_wire_consumers() {
    let fixture: ExecutionSequenceFixture =
        serde_json::from_str(include_str!("fixtures/strategy-execution-sequence-v1.json"))
            .expect("shared strategy execution update fixture parses");
    let matrix: Value =
        serde_json::from_str(include_str!("fixtures/strategy-lifecycle-matrix-v1.json"))
            .expect("shared normalized strategy lifecycle matrix parses");
    assert_eq!(fixture.events.len(), 6);
    assert_eq!(
        fixture.expected_callback_order,
        matrix["expected_execution_update_callbacks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    );

    let context = StrategyContext {
        strategy_id: fixture.strategy_id,
        strategy_version: "1".into(),
        account_id: fixture.account_id,
        venue_id: "BINANCE".into(),
        data_fingerprint: "execution-update-golden-v1".into(),
        as_of: 12,
        positions_exact: std::collections::BTreeMap::new(),
        cash_exact: std::collections::BTreeMap::new(),
        available_margin_exact: None,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: None,
        risk_state: "clear".into(),
    };

    let mut statuses = BTreeMap::new();
    let mut fills = BTreeMap::new();
    let mut callback_order = Vec::new();
    let mut strategy = UpdateRecorder {
        callbacks: Vec::new(),
    };
    for event in &fixture.events {
        match event.kind.as_str() {
            "order_status" => {
                let update: StrategyOrderStatusUpdate =
                    serde_json::from_value(event.update.clone()).unwrap();
                update.validate_for(&context).unwrap();
                callback_order.push(format!(
                    "{}:{}:{}:order_status",
                    update.order.account_id, update.order.instrument, update.event_seq
                ));
                assert!(strategy
                    .on_strategy_order_status(&context, &update)
                    .unwrap()
                    .is_none());
                let key = (
                    update.order.account_id.clone(),
                    update.order.instrument.clone(),
                    update.event_seq,
                    update.order.order_id,
                );
                assert_eq!(serde_json::to_value(&update).unwrap(), event.update);
                assert!(statuses.insert(key, update).is_none());
            }
            "fill" => {
                let update: StrategyFillUpdateV2 =
                    serde_json::from_value(event.update.clone()).unwrap();
                update.validate_for(&context).unwrap();
                callback_order.push(format!(
                    "{}:{}:{}:fill",
                    update.order.account_id, update.order.instrument, update.event_seq
                ));
                assert!(strategy
                    .on_strategy_fill_update_v2(&context, &update)
                    .unwrap()
                    .is_none());
                let key = (
                    update.order.account_id.clone(),
                    update.order.instrument.clone(),
                    update.event_seq,
                    update.order.order_id,
                );
                assert_eq!(serde_json::to_value(&update).unwrap(), event.update);
                assert!(fills.insert(key, update).is_none());
            }
            other => panic!("unknown shared execution event kind: {other}"),
        }
    }

    assert_eq!(callback_order, fixture.expected_callback_order);
    assert_eq!(
        strategy.callbacks,
        matrix["expected_execution_update_callbacks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(statuses.len(), 3);
    assert_eq!(fills.len(), 3);
    let accounts = statuses
        .keys()
        .map(|(account, _, _, _)| account.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(accounts, BTreeSet::from(["paper", "paper-b"]));

    let mut partial_fill_counts = BTreeMap::new();
    for (key, status) in &statuses {
        let fill = fills
            .get(key)
            .expect("every fill has its committed order status");
        assert_eq!(status.event_seq, fill.event_seq);
        assert_eq!(status.ts, fill.ts);
        assert_eq!(
            status.order.filled_raw,
            fill.order.filled_raw + fill.fill.qty.raw()
        );
        assert_eq!(fill.order.order_id, status.order.order_id);
        assert_eq!(fill.order.account_id, status.order.account_id);
        assert_eq!(fill.order.instrument, status.order.instrument);
        *partial_fill_counts
            .entry((key.0.clone(), key.1.clone(), key.3))
            .or_insert(0usize) += 1;
    }
    assert_eq!(
        partial_fill_counts[&("paper".into(), fixture.instrument, 7)],
        2
    );
    assert_eq!(
        partial_fill_counts[&("paper-b".into(), "ETHUSDT.OKX".into(), 7)],
        1
    );
}
