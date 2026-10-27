//! Bounded parameter experiments over the shared Rust two-leg backtest use case.

use super::artifacts::write_text;
use super::experiment_grid::{
    apply_parameters, expand_grid, normalized_strategy, validate_experiment_id, validate_grid,
};
use super::experiment_spec_artifact::{
    serialize_experiment_input, verify_spec_artifact, write_spec_artifact,
};
use super::guard::guard_panics;
use super::run_multi_leg_backtest::custom_strategy::{self, InjectedMultiLegStrategy};
use super::run_multi_leg_backtest::run_inner;
mod canonical_spec;
use super::{attach, ExperimentParameterSpace};
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiLegBacktestOutcome, MultiLegBacktestSpec,
    RunContext, RunHandle, StrategyArtifactIdentity,
};
pub(crate) use canonical_spec::verify_candidate_run_manifests_with_reader;
use canonical_spec::{build_multi_leg_experiment_identity, verify_candidate_run_manifests};
use qx_core::Fnv1a;
use qx_strategy::{BuiltinStrategyKind, Strategy};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const RUN_MULTI_LEG_EXPERIMENT_SCHEMA_VERSION: u32 = 1;

type StrategyFactory<'a> =
    dyn FnMut(&MultiLegBacktestSpec) -> Result<Box<dyn Strategy>, String> + 'a;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunMultiLegExperimentSpec {
    pub schema_version: u32,
    pub experiment_id: String,
    pub base: MultiLegBacktestSpec,
    pub parameter_space: Vec<ExperimentParameterSpace>,
    /// Optional multi-leg built-in strategy dimension; omitted to preserve old fingerprints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strategy_kinds: Vec<BuiltinStrategyKind>,
}

