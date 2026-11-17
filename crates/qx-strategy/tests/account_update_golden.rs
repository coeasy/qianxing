use qx_strategy::contract::StrategyAccountUpdateV1;
use qx_strategy::{MarketEvent, Strategy, StrategyContext, StrategyDecision};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct AccountSequenceFixture {
    strategy_id: String,
    account_id: String,
    venue_id: String,
    expected_callback_order: Vec<String>,
    events: Vec<StrategyAccountUpdateV1>,
}

struct AccountRecorder {
    callbacks: Vec<String>,
}

impl Strategy for AccountRecorder {
    fn on_event(
        &mut self,
        _context: &StrategyContext,
        _event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        Err("account-update golden does not dispatch market events".into())
    }

    fn on_strategy_account_update(
        &mut self,
        _context: &StrategyContext,
        update: &StrategyAccountUpdateV1,
    ) -> Result<Option<StrategyDecision>, String> {
        self.callbacks.push(format!(
            "{}:{}:{}:{}",
            update.account_id, update.venue_id, update.event_seq, update.event
        ));
        Ok(None)
    }
}

#[test]
fn empty_account_and_position_snapshots_share_the_normalized_callback_matrix() {
    let fixture: AccountSequenceFixture = serde_json::from_str(include_str!(
        "fixtures/strategy-account-update-sequence-v1.json"
    ))
    .expect("shared account snapshot sequence parses");
    let matrix: Value =
        serde_json::from_str(include_str!("fixtures/strategy-lifecycle-matrix-v1.json"))
            .expect("shared normalized strategy lifecycle matrix parses");
    assert_eq!(
        fixture.expected_callback_order,
        matrix["expected_account_update_callbacks"]
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
        venue_id: fixture.venue_id,
        data_fingerprint: "strategy-account-update-matrix-v1".into(),
        as_of: 10,
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: None,
        risk_state: "clear".into(),
    };
    let mut strategy = AccountRecorder {
        callbacks: Vec::new(),
    };
    for update in &fixture.events {
        update.validate_for(&context).unwrap();
        assert!(strategy
            .on_strategy_account_update(&context, update)
            .unwrap()
            .is_none());
    }
    assert_eq!(
        strategy.callbacks,
        matrix["expected_account_update_callbacks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    );
}
