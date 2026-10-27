//! Bounded parameter experiments over the shared Rust Tick/OrderBook use case.

use super::artifacts::{read_text, write_text};
use super::experiment_grid::{
    apply_depth_parameters, expand_grid, normalized_strategy, validate_depth_grid,
    validate_experiment_id,
};
use super::experiment_spec_artifact::{
    serialize_experiment_input, verify_spec_artifact, write_spec_artifact,
};
use super::guard::guard_panics;
use super::run_depth_backtest::{run_inner, run_inner_with_experiment_strategy};
use super::run_experiment::{
    build_experiment_run_manifest, experiment_result_hash_from_json,
    normalize_experiment_identity_values, quote_unsafe_integer_tokens,
};
use super::{attach, ExperimentParameterSpace};
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, CallerCapability, DepthBacktestOutcome, DepthBacktestSpec,
    RunContext, RunHandle, StrategyArtifactIdentity,
};
use qx_core::{Fnv1a, RunManifest};
use qx_spec::{
    ArtifactRefV2, CanonicalDatasetIdentity, CanonicalRunClock, CanonicalRunSpec,
    CanonicalStrategyIdentity, ReplayVerdict, RunRecordV2, RunStatus,
};
use qx_strategy::{BuiltinStrategyKind, Strategy};
use qx_xingban::DepthFrame;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub const RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION: u32 = 1;

type StrategyFactory<'a> = dyn FnMut(&DepthBacktestSpec) -> Result<Box<dyn Strategy>, String> + 'a;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunDepthExperimentSpec {
    pub schema_version: u32,
    pub experiment_id: String,
    pub base: DepthBacktestSpec,
    pub parameter_space: Vec<ExperimentParameterSpace>,
    /// Optional strategy-selection dimension. Empty preserves the legacy spec fingerprint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strategy_kinds: Vec<BuiltinStrategyKind>,
}

