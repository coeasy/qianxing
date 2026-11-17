use crate::{
    ExperimentParameterSpace, MultiMarketDepthBacktestOutcome, MultiMarketDepthBacktestSpec,
};
use qx_strategy::BuiltinStrategyKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunMultiMarketDepthExperimentSpec {
    pub schema_version: u32,
    pub experiment_id: String,
    pub target_strategy_id: String,
    pub base: MultiMarketDepthBacktestSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameter_space: Vec<ExperimentParameterSpace>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strategy_kinds: Vec<BuiltinStrategyKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callback_parameter_space: Vec<ExperimentParameterSpace>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiMarketDepthExperimentCandidate {
    pub ordinal: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_kind: Option<String>,
    pub parameters: BTreeMap<String, i128>,
    pub outcome: Option<MultiMarketDepthBacktestOutcome>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiMarketDepthExperimentRankedCandidate {
    pub rank: usize,
    pub ordinal: usize,
    pub run_id: String,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub shared_result_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMultiMarketDepthExperimentResult {
    pub schema_version: u32,
    pub experiment_id: String,
    pub target_strategy_id: String,
    pub spec_fingerprint: String,
    pub total_candidates: usize,
    pub completed_candidates: usize,
    pub succeeded_candidates: usize,
    pub failed_candidates: usize,
    pub candidates: Vec<MultiMarketDepthExperimentCandidate>,
    pub ranking: Vec<MultiMarketDepthExperimentRankedCandidate>,
    pub artifact_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_run_spec_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_run_spec_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_data_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_manifest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_record: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_locations: Option<BTreeMap<String, String>>,
}