impl RunMultiLegExperimentSpec {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload)
            .map_err(|error| invalid(format!("RunMultiLegExperimentSpec JSON 无效: {error}")))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        let mut canonical = self.clone();
        canonical.strategy_kinds.sort_by_key(|kind| kind.name());
        serde_json::to_string(&canonical)
            .map_err(|error| invariant(format!("RunMultiLegExperimentSpec 序列化失败: {error}")))
    }

    pub fn validate(&self) -> Result<usize, AppError> {
        if self.schema_version != RUN_MULTI_LEG_EXPERIMENT_SCHEMA_VERSION {
            return Err(invalid(format!(
                "RunMultiLegExperimentSpec schema_version={} 本版本只认 {}",
                self.schema_version, RUN_MULTI_LEG_EXPERIMENT_SCHEMA_VERSION
            )));
        }
        validate_experiment_id(&self.experiment_id)?;
        let mut kinds = if self.strategy_kinds.is_empty() {
            vec![self.base.strategy.kind]
        } else {
            self.strategy_kinds.clone()
        };
        kinds.sort_by_key(|kind| kind.name());
        let mut seen = BTreeSet::new();
        if kinds.iter().any(|kind| !seen.insert(kind.name())) {
            return Err(invalid("strategy_kinds 不得重复"));
        }
        if kinds.iter().any(|kind| !kind.needs_reference_leg()) {
            return Err(invalid(
                "多腿实验的 strategy_kinds 只能包含需要 reference leg 的策略",
            ));
        }
        let strategy_selection = !self.strategy_kinds.is_empty();
        let mut per_kind = None;
        for kind in &kinds {
            let mut candidate = self.base.clone();
            candidate.strategy.kind = *kind;
            candidate.strategy = normalized_strategy(&candidate.strategy, &self.parameter_space);
            candidate.validate()?;
            let count = if strategy_selection && self.parameter_space.is_empty() {
                1
            } else {
                validate_grid(&candidate.strategy, &self.parameter_space)?
            };
            if per_kind
                .replace(count)
                .is_some_and(|previous| previous != count)
            {
                return Err(invalid("不同策略 kind 的参数网格候选数必须一致"));
            }
        }
        per_kind
            .unwrap_or_default()
            .checked_mul(kinds.len())
            .filter(|total| *total <= super::run_experiment::MAX_EXPERIMENT_CANDIDATES)
            .ok_or_else(|| invalid("实验最多接受 256 个候选组合"))
    }

    /// Run the bounded grid on cancellable Rust multi-leg backtests.
    pub fn start(self, context: RunContext) -> RunHandle<RunMultiLegExperimentResult> {
        let run_id = context.correlation_id().to_string();
        RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "多腿参数回测实验")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner_experiment(&self, &context, Some(&cancellation), None, None)
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }

    /// Start a parameter experiment whose each candidate receives a fresh caller-authored strategy.
    pub fn start_with_strategy_factory<F>(
        self,
        identity: StrategyArtifactIdentity,
        mut factory: F,
        context: RunContext,
    ) -> Result<RunHandle<RunMultiLegExperimentResult>, AppError>
    where
        F: FnMut(&MultiLegBacktestSpec) -> Result<Box<dyn Strategy>, String> + Send + 'static,
    {
        context.require(CallerCapability::Research, "多语言策略多腿参数回测实验")?;
        self.validate()?;
        custom_strategy::validate_experiment_identity(&self, &identity)?;
        let run_id = context.correlation_id().to_string();
        Ok(RunHandle::spawn(run_id, move |cancellation| {
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner_experiment(
                    &self,
                    &context,
                    Some(&cancellation),
                    Some(&identity),
                    Some(&mut factory),
                )
            })
            .map_err(|error| attach(error, &correlation_id))
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiLegExperimentCandidate {
    pub ordinal: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_kind: Option<String>,
    pub parameters: BTreeMap<String, i128>,
    pub outcome: Option<MultiLegBacktestOutcome>,
    /// Stable AppError JSON; a failed candidate does not stop the other runs.
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiLegExperimentRankedCandidate {
    pub rank: usize,
    pub ordinal: usize,
    pub run_id: String,
    pub combined_return_bps: i32,
    pub primary_result_hash: String,
    pub reference_result_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMultiLegExperimentResult {
    pub schema_version: u32,
    pub experiment_id: String,
    pub spec_fingerprint: String,
    pub total_candidates: usize,
    pub completed_candidates: usize,
    pub succeeded_candidates: usize,
    pub failed_candidates: usize,
    pub candidates: Vec<MultiLegExperimentCandidate>,
    /// Stable return ranking; ties retain candidate ordinal. Risk metrics remain in each
    /// candidate's attribution artifact and are not recomputed by the experiment layer.
    pub ranking: Vec<MultiLegExperimentRankedCandidate>,
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

/// A report, manifest and record whose storage locations have been rebound for a local run or a
/// portable evidence bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedMultiLegExperimentArtifacts {
    pub result: RunMultiLegExperimentResult,
    pub report_json: String,
    pub run_manifest_json: String,
    pub run_record_json: String,
}

impl RunMultiLegExperimentResult {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload)
            .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error.to_string()))
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|error| invariant(format!("多腿实验结果序列化失败: {error}")))
    }
}

pub fn run_multi_leg_experiment(
    spec: &RunMultiLegExperimentSpec,
    context: &RunContext,
) -> Result<RunMultiLegExperimentResult, AppError> {
    let correlation_id = context.correlation_id().to_string();
    context.require(CallerCapability::Research, "多腿参数回测实验")?;
    guard_panics(&correlation_id, || {
        run_inner_experiment(spec, context, None, None, None)
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| attach(invariant("同步多腿参数实验未产生报告"), &correlation_id))
}

impl RunMultiLegExperimentSpec {
    /// Execute a parameter grid with a fresh caller-authored strategy per candidate.
    pub fn run_with_strategy_factory<F>(
        &self,
        identity: &StrategyArtifactIdentity,
        factory: &mut F,
        context: &RunContext,
    ) -> Result<RunMultiLegExperimentResult, AppError>
    where
        F: FnMut(&MultiLegBacktestSpec) -> Result<Box<dyn Strategy>, String>,
    {
        let correlation_id = context.correlation_id().to_string();
        context.require(CallerCapability::Research, "多语言策略多腿参数回测实验")?;
        self.validate()?;
        custom_strategy::validate_experiment_identity(self, identity)?;
        guard_panics(&correlation_id, || {
            run_inner_experiment(self, context, None, Some(identity), Some(factory))
        })
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| {
            attach(
                invariant("同步多语言策略多腿实验未产生报告"),
                &correlation_id,
            )
        })
    }
}