impl RunDepthExperimentSpec {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload)
            .map_err(|error| invalid(format!("RunDepthExperimentSpec JSON 无效: {error}")))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        let mut canonical = self.clone();
        canonical.strategy_kinds.sort_by_key(|kind| kind.name());
        serde_json::to_string(&canonical)
            .map_err(|error| invariant(format!("RunDepthExperimentSpec 序列化失败: {error}")))
    }

    pub fn validate(&self) -> Result<usize, AppError> {
        if self.schema_version != RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION {
            return Err(invalid(format!(
                "RunDepthExperimentSpec schema_version={} 本版本只认 {}",
                self.schema_version, RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION
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
        if kinds.iter().any(|kind| kind.needs_reference_leg()) {
            return Err(invalid(
                "双腿策略必须使用 RunMultiLegExperimentSpec 和双数据集",
            ));
        }
        if !self.strategy_kinds.is_empty() && self.base.market_making.is_some() {
            return Err(invalid(
                "market_making 参数实验不能与 strategy_kinds 内置策略选择混用",
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
                validate_depth_grid(
                    &candidate.strategy,
                    candidate.market_making,
                    &self.parameter_space,
                )?
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

    /// Run a parameter grid with a fresh caller-authored strategy for every depth candidate.
    pub fn run_with_strategy_factory<F>(
        &self,
        identity: &StrategyArtifactIdentity,
        factory: &mut F,
        context: &RunContext,
    ) -> Result<RunDepthExperimentResult, AppError>
    where
        F: FnMut(&DepthBacktestSpec) -> Result<Box<dyn Strategy>, String>,
    {
        context.require(
            CallerCapability::Research,
            "多语言 Tick/OrderBook 策略参数实验",
        )?;
        validate_strategy_experiment(self, identity)?;
        let correlation_id = context.correlation_id().to_string();
        guard_panics(&correlation_id, || {
            run_depth_experiment_inner(self, context, None, Some(identity), Some(factory))
        })
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| {
            attach(
                invariant("同步多语言深度参数实验未产生报告"),
                &correlation_id,
            )
        })
    }

    /// Run the grid through the cancellable Rust Tick/OrderBook backtest path.
    pub fn start(self, context: RunContext) -> RunHandle<RunDepthExperimentResult> {
        let run_id = context.correlation_id().to_string();
        RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "Tick/OrderBook 参数实验")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_depth_experiment_inner(&self, &context, Some(&cancellation), None, None)
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }

    /// Start a cancellable parameter grid with a fresh caller-authored depth strategy per candidate.
    pub fn start_with_strategy_factory<F>(
        self,
        identity: StrategyArtifactIdentity,
        mut factory: F,
        context: RunContext,
    ) -> Result<RunHandle<RunDepthExperimentResult>, AppError>
    where
        F: FnMut(&DepthBacktestSpec) -> Result<Box<dyn Strategy>, String> + Send + 'static,
    {
        context.require(
            CallerCapability::Research,
            "多语言 Tick/OrderBook 策略参数实验",
        )?;
        validate_strategy_experiment(&self, &identity)?;
        let run_id = context.correlation_id().to_string();
        Ok(RunHandle::spawn(run_id, move |cancellation| {
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_depth_experiment_inner(
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
pub struct DepthExperimentCandidate {
    pub ordinal: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_kind: Option<String>,
    pub parameters: BTreeMap<String, i128>,
    pub outcome: Option<DepthBacktestOutcome>,
    /// Stable AppError JSON; candidate errors do not hide successful runs.
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepthExperimentRankedCandidate {
    pub rank: usize,
    pub ordinal: usize,
    pub run_id: String,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub result_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunDepthExperimentResult {
    pub schema_version: u32,
    pub experiment_id: String,
    pub spec_fingerprint: String,
    pub total_candidates: usize,
    pub completed_candidates: usize,
    pub succeeded_candidates: usize,
    pub failed_candidates: usize,
    pub candidates: Vec<DepthExperimentCandidate>,
    pub ranking: Vec<DepthExperimentRankedCandidate>,
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

impl RunDepthExperimentResult {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let result: Self = serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("RunDepthExperimentResult JSON 无法读取: {error}"),
            )
        })?;
        validate_depth_experiment_result(&result)?;
        Ok(result)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        validate_depth_experiment_result(self)?;
        serde_json::to_string(self)
            .map_err(|error| invariant(format!("RunDepthExperimentResult 序列化失败: {error}")))
    }
}

/// Expand and rank a bounded grid while every candidate uses the Rust depth engine.
pub fn run_depth_experiment(
    spec: &RunDepthExperimentSpec,
    context: &RunContext,
) -> Result<RunDepthExperimentResult, AppError> {
    context.require(CallerCapability::Research, "Tick/OrderBook 参数实验")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        run_depth_experiment_inner(spec, context, None, None, None)
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| {
        attach(
            invariant("同步 Tick/OrderBook 参数实验未产生报告"),
            &correlation_id,
        )
    })
}

fn validate_strategy_experiment(
    spec: &RunDepthExperimentSpec,
    identity: &StrategyArtifactIdentity,
) -> Result<(), AppError> {
    spec.validate()?;
    if identity.strategy_id != spec.base.strategy.strategy_id {
        return Err(invalid(
            "caller strategy identity must match the primary depth strategy slot",
        ));
    }
    if !spec.strategy_kinds.is_empty() {
        return Err(invalid(
            "caller strategy factory cannot be combined with built-in strategy_kinds selection",
        ));
    }
    identity.validate_for_depth_experiment(&spec.base)?;
    Ok(())
}

fn run_depth_experiment_inner(
    spec: &RunDepthExperimentSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    identity: Option<&StrategyArtifactIdentity>,
    mut factory: Option<&mut StrategyFactory<'_>>,
) -> Result<Option<RunDepthExperimentResult>, AppError> {
    let total = spec.validate()?;
    let mut fingerprint = Fnv1a::new();
    fingerprint.write_text(&spec.to_json()?);
    if let Some(identity) = identity {
        fingerprint.write_text(&identity.strategy_id);
        fingerprint.write_text(&identity.strategy_version);
        fingerprint.write_text(&identity.source_sha256);
    }
    let spec_fingerprint = format!("{:016x}", fingerprint.finish());
    let spec_json = spec.to_json()?;
    let persisted_input = serialize_experiment_input(&spec_json, context.code_commit(), identity)?;
    let frame_payload = read_text(&spec.base.depth_path)?;
    let frame = DepthFrame::from_json(&frame_payload).map_err(|error| {
        invalid(format!(
            "深度参数实验 DepthFrame 校验失败 {}: {error}",
            spec.base.depth_path
        ))
    })?;
    if frame.snapshots.is_empty() {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            "Tick/OrderBook 参数实验至少需要一份盘口快照",
        ));
    }
    let input_data_fingerprint = format!("depth-frame-fnv1a64:{:016x}", frame.input_hash());
    let quality = spec
        .base
        .quality_manifest_path
        .as_deref()
        .map(std::path::Path::new)
        .map(|path| {
            super::run_depth_backtest::quality::verified_depth_dataset_quality(
                path,
                std::path::Path::new(&spec.base.depth_path),
                &frame,
                &spec.base.tier,
            )
        })
        .transpose()?;
    let quality_fingerprint = quality
        .as_ref()
        .and_then(|quality| quality.get("manifest_fingerprint"))
        .and_then(Value::as_str);
    let canonical_spec = build_depth_experiment_canonical_run_spec(
        spec,
        &spec_json,
        &frame,
        identity,
        quality_fingerprint,
        context,
    )?;
    let canonical_json = canonical_spec
        .canonical_json()
        .map_err(|error| invariant(format!("深度实验 CanonicalRunSpec 规范化失败: {error}")))?;
    let canonical_fingerprint = canonical_spec
        .fingerprint()
        .map_err(|error| invariant(format!("深度实验 CanonicalRunSpec 指纹失败: {error}")))?;
    let root = PathBuf::from(&spec.base.output_dir).join(&spec.experiment_id);
    let artifact_path = root
        .join(format!("{}.depth-experiment.json", spec.experiment_id))
        .to_string_lossy()
        .into_owned();
    let spec_artifact_path = root
        .join(format!("{}.depth-experiment.spec.json", spec.experiment_id))
        .to_string_lossy()
        .into_owned();
    match std::fs::read_to_string(&artifact_path) {
        Ok(existing) => {
            let existing = existing_result(
                &existing,
                &spec_fingerprint,
                &canonical_json,
                &canonical_fingerprint,
                &input_data_fingerprint,
                quality_fingerprint,
                &spec.experiment_id,
                context,
                &canonical_spec,
                &root,
                &spec_artifact_path,
                &persisted_input,
            )?;
            return Ok(Some(existing));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取既有深度参数实验报告", &error)),
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
        let outcome = match apply_depth_parameters(
            &mut candidate.strategy,
            &mut candidate.market_making,
            &parameters,
        ) {
            Ok(()) => match DepthBacktestSpec::validate_entry(
                &candidate.tier,
                candidate.strategy.kind,
                candidate.strategy.quantity_raw,
                candidate.fee_bps,
            ) {
                Err(error) => Err(error),
                Ok(()) => {
                    let run = match (identity, factory.as_deref_mut()) {
                        (Some(identity), Some(factory)) => factory(&candidate)
                            .map_err(|error| {
                                invalid(format!("候选 {ordinal} 自定义策略 factory 失败: {error}"))
                            })
                            .and_then(|strategy| {
                                run_inner_with_experiment_strategy(
                                    &candidate,
                                    context,
                                    cancellation,
                                    identity.clone(),
                                    strategy,
                                )
                            }),
                        (None, None) => run_inner(&candidate, context, cancellation),
                        _ => Err(invariant("自定义深度实验身份与 factory 状态不一致")),
                    };
                    match run {
                        Ok(Some(outcome)) => Ok(outcome),
                        Ok(None) => return Ok(None),
                        Err(error) => Err(error),
                    }
                }
            },
            Err(error) => Err(error),
        };
        candidates.push(match outcome {
            Ok(outcome) => DepthExperimentCandidate {
                ordinal,
                strategy_kind: (!spec.strategy_kinds.is_empty()).then(|| kind.name().to_string()),
                parameters,
                outcome: Some(outcome),
                error: None,
            },
            Err(error) => DepthExperimentCandidate {
                ordinal,
                strategy_kind: (!spec.strategy_kinds.is_empty()).then(|| kind.name().to_string()),
                parameters,
                outcome: None,
                error: Some(error.to_json()),
            },
        });
    }

    let mut ranking = candidates
        .iter()
        .filter_map(|candidate| {
            candidate.outcome.as_ref().map(|outcome| {
                (
                    candidate.ordinal,
                    outcome.run_id.clone(),
                    outcome.return_bps,
                    outcome.max_drawdown_bps,
                    outcome.result_hash.clone(),
                )
            })
        })
        .collect::<Vec<_>>();
    ranking.sort_by(|left, right| {
        right
            .2
            .cmp(&left.2)
            .then_with(|| left.3.cmp(&right.3))
            .then_with(|| left.0.cmp(&right.0))
    });
    let ranking = ranking
        .into_iter()
        .enumerate()
        .map(
            |(index, (ordinal, run_id, return_bps, max_drawdown_bps, result_hash))| {
                DepthExperimentRankedCandidate {
                    rank: index + 1,
                    ordinal,
                    run_id,
                    return_bps,
                    max_drawdown_bps,
                    result_hash,
                }
            },
        )
        .collect::<Vec<_>>();
    let succeeded_candidates = ranking.len();
    let result = RunDepthExperimentResult {
        schema_version: RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION,
        experiment_id: spec.experiment_id.clone(),
        spec_fingerprint: spec_fingerprint.clone(),
        total_candidates: total,
        completed_candidates: candidates.len(),
        succeeded_candidates,
        failed_candidates: total - succeeded_candidates,
        candidates,
        ranking,
        artifact_path: artifact_path.clone(),
        canonical_run_spec_json: Some(canonical_json.clone()),
        canonical_run_spec_fingerprint: Some(canonical_fingerprint.clone()),
        input_data_fingerprint: Some(input_data_fingerprint.clone()),
        result_hash: None,
        run_manifest: None,
        run_record: None,
        artifact_locations: None,
    };
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        return Ok(None);
    }
    std::fs::create_dir_all(&root)
        .map_err(|error| AppError::from_io("创建深度参数实验产物目录", &error))?;
    write_spec_artifact(&spec_artifact_path, &persisted_input, "深度参数实验")?;
    verify_depth_experiment_candidate_outcomes(
        &result,
        context,
        &input_data_fingerprint,
        quality_fingerprint,
    )?;
    let persisted = match persist_depth_experiment_result(
        result,
        &canonical_spec,
        &root,
        &artifact_path,
        &spec_artifact_path,
    ) {
        Ok(persisted) => persisted,
        Err(write_error) => {
            if let Ok(existing) = std::fs::read_to_string(&artifact_path) {
                let recovered = existing_result(
                    &existing,
                    &spec_fingerprint,
                    &canonical_json,
                    &canonical_fingerprint,
                    &input_data_fingerprint,
                    quality_fingerprint,
                    &spec.experiment_id,
                    context,
                    &canonical_spec,
                    &root,
                    &spec_artifact_path,
                    &persisted_input,
                )?;
                return Ok(Some(recovered));
            }
            return Err(write_error);
        }
    };
    Ok(Some(persisted))
}

fn build_depth_experiment_canonical_run_spec(
    spec: &RunDepthExperimentSpec,
    spec_json: &str,
    frame: &DepthFrame,
    identity: Option<&StrategyArtifactIdentity>,
    quality_fingerprint: Option<&str>,
    context: &RunContext,
) -> Result<CanonicalRunSpec, AppError> {
    let first = frame
        .snapshots
        .first()
        .ok_or_else(|| invalid("深度实验 CanonicalRunSpec 缺少开始快照"))?;
    let last = frame
        .snapshots
        .last()
        .ok_or_else(|| invalid("深度实验 CanonicalRunSpec 缺少结束快照"))?;
    let portable_spec_json = quote_unsafe_integer_tokens(spec_json);
    let mut behavior: Value = serde_json::from_str(&portable_spec_json)
        .map_err(|error| invalid(format!("深度实验规格规范化 JSON 无效: {error}")))?;
    let fields = behavior
        .as_object_mut()
        .ok_or_else(|| invalid("深度实验规格规范化结果不是 JSON 对象"))?;
    fields.remove("experiment_id");
    let base = fields
        .get_mut("base")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| invalid("深度实验 CanonicalRunSpec 缺少 base"))?;
    for field in [
        "run_id",
        "manifest_run_id",
        "depth_path",
        "output_dir",
        "quality_manifest_path",
        "runtime_config_path",
        "market_spec_path",
        "market_spec_source",
        "execution_cost_source",
        "account_base_source",
        "signal_source",
        "signal_declared_unused",
        "declared_ashare_rules",
    ] {
        base.remove(field);
    }
    normalize_experiment_identity_values(&mut behavior, "behavior")?;

    let mut kinds = if spec.strategy_kinds.is_empty() {
        vec![spec.base.strategy.kind]
    } else {
        spec.strategy_kinds.clone()
    };
    kinds.sort_by_key(|kind| kind.name());
    let mut strategies = Vec::new();
    for kind in kinds {
        let mut strategy = spec.base.strategy.clone();
        strategy.kind = kind;
        let mut parameters = serde_json::to_value(&strategy)
            .map_err(|error| invariant(format!("深度实验策略参数编码失败: {error}")))?;
        normalize_experiment_identity_values(&mut parameters, "strategies.parameters")?;
        let strategy_id = if spec.strategy_kinds.is_empty() {
            strategy.strategy_id.clone()
        } else {
            format!("{}#{}", strategy.strategy_id, kind.name())
        };
        strategies.push(CanonicalStrategyIdentity {
            strategy_id,
            strategy_version: identity
                .filter(|identity| identity.strategy_id == strategy.strategy_id)
                .map(|identity| identity.strategy_version.clone())
                .unwrap_or_else(|| super::run_backtest::strategy_slot::strategy_version(&strategy)),
            source_sha256: identity
                .filter(|identity| identity.strategy_id == strategy.strategy_id)
                .map(|identity| identity.source_sha256.clone()),
            parameters,
        });
    }
    if let Some(portfolio) = &spec.base.portfolio {
        for member in &portfolio.additional_strategies {
            let mut parameters = serde_json::to_value(&member.strategy)
                .map_err(|error| invariant(format!("深度实验组合策略参数编码失败: {error}")))?;
            normalize_experiment_identity_values(&mut parameters, "strategies.parameters")?;
            strategies.push(CanonicalStrategyIdentity {
                strategy_id: member.strategy.strategy_id.clone(),
                strategy_version: format!("{}-v1", member.strategy.strategy_id),
                source_sha256: None,
                parameters,
            });
        }
    }

    let mut market_rules = BTreeMap::new();
    let instrument_spec_version = match spec.base.market_spec_source.as_deref() {
        Some(source) => source.to_owned(),
        None if spec.base.market_spec.is_some() => "trading-instrument-spec-v1".into(),
        None => "instrument-spec:none".into(),
    };
    market_rules.insert(frame.instrument.to_string(), instrument_spec_version);
    let risk = crate::cases::risk_gate::risk_gate_for(
        spec.base.risk_rules.as_ref(),
        spec.base.allow_short(),
    );
    let extensions = quality_fingerprint
        .map(|fingerprint| {
            BTreeMap::from([(
                "dataset_quality_fingerprint".into(),
                Value::String(fingerprint.into()),
            )])
        })
        .unwrap_or_default();
    let execution_model = serde_json::json!({
        "runtime_version": crate::cases::run_depth_backtest::DEPTH_RUNTIME_VERSION,
        "matching_kernel": if spec.base.tier == "l1" {
            crate::cases::kernels::TICK_MATCHING_KERNEL
        } else {
            crate::cases::kernels::ORDER_BOOK_MATCHING_KERNEL
        },
        "risk_rule_set_version": risk.rule_set().version(),
        "account_id": spec.base.account_id.as_deref().unwrap_or("app"),
        "effective_fee_bps": spec.base.resolved_fee_bps().to_string(),
        "latency_snapshots": spec.base.latency_snapshots,
        "queue_position_bps": spec.base.queue_position_bps.to_string(),
        "queue_position_model": spec.base.queue_position_model,
        "market_impact_bps": spec.base.market_impact_bps.to_string(),
    });
    CanonicalRunSpec::new(
        "depth_experiment",
        context.code_commit(),
        strategies,
        vec![CanonicalDatasetIdentity {
            role: "depth_snapshots".into(),
            instrument: frame.instrument.to_string(),
            content_fingerprint: format!("depth-frame-fnv1a64:{:016x}", frame.input_hash()),
            start_ts_ms: first.ts,
            end_ts_ms: last.ts,
        }],
        market_rules,
        CanonicalRunClock {
            start_ts_ms: first.ts,
            end_ts_ms: last.ts,
            timestamp_unit: "unix_ms".into(),
            timezone: "UTC".into(),
        },
        0,
        behavior,
        execution_model,
        extensions,
    )
    .map_err(|error| invalid(format!("深度实验 CanonicalRunSpec 无效: {error}")))
}

fn verify_depth_experiment_candidate_outcomes(
    result: &RunDepthExperimentResult,
    context: &RunContext,
    expected_input_fingerprint: &str,
    expected_quality_fingerprint: Option<&str>,
) -> Result<(), AppError> {
    let mut verified = BTreeSet::new();
    for candidate in &result.candidates {
        let Some(outcome) = candidate.outcome.as_ref() else {
            continue;
        };
        if !verified.insert(outcome.run_id.clone()) {
            continue;
        }
        let verification = crate::cases::verify_depth_run(outcome, context)?;
        if !verification.verified {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                format!(
                    "深度实验候选 {} 产物复核失败: {}",
                    outcome.run_id,
                    verification.mismatches.join("; ")
                ),
            ));
        }
        let manifest_payload = read_text(&outcome.artifacts.run_manifest)?;
        let manifest = RunManifest::from_json(&manifest_payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("深度实验候选 {} RunManifest 无效: {error}", outcome.run_id),
            )
        })?;
        let candidate_spec = manifest
            .canonical_run_spec_json
            .as_deref()
            .ok_or_else(|| invalid("深度实验候选 RunManifest 缺少 CanonicalRunSpec"))
            .and_then(|payload| {
                CanonicalRunSpec::from_json(payload)
                    .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))
            })?;
        if candidate_spec.run_type != "depth_backtest"
            || candidate_spec.datasets.len() != 1
            || candidate_spec.datasets[0].content_fingerprint != expected_input_fingerprint
            || candidate_spec.datasets[0].instrument != outcome.instrument
            || candidate_spec
                .extensions
                .get("dataset_quality_fingerprint")
                .and_then(Value::as_str)
                != expected_quality_fingerprint
        {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("深度实验候选 {} 绑定了不同的深度数据集", outcome.run_id),
            ));
        }
    }
    Ok(())
}

