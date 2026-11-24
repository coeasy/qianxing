//! Per-order EventLog positions used by fee recovery and immutable decision lookups.
//!
//! These indexes are projections of the append-only EventLog. They are updated on local append,
//! shared-log tail refresh, and full replay, so recovery queries stay scoped to one order instead
//! of rescanning unrelated market history on every reconciliation cycle.

use qx_core::{Event, EventKind};
use std::collections::BTreeMap;

#[derive(Clone, Default)]
pub(super) struct RuntimeOrderFactIndex {
    fee_events_by_order: BTreeMap<u64, Vec<usize>>,
    order_risk_decisions: BTreeMap<u64, usize>,
    execution_capability_decisions: BTreeMap<u64, usize>,
}

impl RuntimeOrderFactIndex {
    pub(super) fn remember(&mut self, event: &Event, event_index: usize) {
        match &event.kind {
            EventKind::Filled { fill } => {
                self.fee_events_by_order
                    .entry(fill.order_id)
                    .or_default()
                    .push(event_index);
            }
            EventKind::FeeAdjustment { order_id, .. } => {
                self.fee_events_by_order
                    .entry(*order_id)
                    .or_default()
                    .push(event_index);
            }
            EventKind::OrderRiskDecision { decision } => {
                self.order_risk_decisions
                    .entry(decision.client_order_id)
                    .or_insert(event_index);
            }
            EventKind::ExecutionCapabilityDecision { decision } => {
                self.execution_capability_decisions
                    .entry(decision.client_order_id)
                    .or_insert(event_index);
            }
            _ => {}
        }
    }

    pub(super) fn fee_event_indices(&self, order_id: u64) -> &[usize] {
        self.fee_events_by_order
            .get(&order_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(super) fn order_risk_decision_index(&self, client_order_id: u64) -> Option<usize> {
        self.order_risk_decisions.get(&client_order_id).copied()
    }

    pub(super) fn execution_capability_decision_index(
        &self,
        client_order_id: u64,
    ) -> Option<usize> {
        self.execution_capability_decisions
            .get(&client_order_id)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{
        Event, ExecutionCapabilityDecisionFact, ExecutionCapabilityDecisionKind, InstrumentId,
        OrderRiskDecisionFact, Priority, VenueId,
    };

    fn risk_decision(client_order_id: u64, rule_set_version: &str) -> OrderRiskDecisionFact {
        OrderRiskDecisionFact {
            client_order_id,
            instrument: instrument(),
            strategy_id: None,
            signal_id: None,
            intent_id: None,
            allowed: true,
            violations: Vec::new(),
            projected_position_raw: None,
            projected_margin_raw: None,
            reference_price_raw: None,
            rule_set_version: rule_set_version.into(),
        }
    }

    fn capability_decision(client_order_id: u64) -> ExecutionCapabilityDecisionFact {
        ExecutionCapabilityDecisionFact {
            schema_version: 1,
            kind: ExecutionCapabilityDecisionKind::Submit,
            decision_id: "a".repeat(64),
            command_id: None,
            order_group_id: None,
            client_order_id,
            account_id: "account-a".into(),
            run_id: None,
            strategy_id: None,
            signal_id: None,
            intent_id: None,
            venue_id: "BINANCE".into(),
            order_fingerprint: "b".repeat(64),
            catalog_fingerprint: "c".repeat(64),
            market_rule_fingerprint: "d".repeat(64),
            preflight_json: "{}".into(),
        }
    }

    fn instrument() -> InstrumentId {
        InstrumentId::new("BTC-USDT", VenueId::new("BINANCE")).expect("valid instrument")
    }

    fn event(seq: u64, kind: EventKind) -> Event {
        Event::new(seq, seq, Priority::COMMAND, kind)
    }

    #[test]
    fn order_decision_indexes_keep_the_first_durable_fact() {
        let mut index = RuntimeOrderFactIndex::default();
        index.remember(
            &event(
                0,
                EventKind::OrderRiskDecision {
                    decision: risk_decision(7, "risk-v1"),
                },
            ),
            0,
        );
        index.remember(
            &event(
                1,
                EventKind::OrderRiskDecision {
                    decision: risk_decision(7, "risk-v2"),
                },
            ),
            1,
        );

        assert_eq!(index.order_risk_decision_index(7), Some(0));
        assert_eq!(index.order_risk_decision_index(8), None);

        index.remember(
            &event(
                2,
                EventKind::ExecutionCapabilityDecision {
                    decision: capability_decision(7),
                },
            ),
            2,
        );
        index.remember(
            &event(
                3,
                EventKind::ExecutionCapabilityDecision {
                    decision: capability_decision(7),
                },
            ),
            3,
        );
        assert_eq!(index.execution_capability_decision_index(7), Some(2));
        assert_eq!(index.execution_capability_decision_index(8), None);
    }

    #[test]
    fn fee_event_index_is_scoped_by_order_and_preserves_log_order() {
        let mut index = RuntimeOrderFactIndex::default();
        for (seq, order_id) in [(0, 7), (1, 9), (2, 7)] {
            index.remember(
                &event(
                    seq,
                    EventKind::FeeAdjustment {
                        account_id: "account-a".into(),
                        venue_id: "BINANCE".into(),
                        order_id,
                        instrument: instrument(),
                        fees: Vec::new(),
                    },
                ),
                seq as usize,
            );
        }

        assert_eq!(index.fee_event_indices(7), &[0, 2]);
        assert_eq!(index.fee_event_indices(9), &[1]);
        assert!(index.fee_event_indices(10).is_empty());
    }
}