fn run_inner_experiment(
    spec: &RunMultiLegExperimentSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    identity: Option<&StrategyArtifactIdentity>,
    mut factory: Option<&mut StrategyFactory<'_>>,
) -> Result<Option<RunMultiLegExperimentResult>, AppError> {
    let total = spec.validate()?;
    let spec_json = spec.to_json()?;
    let mut fingerprint = Fnv1a::new();
    fingerprint.write_text(&spec_json);
    if let Some(identity) = identity {
        fingerprint.write_text(&identity.strategy_id);
        fingerprint.write_text(&identity.strategy_version);
        fingerprint.write_text(&identity.source_sha256);
    }
    let spec_fingerprint = format!("{:016x}", fingerprint.finish());
    let persisted_input = serialize_experiment_input(&spec_json, context.code_commit(), identity)?;
    let (canonical_run_spec, input_data_fingerprint) =
        build_multi_leg_experiment_identity(spec, identity, context)?;
    let canonical_run_spec_json = canonical_run_spec
        .canonical_json()
        .map_err(|error| invariant(format!("多腿实验 CanonicalRunSpec 无效: {error}")))?;
    let canonical_run_spec_fingerprint = canonical_run_spec
        .fingerprint()
        .map_err(|error| invariant(format!("多腿实验 CanonicalRunSpec 指纹失败: {error}")))?;
    let root = PathBuf::from(&spec.base.output_dir).join(&spec.experiment_id);
    let artifact_path = root
        .join(format!("{}.multi-leg-experiment.json", spec.experiment_id))
        .to_string_lossy()
        .into_owned();
    let spec_path = artifact_path.replace(
        ".multi-leg-experiment.json",
        ".multi-leg-experiment.spec.json",
    );
    match std::fs::read_to_string(&artifact_path) {
        Ok(existing) => {
            let mut result = existing_result(
                &existing,
                &spec_fingerprint,
                &spec.experiment_id,
                &canonical_run_spec_json,
                &canonical_run_spec_fingerprint,
                &input_data_fingerprint,
            )?;
            verify_candidate_run_manifests(
                &result.candidates,
                &input_data_fingerprint,
                &canonical_run_spec,
            )?;
            verify_spec_artifact(&spec_path, &persisted_input, "多腿实验")?;
            if has_complete_persistence_envelope(&result) {
                verify_persisted_multi_leg_experiment_result(&result)?;
                if result
                    .artifact_locations
                    .as_ref()
                    .is_some_and(|locations| locations.contains_key("experiment_input"))
                {
                    return Ok(Some(result));
                }
                result = persist_multi_leg_experiment_result(
                    result,
                    &canonical_run_spec,
                    &root,
                    &artifact_path,
                    &spec_path,
                )?;
            } else if has_any_persistence_envelope(&result) {
                return Err(AppError::new(
                    AppErrorCategory::StorageFailure,
                    "多腿实验报告持久化信封不完整，拒绝复用",
                ));
            } else {
                result = persist_multi_leg_experiment_result(
                    result,
                    &canonical_run_spec,
                    &root,
                    &artifact_path,
                    &spec_path,
                )?;
            }
            return Ok(Some(result));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取既有多腿实验报告", &error)),
    }

    let mut kinds = if spec.strategy_kinds.is_empty() {
        vec![spec.base.strategy.kind]
    } else {
        spec.strategy_kinds.clone()
    };
    kinds.sort_by_key(|kind| kind.name());
    let combinations = if spec.parameter_space.is_empty() {
        vec![BTreeMap::new()]
    } else {
        expand_grid(&spec.parameter_space)
    };
    debug_assert_eq!(combinations.len() * kinds.len(), total);
    let mut candidates = Vec::with_capacity(total);
    for (index, (kind, parameters)) in kinds
        .into_iter()
        .flat_map(|kind| {
            combinations
                .iter()
                .cloned()
                .map(move |parameters| (kind, parameters))
        })
        .enumerate()
    {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            return Ok(None);
        }
        let ordinal = index + 1;
        let mut candidate = spec.base.clone();
        candidate.strategy.kind = kind;
        candidate.run_id = format!("{}-{ordinal:04}", spec.experiment_id);
        candidate.output_dir = root
            .join(format!("run-{ordinal:04}"))
            .to_string_lossy()
            .into_owned();
        let outcome = match apply_parameters(&mut candidate.strategy, &parameters) {
            Ok(()) => {
                let injected = match (identity, factory.as_deref_mut()) {
                    (Some(identity), Some(factory)) => match factory(&candidate) {
                        Ok(strategy) => Some(InjectedMultiLegStrategy {
                            identity: identity.clone(),
                            strategy,
                        }),
                        Err(error) => {
                            candidates.push(MultiLegExperimentCandidate {
                                ordinal,
                                strategy_kind: None,
                                parameters,
                                outcome: None,
                                error: Some(
                                    AppError::new(AppErrorCategory::InvalidInput, error).to_json(),
                                ),
                            });
                            continue;
                        }
                    },
                    (None, None) => None,
                    _ => return Err(invariant("多腿实验策略工厂和身份必须同时提供")),
                };
                match run_inner(&candidate, context, cancellation, injected, None) {
                    Ok(Some(outcome)) => Ok(outcome),
                    Ok(None) => return Ok(None),
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        };
        candidates.push(match outcome {
            Ok(outcome) => MultiLegExperimentCandidate {
                ordinal,
                strategy_kind: (!spec.strategy_kinds.is_empty()).then(|| kind.name().to_string()),
                parameters,
                outcome: Some(outcome),
                error: None,
            },
            Err(error) => MultiLegExperimentCandidate {
                ordinal,
                strategy_kind: (!spec.strategy_kinds.is_empty()).then(|| kind.name().to_string()),
                parameters,
                outcome: None,
                error: Some(error.to_json()),
            },
        });
    }

    verify_candidate_run_manifests(&candidates, &input_data_fingerprint, &canonical_run_spec)?;

    let mut ranking = candidates
        .iter()
        .filter_map(|candidate| {
            candidate.outcome.as_ref().map(|outcome| {
                (
                    candidate.ordinal,
                    outcome.run_id.clone(),
                    outcome.combined_return_bps,
                    outcome.primary_result_hash.clone(),
                    outcome.reference_result_hash.clone(),
                )
            })
        })
        .collect::<Vec<_>>();
    ranking.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(&right.0)));
    let ranking = ranking
        .into_iter()
        .enumerate()
        .map(
            |(
                index,
                (ordinal, run_id, combined_return_bps, primary_result_hash, reference_result_hash),
            )| {
                MultiLegExperimentRankedCandidate {
                    rank: index + 1,
                    ordinal,
                    run_id,
                    combined_return_bps,
                    primary_result_hash,
                    reference_result_hash,
                }
            },
        )
        .collect::<Vec<_>>();
    let succeeded_candidates = ranking.len();
    let result = RunMultiLegExperimentResult {
        schema_version: RUN_MULTI_LEG_EXPERIMENT_SCHEMA_VERSION,
        experiment_id: spec.experiment_id.clone(),
        spec_fingerprint,
        total_candidates: total,
        completed_candidates: candidates.len(),
        succeeded_candidates,
        failed_candidates: total - succeeded_candidates,
        candidates,
        ranking,
        artifact_path: artifact_path.clone(),
        canonical_run_spec_json: Some(canonical_run_spec_json),
        canonical_run_spec_fingerprint: Some(canonical_run_spec_fingerprint),
        input_data_fingerprint: Some(input_data_fingerprint),
        result_hash: None,
        run_manifest: None,
        run_record: None,
        artifact_locations: None,
    };
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        return Ok(None);
    }
    std::fs::create_dir_all(&root)
        .map_err(|error| AppError::from_io("创建多腿实验产物目录", &error))?;
    write_spec_artifact(&spec_path, &persisted_input, "多腿实验")?;
    let persisted = persist_multi_leg_experiment_result(
        result,
        &canonical_run_spec,
        &root,
        &artifact_path,
        &spec_path,
    )?;
    Ok(Some(persisted))
}