fn persist_depth_experiment_result(
    mut result: RunDepthExperimentResult,
    canonical_spec: &CanonicalRunSpec,
    root: &std::path::Path,
    report_index_path: &str,
    spec_artifact_path: &str,
) -> Result<RunDepthExperimentResult, AppError> {
    let result_payload = serde_json::to_string(&result)
        .map_err(|error| invariant(format!("深度实验事实摘要编码失败: {error}")))?;
    let result_hash = experiment_result_hash_from_json(&result_payload)
        .map_err(|error| invariant(format!("深度实验事实摘要规范化失败: {error}")))?;
    result.result_hash = Some(result_hash.clone());
    let manifest =
        build_experiment_run_manifest(&result.experiment_id, canonical_spec, &result_hash)?;
    let manifest_path = crate::artifact_store::persist_run_manifest(root, &manifest)?;
    let report_path = root.join(format!(
        "{}-{result_hash}-input-v1.depth-experiment.json",
        result.experiment_id
    ));
    let record_path = root.join(format!(
        "{}-{result_hash}-input-v1.record.json",
        result.experiment_id
    ));
    result.artifact_path = report_index_path.to_owned();
    result.run_manifest = Some(manifest_path.to_string_lossy().into_owned());
    result.run_record = Some(record_path.to_string_lossy().into_owned());
    result.artifact_locations = Some(BTreeMap::from([
        (
            "experiment_report".into(),
            report_path.to_string_lossy().into_owned(),
        ),
        (
            "run_manifest".into(),
            manifest_path.to_string_lossy().into_owned(),
        ),
        ("experiment_input".into(), spec_artifact_path.to_owned()),
    ]));
    let report_payload = result.to_json()?;
    let manifest_payload = manifest
        .to_json()
        .map_err(|error| invariant(format!("深度实验 RunManifest 编码失败: {error}")))?;
    let report_bytes = report_payload.as_bytes();
    let manifest_bytes = manifest_payload.as_bytes();
    let spec_bytes = std::fs::read(spec_artifact_path)
        .map_err(|error| AppError::from_io("读取深度参数实验输入规格 sidecar", &error))?;
    let record = RunRecordV2 {
        schema_version: qx_spec::RUN_RECORD_V2_SCHEMA_VERSION,
        run_id: result.experiment_id.clone(),
        status: RunStatus::Completed,
        input_digest: result
            .input_data_fingerprint
            .clone()
            .ok_or_else(|| invariant("深度实验输入数据指纹缺失"))?,
        code_identity: format!("{}:{}", manifest.code_commit, manifest.runtime_version),
        config_fingerprint: result
            .canonical_run_spec_fingerprint
            .clone()
            .ok_or_else(|| invariant("深度实验 CanonicalRunSpec 指纹缺失"))?,
        started_at_ms: manifest.clock_start,
        finished_at_ms: Some(manifest.clock_end),
        artifact_refs: vec![
            ArtifactRefV2 {
                name: "experiment_report".into(),
                artifact_id: qx_strategy::sha256_hex(report_bytes),
                kind: "run-depth-experiment-result-json".into(),
                schema_version: RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION,
                byte_len: report_bytes.len() as u64,
            },
            ArtifactRefV2 {
                name: "run_manifest".into(),
                artifact_id: qx_strategy::sha256_hex(manifest_bytes),
                kind: "run-manifest-json".into(),
                schema_version: 1,
                byte_len: manifest_bytes.len() as u64,
            },
            ArtifactRefV2 {
                name: "experiment_input".into(),
                artifact_id: qx_strategy::sha256_hex(&spec_bytes),
                kind: "run-experiment-input-json".into(),
                schema_version: 1,
                byte_len: spec_bytes.len() as u64,
            },
        ],
        replay_verdict: ReplayVerdict::Verified,
        capability_level: qx_spec::CapabilityLevel::L1,
    };
    let record_payload = record
        .to_json()
        .map_err(|error| invariant(format!("深度实验 RunRecord V2 编码失败: {error}")))?;
    publish_depth_experiment_artifact(&record_path, &record_payload, "RunRecord V2")?;
    publish_depth_experiment_artifact(&report_path, &report_payload, "参数实验报告")?;
    write_text(report_index_path, &report_payload)?;
    // Verify through the standard read path without materializing candidate i128s into Value;
    // serde_json::Value cannot preserve every signed 128-bit integer on all feature sets.
    verify_persisted_depth_experiment_result(&result)?;
    Ok(result)
}

