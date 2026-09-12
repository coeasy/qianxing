use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CorporateActionType {
    Split,
    Dividend,
    RightsIssue,
    Merge,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CorporateAction {
    pub instrument: String,
    pub timestamp: u64,
    pub action_type: CorporateActionType,
    pub value_raw: i128,
}