fn has_any_persistence_envelope(result: &RunMultiLegExperimentResult) -> bool {
    result.result_hash.is_some()
        || result.run_manifest.is_some()
        || result.run_record.is_some()
        || result.artifact_locations.is_some()
}

fn has_complete_persistence_envelope(result: &RunMultiLegExperimentResult) -> bool {
    result.result_hash.is_some()
        && result.run_manifest.is_some()
        && result.run_record.is_some()
        && result.artifact_locations.is_some()
}

fn persist_multi_leg_experiment_result(
    result: RunMultiLegExperimentResult,
    canonical: &qx_spec::CanonicalRunSpec,
    root: &Path,
    report_index_path: &str,
    spec_artifact_path: &str,
) -> Result<RunMultiLegExperimentResult, AppError> {
    let facts_payload = result.to_json()?;
    let result_hash = super::experiment_result_hash_from_json(&facts_payload)
        .map_err(|error| invariant(format!("多腿实验结果指纹失败: {error}")))?;
    let manifest = build_multi_leg_experiment_manifest(&result, canonical, &result_hash)?;
    let manifest_path = crate::artifact_store::persist_run_manifest(root, &manifest)?;
    let report_path = root.join(format!(
        "{}-{result_hash}-input-v1.multi-leg-experiment.json",
        result.experiment_id
    ));
    let record_path = root.join(format!(
        "{}-{result_hash}-input-v1.record.json",
        result.experiment_id
    ));
    let experiment_input_bytes = std::fs::read(spec_artifact_path)
        .map_err(|error| AppError::from_io("读取多腿实验输入规格 sidecar", &error))?;
    let prepared = prepare_multi_leg_experiment_artifacts(
        result,
        canonical,
        report_index_path,
        &report_path.to_string_lossy(),
        &manifest_path.to_string_lossy(),
        &record_path.to_string_lossy(),
        spec_artifact_path,
        &experiment_input_bytes,
    )?;
    publish_multi_leg_experiment_artifact(&record_path, &prepared.run_record_json, "RunRecord V2")?;
    publish_multi_leg_experiment_artifact(&report_path, &prepared.report_json, "实验报告")?;
    write_text(report_index_path, &prepared.report_json)?;

    verify_persisted_multi_leg_experiment_result(&prepared.result)?;
    Ok(prepared.result)
}