fn publish_depth_experiment_artifact(
    path: &std::path::Path,
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
            AppError::from_io(&format!("发布深度实验 {label}"), &error)
        }
    })
}

fn validate_depth_experiment_result(result: &RunDepthExperimentResult) -> Result<(), AppError> {
    match (
        result.canonical_run_spec_json.as_deref(),
        result.canonical_run_spec_fingerprint.as_deref(),
    ) {
        (None, None) => {}
        (Some(payload), Some(expected_fingerprint)) => {
            let canonical = CanonicalRunSpec::from_json(payload)
                .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))?;
            let canonical_json = canonical
                .canonical_json()
                .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))?;
            let fingerprint = canonical
                .fingerprint()
                .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))?;
            if canonical_json != payload
                || fingerprint != expected_fingerprint
                || canonical.run_type != "depth_experiment"
                || canonical.datasets.len() != 1
                || canonical.datasets[0].role != "depth_snapshots"
                || result.input_data_fingerprint.as_deref()
                    != Some(canonical.datasets[0].content_fingerprint.as_str())
                || result
                    .candidates
                    .iter()
                    .filter_map(|candidate| candidate.outcome.as_ref())
                    .any(|outcome| canonical.datasets[0].instrument != outcome.instrument)
            {
                return Err(AppError::new(
                    AppErrorCategory::StorageFailure,
                    "深度实验 CanonicalRunSpec 与报告数据身份或指纹不一致",
                ));
            }
        }
        _ => {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                "深度实验 CanonicalRunSpec JSON 与 fingerprint 必须同时存在或同时缺省",
            ));
        }
    }
    match (
        result.result_hash.as_deref(),
        result.run_manifest.as_deref(),
        result.run_record.as_deref(),
        result.artifact_locations.as_ref(),
    ) {
        (None, None, None, None) => Ok(()),
        (Some(result_hash), Some(manifest), Some(record), Some(locations)) => {
            let valid_hash = result_hash.len() == 16
                && result_hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
            if !valid_hash
                || manifest.trim().is_empty()
                || record.trim().is_empty()
                || result.canonical_run_spec_json.is_none()
                || result.canonical_run_spec_fingerprint.is_none()
                || result.input_data_fingerprint.is_none()
                || !matches!(locations.len(), 2 | 3)
                || (locations.len() == 3
                    && locations
                        .get("experiment_input")
                        .is_none_or(|path| path.trim().is_empty()))
                || locations
                    .get("experiment_report")
                    .is_none_or(|path| path.trim().is_empty() || path == &result.artifact_path)
                || locations.get("run_manifest").map(String::as_str) != Some(manifest)
            {
                return Err(AppError::new(
                    AppErrorCategory::StorageFailure,
                    "深度实验标准产物指针或 ArtifactLocations 无效",
                ));
            }
            let payload = serde_json::to_string(result)
                .map_err(|error| invariant(format!("深度实验事实摘要编码失败: {error}")))?;
            let actual_hash = experiment_result_hash_from_json(&payload)
                .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))?;
            if actual_hash != result_hash {
                return Err(AppError::new(
                    AppErrorCategory::StorageFailure,
                    "深度实验 result_hash 与报告事实不一致",
                ));
            }
            Ok(())
        }
        _ => Err(AppError::new(
            AppErrorCategory::StorageFailure,
            "深度实验 result_hash、RunManifest、RunRecord 和 ArtifactLocations 必须全部存在或全部缺省",
        )),
    }
}

