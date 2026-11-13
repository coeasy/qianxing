use crate::{
    ExperimentOutOfSampleReport, ExperimentSearchAlgorithm, ExperimentSearchObjective,
    ExperimentSearchPlan, ExperimentSearchTrial, ExperimentSplitPlan,
};
use crate::{ExperimentParameterSpace, MultiMarketBacktestOutcome, MultiMarketBacktestSpec};
use qx_strategy::BuiltinStrategyKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunMultiMarketExperimentSpec {
    pub schema_version: u32,
    pub experiment_id: String,
    /// The one strategy slot varied by the grid or adaptive search; all others stay fixed.
    pub target_strategy_id: String,
    pub base: MultiMarketBacktestSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameter_space: Vec<ExperimentParameterSpace>,
    /// Optional strategy-selection dimension for the target slot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strategy_kinds: Vec<BuiltinStrategyKind>,
    /// Arbitrary numeric parameters passed to a custom strategy factory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callback_parameter_space: Vec<ExperimentParameterSpace>,
    /// Optional time-ordered train/test or walk-forward evaluation across the union timeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_plan: Option<ExperimentSplitPlan>,
    /// Deterministic training-only adaptive search over the declared bounded parameter space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<ExperimentSearchPlan>,
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
pub struct MultiMarketExperimentFoldCandidateFailure {
    pub fold: usize,
    pub ordinal: usize,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiMarketExperimentFoldSearchReport {
    pub fold: usize,
    pub seed: u64,
    pub train_range: [u64; 2],
    pub test_range: [u64; 2],
    pub trials: Vec<ExperimentSearchTrial>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiMarketExperimentSearchReport {
    pub algorithm: ExperimentSearchAlgorithm,
    pub objective: ExperimentSearchObjective,
    pub seed: u64,
    pub budget: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_training_drawdown_bps: Option<u32>,
    pub fold_local_search: bool,
    pub fold_searches: Vec<MultiMarketExperimentFoldSearchReport>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_of_sample: Option<ExperimentOutOfSampleReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fold_candidate_failures: Vec<MultiMarketExperimentFoldCandidateFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<MultiMarketExperimentSearchReport>,
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