/// Rebuild pathless run identity and content-addressed references when a report is copied into a
/// portable bundle or another storage root.
pub fn prepare_multi_leg_experiment_artifacts(
    mut result: RunMultiLegExperimentResult,
    canonical: &qx_spec::CanonicalRunSpec,
    report_index_path: &str,
    report_location: &str,
    manifest_location: &str,
    record_location: &str,
    experiment_input_location: &str,
    experiment_input_bytes: &[u8],
) -> Result<PreparedMultiLegExperimentArtifacts, AppError> {
    let facts_payload = result.to_json()?;
    let result_hash = super::experiment_result_hash_from_json(&facts_payload)
        .map_err(|error| invariant(format!("多腿实验结果指纹失败: {error}")))?;
    let manifest = build_multi_leg_experiment_manifest(&result, canonical, &result_hash)?;
    let manifest_json = manifest
        .to_json()
        .map_err(|error| invariant(format!("多腿实验 RunManifest 编码失败: {error}")))?;
    result.artifact_path = report_index_path.to_owned();
    result.result_hash = Some(result_hash);
    result.run_manifest = Some(manifest_location.to_owned());
    result.run_record = Some(record_location.to_owned());
    result.artifact_locations = Some(BTreeMap::from([
        ("experiment_report".into(), report_location.to_owned()),
        ("run_manifest".into(), manifest_location.to_owned()),
        (
            "experiment_input".into(),
            experiment_input_location.to_owned(),
        ),
    ]));
    let report_json = result.to_json()?;
    let report_bytes = report_json.as_bytes();
    let manifest_bytes = manifest_json.as_bytes();
    let record = qx_spec::RunRecordV2 {
        schema_version: qx_spec::RUN_RECORD_V2_SCHEMA_VERSION,
        run_id: result.experiment_id.clone(),
        status: qx_spec::RunStatus::Completed,
        input_digest: result
            .input_data_fingerprint
            .clone()
            .ok_or_else(|| invariant("多腿实验输入指纹缺失"))?,
        code_identity: format!("{}:{}", manifest.code_commit, manifest.runtime_version),
        config_fingerprint: result
            .canonical_run_spec_fingerprint
            .clone()
            .ok_or_else(|| invariant("多腿实验 CanonicalRunSpec 指纹缺失"))?,
        started_at_ms: manifest.clock_start,
        finished_at_ms: Some(manifest.clock_end),
        artifact_refs: vec![
            qx_spec::ArtifactRefV2 {
                name: "experiment_report".into(),
                artifact_id: qx_strategy::sha256_hex(report_bytes),
                kind: "run-multi-leg-experiment-result-json".into(),
                schema_version: RUN_MULTI_LEG_EXPERIMENT_SCHEMA_VERSION,
                byte_len: report_bytes.len() as u64,
            },
            qx_spec::ArtifactRefV2 {
                name: "run_manifest".into(),
                artifact_id: qx_strategy::sha256_hex(manifest_bytes),
                kind: "run-manifest-json".into(),
                schema_version: 1,
                byte_len: manifest_bytes.len() as u64,
            },
            qx_spec::ArtifactRefV2 {
                name: "experiment_input".into(),
                artifact_id: qx_strategy::sha256_hex(experiment_input_bytes),
                kind: "run-experiment-input-json".into(),
                schema_version: 1,
                byte_len: experiment_input_bytes.len() as u64,
            },
        ],
        replay_verdict: qx_spec::ReplayVerdict::Verified,
        capability_level: qx_spec::CapabilityLevel::L1,
    };
    let run_record_json = record
        .to_json()
        .map_err(|error| invariant(format!("多腿实验 RunRecord V2 编码失败: {error}")))?;
    Ok(PreparedMultiLegExperimentArtifacts {
        result,
        report_json,
        run_manifest_json: manifest_json,
        run_record_json,
    })
}

