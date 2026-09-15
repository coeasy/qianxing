use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorporateActionType {
    Split,
    Dividend,
    BonusShare,
    CapitalTransfer,
    RightsIssue,
    NewShareIssue,
    Repurchase,
    ConvertibleBondIssue,
    ConvertibleBondInterest,
    ConvertibleBondRedemption,
    ConvertibleBondCall,
    ConvertibleBondPut,
    ConvertibleBondConversion,
    Suspension,
    CapitalChange,
    Merge,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CorporateAction {
    pub instrument: String,
    pub timestamp: u64,
    pub action_type: CorporateActionType,
    pub value_raw: i128,
    #[serde(default)]
    pub secondary_value_raw: i128,
    #[serde(default = "default_ratio")]
    pub ratio_num: i128,
    #[serde(default = "default_ratio")]
    pub ratio_den: i128,
    #[serde(default)]
    pub price_raw: i128,
    #[serde(default)]
    pub published_at: Option<u64>,
    #[serde(default)]
    pub effective_at: Option<u64>,
    #[serde(default)]
    pub source: String,
}

fn default_ratio() -> i128 {
    1
}