fn existing_result(
    payload: &str,
    expected_fingerprint: &str,
    expected_canonical_json: &str,
    expected_canonical_fingerprint: &str,
    expected_input_fingerprint: &str,
    expected_quality_fingerprint: Option<&str>,
    experiment_id: &str,
    context: &RunContext,
    canonical_spec: &CanonicalRunSpec,
    root: &std::path::Path,
    spec_artifact_path: &str,
    persisted_input: &str,
) -> Result<RunDepthExperimentResult, AppError> {
    let mut previous = RunDepthExperimentResult::from_json(payload)?;
    let same_spec = previous.canonical_run_spec_fingerprint.as_deref().map_or(
        previous.spec_fingerprint == expected_fingerprint,
        |fingerprint| fingerprint == expected_canonical_fingerprint,
    );
    if previous.experiment_id != experiment_id || !same_spec {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("experiment_id {experiment_id} 已绑定不同 Tick/OrderBook 实验规格"),
        ));
    }
    if previous
        .canonical_run_spec_fingerprint
        .as_deref()
        .is_some_and(|fingerprint| fingerprint != expected_canonical_fingerprint)
        || previous
            .canonical_run_spec_json
            .as_deref()
            .is_some_and(|payload| payload != expected_canonical_json)
        || previous
            .input_data_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| fingerprint != expected_input_fingerprint)
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("experiment_id {experiment_id} 已绑定不同深度数据或 CanonicalRunSpec"),
        ));
    }
    verify_depth_experiment_candidate_outcomes(
        &previous,
        context,
        expected_input_fingerprint,
        expected_quality_fingerprint,
    )?;
    if previous.result_hash.is_some() {
        verify_persisted_depth_experiment_result(&previous)?;
        if previous
            .artifact_locations
            .as_ref()
            .and_then(|locations| locations.get("experiment_input"))
            .is_some_and(|path| path == spec_artifact_path)
        {
            verify_spec_artifact(spec_artifact_path, persisted_input, "深度参数实验")?;
        } else {
            write_spec_artifact(spec_artifact_path, persisted_input, "深度参数实验")?;
            let report_index_path = previous.artifact_path.clone();
            return persist_depth_experiment_result(
                previous,
                canonical_spec,
                root,
                &report_index_path,
                spec_artifact_path,
            );
        }
        return Ok(previous);
    }
    previous.canonical_run_spec_json = Some(expected_canonical_json.into());
    previous.canonical_run_spec_fingerprint = Some(expected_canonical_fingerprint.into());
    previous.input_data_fingerprint = Some(expected_input_fingerprint.into());
    let report_index_path = previous.artifact_path.clone();
    write_spec_artifact(spec_artifact_path, persisted_input, "深度参数实验")?;
    let upgraded = persist_depth_experiment_result(
        previous,
        canonical_spec,
        root,
        &report_index_path,
        spec_artifact_path,
    )?;
    Ok(upgraded)
}

fn verify_persisted_depth_experiment_result(
    result: &RunDepthExperimentResult,
) -> Result<(), AppError> {
    let verification_summary = serde_json::json!({
        "schema_version": result.schema_version,
        "experiment_id": result.experiment_id,
        "artifact_path": result.artifact_path,
        "canonical_run_spec_json": result.canonical_run_spec_json,
        "canonical_run_spec_fingerprint": result.canonical_run_spec_fingerprint,
        "input_data_fingerprint": result.input_data_fingerprint,
        "result_hash": result.result_hash,
        "run_manifest": result.run_manifest,
        "run_record": result.run_record,
        "artifact_locations": result.artifact_locations,
    });
    crate::artifact_store::verify_declared_run_manifest(&verification_summary).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("深度实验 RunManifest/RunRecord 读回失败: {error}"),
        )
    })
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn invariant(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}