fn build_multi_leg_experiment_manifest(
    result: &RunMultiLegExperimentResult,
    canonical: &qx_spec::CanonicalRunSpec,
    result_hash: &str,
) -> Result<qx_core::RunManifest, AppError> {
    let runtime_version = canonical
        .execution_model
        .get("runtime_version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invariant("多腿实验执行模型缺少 runtime_version"))?;
    let global_seed = canonical
        .random_seed
        .parse::<u64>()
        .map_err(|error| invalid(format!("多腿实验随机种子无效: {error}")))?;
    let data_fingerprint = result
        .input_data_fingerprint
        .as_deref()
        .ok_or_else(|| invariant("多腿实验缺少输入数据指纹"))?;
    let model_bytes = serde_json::to_vec(&canonical.execution_model)
        .map_err(|error| invariant(format!("多腿实验执行模型编码失败: {error}")))?;
    let canonical_json = canonical
        .canonical_json()
        .map_err(|error| invariant(format!("多腿实验 CanonicalRunSpec 编码失败: {error}")))?;
    let canonical_fingerprint = canonical
        .fingerprint()
        .map_err(|error| invariant(format!("多腿实验 CanonicalRunSpec 指纹失败: {error}")))?;
    let dataset_quality = serde_json::json!({
        "primary": {"manifest_fingerprint": canonical.extensions.get("dataset_quality:primary").and_then(serde_json::Value::as_str)},
        "reference": {"manifest_fingerprint": canonical.extensions.get("dataset_quality:reference").and_then(serde_json::Value::as_str)},
    });
    let quality = canonical
        .extensions
        .contains_key("dataset_quality:primary")
        .then_some(&dataset_quality);
    let input_components =
        crate::cases::multi_leg_input_components(&canonical.datasets, quality, None)?;
    Ok(qx_core::RunManifest {
        run_id: result.experiment_id.clone(),
        code_commit: canonical.code_revision.clone(),
        config_hash: canonical_fingerprint.clone(),
        data_fingerprint: data_fingerprint.to_owned(),
        input_components,
        clock_start: canonical.clock.start_ts_ms,
        clock_end: canonical.clock.end_ts_ms,
        global_seed,
        determinism_mode: true,
        result_hash: result_hash.to_owned(),
        strategy_version: "multi-leg-experiment-strategy-set-v1".into(),
        instrument_spec_version: "multi-leg-experiment-v1".into(),
        model_fingerprint: qx_strategy::sha256_hex(&model_bytes),
        input_event_hash: data_fingerprint.to_owned(),
        output_event_hash: result_hash.to_owned(),
        runtime_version: runtime_version.to_owned(),
        canonical_run_spec_json: Some(canonical_json),
        canonical_run_spec_fingerprint: Some(canonical_fingerprint),
    })
}

fn publish_multi_leg_experiment_artifact(
    path: &Path,
    payload: &str,
    label: &str,
) -> Result<(), AppError> {
    crate::artifact_store::sink::publish(
        path,
        payload,
        crate::artifact_store::sink::PublishMode::Idempotent,
    )
    .map_err(|error| match error {
        crate::artifact_store::sink::PublishError::Conflict(message) => {
            AppError::new(AppErrorCategory::Conflict, message)
        }
        crate::artifact_store::sink::PublishError::Io(error) => {
            AppError::from_io(&format!("发布多腿实验 {label}"), &error)
        }
    })
}

fn verify_persisted_multi_leg_experiment_result(
    result: &RunMultiLegExperimentResult,
) -> Result<(), AppError> {
    let payload = result.to_json()?;
    let summary: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| invariant(format!("多腿实验结果 JSON 解码失败: {error}")))?;
    crate::artifact_store::verify_declared_run_manifest(&summary).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("多腿实验 RunManifest/RunRecord 读回失败: {error}"),
        )
    })
}

