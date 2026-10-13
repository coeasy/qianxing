use crate::{ExperimentParameterSpace, MultiMarketBacktestOutcome, MultiMarketBacktestSpec};
use qx_strategy::BuiltinStrategyKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunMultiMarketExperimentSpec {
    pub schema_version: u32,
    pub experiment_id: String,
    /// The one strategy slot varied by the grid; all other market/strategy slots stay fixed.
    pub target_strategy_id: String,
    pub base: MultiMarketBacktestSpec,
    pub parameter_space: Vec<ExperimentParameterSpace>,
    /// Optional strategy-selection dimension for the target slot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strategy_kinds: Vec<BuiltinStrategyKind>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiMarketExperimentCandidate {
    pub ordinal: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_kind: Option<String>,
    pub parameters: BTreeMap<String, i128>,
    pub outcome: Option<MultiMarketBacktestOutcome>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiMarketExperimentRankedCandidate {
    pub rank: usize,
    pub ordinal: usize,
    pub run_id: String,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub shared_result_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMultiMarketExperimentResult {
    pub schema_version: u32,
    pub experiment_id: String,
    pub target_strategy_id: String,
    pub spec_fingerprint: String,
    pub total_candidates: usize,
    pub completed_candidates: usize,
    pub succeeded_candidates: usize,
    pub failed_candidates: usize,
    pub candidates: Vec<MultiMarketExperimentCandidate>,
    pub ranking: Vec<MultiMarketExperimentRankedCandidate>,
    pub artifact_path: String,
}
