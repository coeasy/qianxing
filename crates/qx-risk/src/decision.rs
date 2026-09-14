use serde::{Deserialize, Serialize};

use crate::RiskSnapshot;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RiskAction {
    Allow,
    Reject,
    Reduce,
    Rebalance,
}

pub fn evaluate_exposure(snapshot: &RiskSnapshot, max_gross_exposure: i128) -> RiskAction {
    if snapshot.gross_exposure > max_gross_exposure {
        RiskAction::Reduce
    } else {
        RiskAction::Allow
    }
}