fn existing_result(
    payload: &str,
    expected_fingerprint: &str,
    experiment_id: &str,
    canonical_run_spec_json: &str,
    canonical_run_spec_fingerprint: &str,
    input_data_fingerprint: &str,
) -> Result<RunMultiLegExperimentResult, AppError> {
    let previous = RunMultiLegExperimentResult::from_json(payload)?;
    if previous.experiment_id != experiment_id || previous.spec_fingerprint != expected_fingerprint
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("experiment_id {experiment_id} 已绑定不同实验规格"),
        ));
    }
    match (
        previous.canonical_run_spec_json.as_deref(),
        previous.canonical_run_spec_fingerprint.as_deref(),
        previous.input_data_fingerprint.as_deref(),
    ) {
        (Some(old_json), Some(old_fingerprint), Some(old_data_fingerprint))
            if old_json == canonical_run_spec_json
                && old_fingerprint == canonical_run_spec_fingerprint
                && old_data_fingerprint == input_data_fingerprint =>
        {
            Ok(previous)
        }
        (None, None, None) => Err(AppError::new(
            AppErrorCategory::Conflict,
            format!(
                "experiment_id {experiment_id} 的历史报告缺少数据级 CanonicalRunSpec；请使用新的 experiment_id 重新运行，避免复用无法证明输入身份的结果"
            ),
        )),
        _ => Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("experiment_id {experiment_id} 已绑定不同数据或 CanonicalRunSpec"),
        )),
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn invariant(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}
