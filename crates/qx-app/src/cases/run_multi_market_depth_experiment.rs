//! Bounded strategy/parameter experiments over the shared multi-market L1/L2 portfolio engine.

use super::artifacts::write_text;
use super::attach;
use super::experiment_grid::{
    apply_parameters, expand_grid, normalized_strategy, validate_experiment_id, validate_grid,
};
use super::experiment_spec_artifact::{serialize_experiment_input, write_spec_artifact};
use super::guard::guard_panics;
use super::run_experiment::{
    experiment_result_hash_from_json, normalize_experiment_identity_values,
    quote_unsafe_integer_tokens,
};
use super::run_multi_market_depth_backtest::{
    build_multi_market_depth_canonical_run_spec, run_candidate,
};
use crate::run_handle::CancellationToken;
use crate::{
    multi_market_depth_input_set_identity_v1, AppError, AppErrorCategory, CallerCapability,
    MultiMarketDepthBacktestSpec, MultiMarketDepthInputIdentityV1, RunContext, RunHandle,
    StrategyArtifactIdentity,
};
use qx_core::Fnv1a;
use qx_spec::{ArtifactRefV2, CanonicalRunSpec, ReplayVerdict, RunRecordV2, RunStatus};
use qx_strategy::{BuiltinStrategyKind, Strategy};
use qx_xingban::{DepthFrame, OrderBookExecutionDescriptor};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

mod spec;
mod types;
pub use types::*;

pub const RUN_MULTI_MARKET_DEPTH_EXPERIMENT_SCHEMA_VERSION: u32 = 1;
pub const MAX_MULTI_MARKET_DEPTH_EXPERIMENT_CANDIDATES: usize = 256;

impl RunMultiMarketDepthExperimentSpec {
    pub fn start(self, context: RunContext) -> RunHandle<RunMultiMarketDepthExperimentResult> {
        let run_id = context.correlation_id().to_string();
        RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "多市场盘口参数实验")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(&self, &context, Some(&cancellation), None, None)
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }
}

type StrategyFactory<'a> = dyn FnMut(
        &MultiMarketDepthBacktestSpec,
        &BTreeMap<String, i128>,
    ) -> Result<Box<dyn Strategy>, String>
    + 'a;

pub fn run_multi_market_depth_experiment(
    spec: &RunMultiMarketDepthExperimentSpec,
    context: &RunContext,
) -> Result<RunMultiMarketDepthExperimentResult, AppError> {
    let correlation_id = context.correlation_id().to_string();
    context.require(CallerCapability::Research, "多市场盘口参数实验")?;
    guard_panics(&correlation_id, || {
        run_inner(spec, context, None, None, None)
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| attach(invariant("同步多市场盘口实验未产生报告"), &correlation_id))
}

fn run_inner(
    spec: &RunMultiMarketDepthExperimentSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    identity: Option<&StrategyArtifactIdentity>,
    mut factory: Option<&mut StrategyFactory<'_>>,
) -> Result<Option<RunMultiMarketDepthExperimentResult>, AppError> {
    let total = spec.validate()?;
    let mut fingerprint = Fnv1a::new();
    fingerprint.write_text(&spec.to_json()?);
    if let Some(identity) = identity {
        fingerprint.write_text(&identity.strategy_id);
        fingerprint.write_text(&identity.strategy_version);
        fingerprint.write_text(&identity.source_sha256);
    }
    let spec_fingerprint = format!("{:016x}", fingerprint.finish());
    let root = PathBuf::from(&spec.base.output_dir).join(&spec.experiment_id);
    let artifact_path = root
        .join(format!(
            "{}.multi-market-depth-experiment.json",
            spec.experiment_id
        ))
        .to_string_lossy()
        .into_owned();
    let spec_path = artifact_path.replace(
        ".multi-market-depth-experiment.json",
        ".multi-market-depth-experiment.spec.json",
    );
    let canonical_spec =
        serialize_experiment_input(&spec.to_json()?, context.code_commit(), identity)?;
    let (canonical_run_spec, input_data_fingerprint) =
        build_multi_market_depth_experiment_identity(spec, identity, context)?;
    let canonical_run_spec_json = canonical_run_spec
        .canonical_json()
        .map_err(|error| invariant(format!("多市场盘口实验 CanonicalRunSpec 无效: {error}")))?;
    let canonical_run_spec_fingerprint = canonical_run_spec
        .fingerprint()
        .map_err(|error| invariant(format!("多市场盘口实验 CanonicalRunSpec 指纹失败: {error}")))?;
    match std::fs::read_to_string(&artifact_path) {
        Ok(existing) => {
            let result = existing_result(
                &existing,
                &spec_fingerprint,
                &canonical_run_spec_json,
                &canonical_run_spec_fingerprint,
                &input_data_fingerprint,
                &spec.experiment_id,
                &canonical_run_spec,
                &root,
                &spec_path,
                &canonical_spec,
            )?;
            return Ok(Some(result));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取既有多市场盘口实验报告", &error)),
    }

    let target_kind = target_market(spec)?.strategy.kind;
    let mut kinds = if spec.strategy_kinds.is_empty() {
        vec![target_kind]
    } else {
        spec.strategy_kinds.clone()
    };
    kinds.sort_by_key(|kind| kind.name());
    let active_parameter_space = if factory.is_some() {
        &spec.callback_parameter_space
    } else {
        if !spec.callback_parameter_space.is_empty() {
            return Err(invalid(
                "callback_parameter_space 只能与自定义策略工厂一起使用",
            ));
        }
        &spec.parameter_space
    };
    let combinations = if active_parameter_space.is_empty() {
        vec![BTreeMap::new()]
    } else {
        expand_grid(active_parameter_space)
    };
    let mut candidates = Vec::with_capacity(total);
    for kind in kinds {
        for parameters in &combinations {
            if cancellation.is_some_and(CancellationToken::is_cancelled) {
                return Ok(None);
            }
            let ordinal = candidates.len() + 1;
            let mut candidate = spec.base.clone();
            candidate.run_id = format!("{}-{ordinal:04}", spec.experiment_id);
            candidate.output_dir = root
                .join(format!("run-{ordinal:04}"))
                .to_string_lossy()
                .into_owned();
            let target = candidate
                .markets
                .iter_mut()
                .find(|market| market.strategy.strategy_id == spec.target_strategy_id)
                .ok_or_else(|| invariant("validated depth target disappeared"))?;
            target.strategy.kind = kind;
            if factory.is_none() {
                if let Err(error) = apply_parameters(&mut target.strategy, parameters) {
                    candidates.push(failed_candidate(
                        ordinal,
                        kind,
                        !spec.strategy_kinds.is_empty(),
                        parameters.clone(),
                        error,
                    ));
                    continue;
                }
            }
            let candidate_id = format!("{}-{ordinal:04}", spec.experiment_id);
            let injected = match (identity, factory.as_deref_mut()) {
                (Some(identity), Some(factory)) => {
                    let strategy = match guard_panics(&candidate_id, || {
                        factory(&candidate, parameters).map_err(invalid)
                    }) {
                        Ok(strategy) => strategy,
                        Err(error) => {
                            candidates.push(failed_candidate(
                                ordinal,
                                kind,
                                false,
                                parameters.clone(),
                                error,
                            ));
                            continue;
                        }
                    };
                    let mut binding = candidate.clone();
                    let binding_target = binding
                        .markets
                        .iter_mut()
                        .find(|market| market.strategy.strategy_id == spec.target_strategy_id)
                        .ok_or_else(|| invariant("validated depth target disappeared"))?;
                    binding_target.strategy = target_market(spec)?.strategy.clone();
                    match validate_injected_binding(&binding, identity, strategy) {
                        Ok(strategy) => Some(strategy),
                        Err(error) => {
                            candidates.push(failed_candidate(
                                ordinal,
                                kind,
                                false,
                                parameters.clone(),
                                error,
                            ));
                            continue;
                        }
                    }
                }
                (None, None) => None,
                _ => return Err(invariant("多市场盘口实验策略工厂和身份必须同时提供")),
            };
            let candidate_result = guard_panics(&candidate_id, || {
                run_candidate(&candidate, context, cancellation, injected)
            });
            match candidate_result {
                Ok(Some(outcome)) => candidates.push(MultiMarketDepthExperimentCandidate {
                    ordinal,
                    strategy_kind: (!spec.strategy_kinds.is_empty())
                        .then(|| kind.name().to_string()),
                    parameters: parameters.clone(),
                    outcome: Some(outcome),
                    error: None,
                }),
                Ok(None) => return Ok(None),
                Err(error) => candidates.push(failed_candidate(
                    ordinal,
                    kind,
                    !spec.strategy_kinds.is_empty(),
                    parameters.clone(),
                    error,
                )),
            }
        }
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
                    outcome.shared_result_hash.clone(),
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
            |(index, (ordinal, run_id, return_bps, max_drawdown_bps, shared_result_hash))| {
                MultiMarketDepthExperimentRankedCandidate {
                    rank: index + 1,
                    ordinal,
                    run_id,
                    return_bps,
                    max_drawdown_bps,
                    shared_result_hash,
                }
            },
        )
        .collect::<Vec<_>>();
    let succeeded_candidates = ranking.len();
    let result = RunMultiMarketDepthExperimentResult {
        schema_version: RUN_MULTI_MARKET_DEPTH_EXPERIMENT_SCHEMA_VERSION,
        experiment_id: spec.experiment_id.clone(),
        target_strategy_id: spec.target_strategy_id.clone(),
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
        .map_err(|error| AppError::from_io("创建多市场盘口实验目录", &error))?;
    write_spec_artifact(&spec_path, &canonical_spec, "多市场盘口实验")?;
    verify_multi_market_depth_experiment_candidates(
        &result,
        &input_data_fingerprint,
        &canonical_run_spec,
        &root,
    )?;
    let persisted = persist_multi_market_depth_experiment_result(
        result,
        &canonical_run_spec,
        &root,
        &artifact_path,
        &spec_path,
    )?;
    if let Err(error) = write_text(&artifact_path, &persisted.to_json()?) {
        if let Ok(existing) = std::fs::read_to_string(&artifact_path) {
            return existing_result(
                &existing,
                &persisted.spec_fingerprint,
                persisted
                    .canonical_run_spec_json
                    .as_deref()
                    .unwrap_or_default(),
                persisted
                    .canonical_run_spec_fingerprint
                    .as_deref()
                    .unwrap_or_default(),
                persisted
                    .input_data_fingerprint
                    .as_deref()
                    .unwrap_or_default(),
                &persisted.experiment_id,
                &canonical_run_spec,
                &root,
                &spec_path,
                &canonical_spec,
            )
            .map(Some);
        }
        return Err(error);
    }
    verify_persisted_multi_market_depth_experiment_result(&persisted)?;
    Ok(Some(persisted))
}

fn build_multi_market_depth_experiment_identity(
    spec: &RunMultiMarketDepthExperimentSpec,
    identity: Option<&StrategyArtifactIdentity>,
    context: &RunContext,
) -> Result<(CanonicalRunSpec, String), AppError> {
    let mut data_identities = Vec::with_capacity(spec.base.markets.len());
    let mut execution_models = BTreeMap::<String, OrderBookExecutionDescriptor>::new();
    let mut clock_start = u64::MAX;
    let mut clock_end = 0_u64;
    for market in &spec.base.markets {
        let payload = super::artifacts::read_text(&market.depth_path)?;
        let frame = DepthFrame::from_json(&payload).map_err(|error| {
            invalid(format!(
                "市场 {} DepthFrame 无效，不能建立参数实验身份: {error}",
                market.instrument
            ))
        })?;
        if frame.instrument.to_string() != market.instrument {
            return Err(invalid(format!(
                "市场 {} 的盘口帧标的是 {}，不能建立参数实验身份",
                market.instrument, frame.instrument
            )));
        }
        let (Some(first), Some(last)) = (frame.snapshots.first(), frame.snapshots.last()) else {
            return Err(AppError::new(
                AppErrorCategory::FidelityInsufficient,
                format!("市场 {} 至少需要一份盘口快照", market.instrument),
            ));
        };
        let data_tier = market.data_tier.as_deref().unwrap_or("l2");
        let quality = market
            .dataset_manifest_path
            .as_deref()
            .map(|path| {
                super::run_depth_backtest::quality::verified_depth_dataset_quality(
                    Path::new(path),
                    Path::new(&market.depth_path),
                    &frame,
                    data_tier,
                )
            })
            .transpose()?;
        let quality_fingerprint = quality
            .as_ref()
            .and_then(|value| value.get("manifest_fingerprint"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        data_identities.push(MultiMarketDepthInputIdentityV1 {
            instrument: market.instrument.clone(),
            data_tier: data_tier.to_owned(),
            source: frame.source.clone(),
            input_hash: frame.input_hash(),
            start_ts_ms: first.ts,
            end_ts_ms: last.ts,
            quality_fingerprint,
        });
        clock_start = clock_start.min(first.ts);
        clock_end = clock_end.max(last.ts);
        execution_models.insert(
            market.instrument.clone(),
            market.execution_model()?.audit_descriptor(),
        );
    }
    let input_set = multi_market_depth_input_set_identity_v1(&spec.base, &data_identities)?;
    let runtime_version = format!("qx-app-{}", env!("CARGO_PKG_VERSION"));
    let mut canonical = build_multi_market_depth_canonical_run_spec(
        &spec.base,
        &identity.into_iter().cloned().collect::<Vec<_>>(),
        &data_identities,
        &execution_models,
        context.code_commit(),
        &runtime_version,
        clock_start,
        clock_end,
    )?;
    let portable_spec = quote_unsafe_integer_tokens(&spec.to_json()?);
    let mut behavior: serde_json::Value = serde_json::from_str(&portable_spec)
        .map_err(|error| invalid(format!("多市场盘口实验行为 JSON 无效: {error}")))?;
    let fields = behavior
        .as_object_mut()
        .ok_or_else(|| invariant("多市场盘口实验行为不是 JSON 对象"))?;
    fields.remove("experiment_id");
    let base = fields
        .get_mut("base")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| invariant("多市场盘口实验行为缺少 base"))?;
    base.remove("run_id");
    base.remove("output_dir");
    let markets = base
        .get_mut("markets")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| invariant("多市场盘口实验行为缺少 markets"))?;
    for market in markets {
        let market = market
            .as_object_mut()
            .ok_or_else(|| invariant("多市场盘口实验行为项不是 JSON 对象"))?;
        market.remove("depth_path");
        market.remove("dataset_manifest_path");
    }
    normalize_experiment_identity_values(&mut behavior, "behavior")?;
    canonical.run_type = "multi_market_depth_experiment".into();
    canonical.behavior = behavior;
    canonical.extensions.insert(
        "input_data_fingerprint".into(),
        input_set.data_fingerprint.clone().into(),
    );
    for (instrument, input) in spec.base.markets.iter().zip(&data_identities) {
        canonical.extensions.insert(
            format!("depth_source:{}", instrument.instrument),
            input.source.clone().into(),
        );
    }
    for (name, fingerprint) in input_set.input_components {
        canonical
            .extensions
            .insert(format!("input_component:{name}"), fingerprint.into());
    }
    canonical
        .canonical_json()
        .map_err(|error| invariant(format!("多市场盘口实验 CanonicalRunSpec 无效: {error}")))?;
    Ok((canonical, input_set.data_fingerprint))
}

fn build_multi_market_depth_experiment_manifest(
    experiment_id: &str,
    canonical: &CanonicalRunSpec,
    result_hash: &str,
) -> Result<qx_core::RunManifest, AppError> {
    let runtime_version = canonical
        .execution_model
        .get("runtime_version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invariant("多市场盘口实验执行模型缺少 runtime_version"))?;
    let global_seed = canonical
        .random_seed
        .parse::<u64>()
        .map_err(|error| invalid(format!("多市场盘口实验随机种子无效: {error}")))?;
    let data_fingerprint = canonical
        .extensions
        .get("input_data_fingerprint")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invariant("多市场盘口实验 CanonicalRunSpec 缺少输入集合指纹"))?;
    let input_components = canonical
        .extensions
        .iter()
        .filter_map(|(name, value)| {
            name.strip_prefix("input_component:").and_then(|key| {
                value
                    .as_str()
                    .map(|fingerprint| (key.to_owned(), fingerprint.to_owned()))
            })
        })
        .collect::<BTreeMap<_, _>>();
    if input_components.len() != canonical.datasets.len()
        || canonical.datasets.iter().any(|dataset| {
            !input_components.contains_key(&format!("depthframe:{}", dataset.instrument))
        })
    {
        return Err(invariant("多市场盘口实验输入组件与数据集数量不一致"));
    }
    let model_bytes = serde_json::to_vec(&canonical.execution_model)
        .map_err(|error| invariant(format!("多市场盘口实验执行模型编码失败: {error}")))?;
    let canonical_json = canonical
        .canonical_json()
        .map_err(|error| invariant(format!("多市场盘口实验 CanonicalRunSpec 编码失败: {error}")))?;
    let canonical_fingerprint = canonical
        .fingerprint()
        .map_err(|error| invariant(format!("多市场盘口实验 CanonicalRunSpec 指纹失败: {error}")))?;
    Ok(qx_core::RunManifest {
        run_id: experiment_id.to_owned(),
        code_commit: canonical.code_revision.clone(),
        config_hash: canonical_fingerprint.clone(),
        data_fingerprint: data_fingerprint.to_owned(),
        input_components,
        clock_start: canonical.clock.start_ts_ms,
        clock_end: canonical.clock.end_ts_ms,
        global_seed,
        determinism_mode: true,
        result_hash: result_hash.to_owned(),
        strategy_version: "multi-market-depth-experiment-strategy-set-v1".into(),
        instrument_spec_version: "multi-market-depth-experiment-v1".into(),
        model_fingerprint: qx_strategy::sha256_hex(&model_bytes),
        input_event_hash: data_fingerprint.to_owned(),
        output_event_hash: result_hash.to_owned(),
        runtime_version: runtime_version.to_owned(),
        canonical_run_spec_json: Some(canonical_json),
        canonical_run_spec_fingerprint: Some(canonical_fingerprint),
    })
}

fn persist_multi_market_depth_experiment_result(
    mut result: RunMultiMarketDepthExperimentResult,
    canonical: &CanonicalRunSpec,
    root: &Path,
    report_index_path: &str,
    spec_artifact_path: &str,
) -> Result<RunMultiMarketDepthExperimentResult, AppError> {
    let facts_payload = result.to_json()?;
    let result_hash = experiment_result_hash_from_json(&facts_payload)
        .map_err(|error| invariant(format!("多市场盘口实验结果指纹失败: {error}")))?;
    result.result_hash = Some(result_hash.clone());
    let manifest = build_multi_market_depth_experiment_manifest(
        &result.experiment_id,
        canonical,
        &result_hash,
    )?;
    let manifest_path = crate::artifact_store::persist_run_manifest(root, &manifest)?;
    let report_path = root.join(format!(
        "{}-{result_hash}-input-v1.multi-market-depth-experiment.json",
        result.experiment_id
    ));
    let record_path = root.join(format!(
        "{}-{result_hash}-input-v1.record.json",
        result.experiment_id
    ));
    let spec_bytes = std::fs::read(spec_artifact_path)
        .map_err(|error| AppError::from_io("读取多市场盘口实验输入规格 sidecar", &error))?;
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
        .map_err(|error| invariant(format!("多市场盘口实验 RunManifest 编码失败: {error}")))?;
    let report_bytes = report_payload.as_bytes();
    let manifest_bytes = manifest_payload.as_bytes();
    let record = RunRecordV2 {
        schema_version: qx_spec::RUN_RECORD_V2_SCHEMA_VERSION,
        run_id: result.experiment_id.clone(),
        status: RunStatus::Completed,
        input_digest: result
            .input_data_fingerprint
            .clone()
            .ok_or_else(|| invariant("多市场盘口实验输入指纹缺失"))?,
        code_identity: format!("{}:{}", manifest.code_commit, manifest.runtime_version),
        config_fingerprint: result
            .canonical_run_spec_fingerprint
            .clone()
            .ok_or_else(|| invariant("多市场盘口实验 CanonicalRunSpec 指纹缺失"))?,
        started_at_ms: manifest.clock_start,
        finished_at_ms: Some(manifest.clock_end),
        artifact_refs: vec![
            ArtifactRefV2 {
                name: "experiment_report".into(),
                artifact_id: qx_strategy::sha256_hex(report_bytes),
                kind: "run-multi-market-depth-experiment-result-json".into(),
                schema_version: RUN_MULTI_MARKET_DEPTH_EXPERIMENT_SCHEMA_VERSION,
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
        .map_err(|error| invariant(format!("多市场盘口实验 RunRecord V2 编码失败: {error}")))?;
    publish_multi_market_depth_experiment_artifact(&record_path, &record_payload, "RunRecord V2")?;
    publish_multi_market_depth_experiment_artifact(&report_path, &report_payload, "实验报告")?;
    RunMultiMarketDepthExperimentResult::from_json(&report_payload)
}

fn publish_multi_market_depth_experiment_artifact(
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
            AppError::from_io(&format!("发布多市场盘口实验 {label}"), &error)
        }
    })
}

fn verify_persisted_multi_market_depth_experiment_result(
    result: &RunMultiMarketDepthExperimentResult,
) -> Result<(), AppError> {
    let summary = serde_json::json!({
        "schema_version": result.schema_version,
        "experiment_id": result.experiment_id,
        "target_strategy_id": result.target_strategy_id,
        "spec_fingerprint": result.spec_fingerprint,
        "total_candidates": result.total_candidates,
        "completed_candidates": result.completed_candidates,
        "succeeded_candidates": result.succeeded_candidates,
        "failed_candidates": result.failed_candidates,
        "candidates": result.candidates,
        "ranking": result.ranking,
        "artifact_path": result.artifact_path,
        "canonical_run_spec_json": result.canonical_run_spec_json,
        "canonical_run_spec_fingerprint": result.canonical_run_spec_fingerprint,
        "input_data_fingerprint": result.input_data_fingerprint,
        "result_hash": result.result_hash,
        "run_manifest": result.run_manifest,
        "run_record": result.run_record,
        "artifact_locations": result.artifact_locations,
    });
    crate::artifact_store::verify_declared_run_manifest(&summary).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("多市场盘口实验 RunManifest/RunRecord 读回失败: {error}"),
        )
    })
}

fn target_market(
    spec: &RunMultiMarketDepthExperimentSpec,
) -> Result<&crate::MultiMarketDepthBacktestMarket, AppError> {
    spec.base
        .markets
        .iter()
        .find(|market| market.strategy.strategy_id == spec.target_strategy_id)
        .ok_or_else(|| invariant("validated depth target disappeared"))
}

fn validate_injected_binding(
    spec: &MultiMarketDepthBacktestSpec,
    identity: &StrategyArtifactIdentity,
    strategy: Box<dyn Strategy>,
) -> Result<(StrategyArtifactIdentity, Box<dyn Strategy>), AppError> {
    let market = spec
        .markets
        .iter()
        .find(|market| market.strategy.strategy_id == identity.strategy_id)
        .ok_or_else(|| invalid("外部策略身份不属于候选盘口策略槽"))?;
    identity.validate_for_slot(&market.strategy.strategy_id, &market.strategy)?;
    Ok((identity.clone(), strategy))
}

fn validate_identity(
    spec: &RunMultiMarketDepthExperimentSpec,
    identity: &StrategyArtifactIdentity,
) -> Result<(), AppError> {
    if identity.strategy_id != spec.target_strategy_id {
        return Err(invalid(
            "StrategyArtifactIdentity.strategy_id 必须等于 target_strategy_id",
        ));
    }
    identity.validate_for_slot(&spec.target_strategy_id, &target_market(spec)?.strategy)
}

fn verify_multi_market_depth_experiment_candidates(
    result: &RunMultiMarketDepthExperimentResult,
    expected_data_fingerprint: &str,
    canonical: &CanonicalRunSpec,
    artifact_root: &Path,
) -> Result<(), AppError> {
    let reader = crate::artifact_store::FileArtifactReader;
    let root = artifact_root
        .canonicalize()
        .map_err(|error| AppError::from_io("解析多市场盘口实验产物根目录", &error))?;
    let mut verified_run_ids = BTreeSet::new();
    for candidate in &result.candidates {
        let Some(outcome) = candidate.outcome.as_ref() else {
            continue;
        };
        if !verified_run_ids.insert(outcome.run_id.as_str()) {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场盘口实验重复引用候选运行 {}", outcome.run_id),
            ));
        }
        if outcome.data_fingerprint != expected_data_fingerprint
            || outcome.markets_total as usize != canonical.datasets.len()
        {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!(
                    "多市场盘口实验候选 {} 输入身份与父实验不一致",
                    outcome.run_id
                ),
            ));
        }
        let artifact_path = rooted_candidate_path(&root, &outcome.artifact_path)?;
        let payload = std::fs::read_to_string(&artifact_path)
            .map_err(|error| AppError::from_io("读取多市场盘口实验候选产物", &error))?;
        let artifact: serde_json::Value = serde_json::from_str(&payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("多市场盘口实验候选 {} JSON 无效: {error}", outcome.run_id),
            )
        })?;
        let stored_outcome: crate::MultiMarketDepthBacktestOutcome = serde_json::from_value(
            artifact
                .get("outcome")
                .cloned()
                .ok_or_else(|| invariant("多市场深度候选产物缺少 outcome"))?,
        )
        .map_err(|error| invariant(format!("多市场深度候选 outcome 无效: {error}")))?;
        if &stored_outcome != outcome {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!(
                    "多市场盘口实验候选 {} 摘要与持久化产物不一致",
                    outcome.run_id
                ),
            ));
        }
        let manifest_path = artifact
            .get("run_manifest_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invariant("多市场盘口候选产物缺少 RunManifest 路径"))?;
        let manifest_path = rooted_candidate_path(&root, manifest_path)?;
        let manifest_payload = std::fs::read_to_string(&manifest_path)
            .map_err(|error| AppError::from_io("读取多市场盘口候选 RunManifest", &error))?;
        let manifest = qx_core::RunManifest::from_json(&manifest_payload)
            .map_err(|error| invariant(format!("多市场盘口候选 RunManifest 无效: {error}")))?;
        crate::artifact_store::verify_canonical_run_spec_for_manifest(&manifest, &artifact)
            .map_err(|error| {
                AppError::new(
                    AppErrorCategory::Conflict,
                    format!(
                        "多市场盘口实验候选 {} 身份核验失败: {error}",
                        outcome.run_id
                    ),
                )
            })?;
        let candidate_canonical = manifest
            .canonical_run_spec_json
            .as_deref()
            .ok_or_else(|| invariant("多市场盘口候选 RunManifest 缺少 CanonicalRunSpec"))
            .and_then(|json| {
                CanonicalRunSpec::from_json(json).map_err(|error| {
                    invariant(format!("多市场盘口候选 CanonicalRunSpec 无效: {error}"))
                })
            })?;
        if candidate_canonical.run_type != "multi_market_depth_backtest"
            || candidate_canonical.datasets != canonical.datasets
            || candidate_canonical.market_rules != canonical.market_rules
            || candidate_canonical.clock != canonical.clock
            || candidate_canonical.execution_model != canonical.execution_model
        {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!(
                    "多市场盘口实验候选 {} 未绑定父实验的数据/规则/执行环境",
                    outcome.run_id
                ),
            ));
        }
        if artifact
            .get("replay_verified")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场盘口实验候选 {} 未声明已验证重放", outcome.run_id),
            ));
        }
        let event_trace_path = artifact
            .get("event_trace_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invariant("多市场盘口候选产物缺少 EventLog 轨迹"))?;
        let event_trace_path = rooted_candidate_path(&root, event_trace_path)?;
        let event_count = artifact
            .get("event_count")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| invariant("多市场盘口候选产物缺少 event_count"))?;
        let ledger_entries = artifact
            .get("ledger_entries")
            .ok_or_else(|| invariant("多市场盘口候选产物缺少重放账簿"))?;
        let candidate_spec: MultiMarketDepthBacktestSpec = serde_json::from_value(
            artifact
                .get("spec")
                .cloned()
                .ok_or_else(|| invariant("多市场盘口候选产物缺少 spec"))?,
        )
        .map_err(|error| invariant(format!("多市场盘口候选 spec 无效: {error}")))?;
        let strategy_markets = candidate_spec
            .markets
            .iter()
            .map(|market| {
                (
                    market.strategy.strategy_id.clone(),
                    market.instrument.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let replay = crate::read_multi_market_event_report_v1_with_reader(
            event_trace_path.to_string_lossy().as_ref(),
            event_count,
            &manifest.output_event_hash,
            ledger_entries,
            &strategy_markets,
            None,
            &reader,
        )
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::Conflict,
                format!(
                    "多市场盘口实验候选 {} EventLog 重放失败: {error}",
                    outcome.run_id
                ),
            )
        })?;
        if replay.total != event_count {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场盘口实验候选 {} EventLog 摘要不一致", outcome.run_id),
            ));
        }
    }
    Ok(())
}

fn rooted_candidate_path(root: &Path, reference: &str) -> Result<PathBuf, AppError> {
    let path = Path::new(reference)
        .canonicalize()
        .map_err(|error| AppError::from_io("解析多市场盘口候选产物路径", &error))?;
    if !path.starts_with(root) || !path.is_file() {
        return Err(AppError::new(
            AppErrorCategory::StorageFailure,
            format!("多市场盘口候选产物超出实验根目录或不是文件: {reference}"),
        ));
    }
    Ok(path)
}

fn existing_result(
    payload: &str,
    fingerprint: &str,
    canonical_json: &str,
    canonical_fingerprint: &str,
    input_data_fingerprint: &str,
    experiment_id: &str,
    canonical: &CanonicalRunSpec,
    root: &Path,
    spec_artifact_path: &str,
    canonical_spec_artifact: &str,
) -> Result<RunMultiMarketDepthExperimentResult, AppError> {
    let mut existing = RunMultiMarketDepthExperimentResult::from_json(payload)?;
    let canonical_pair = match (
        existing.canonical_run_spec_json.as_deref(),
        existing.canonical_run_spec_fingerprint.as_deref(),
    ) {
        (Some(json), Some(hash)) => Some((json, hash)),
        (None, None) => None,
        _ => {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                "既有多市场盘口实验 CanonicalRunSpec JSON/fingerprint 不完整",
            ));
        }
    };
    let same_identity = canonical_pair
        .map_or(existing.spec_fingerprint == fingerprint, |(_, hash)| {
            hash == canonical_fingerprint
        });
    if !same_identity || existing.experiment_id != experiment_id {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场盘口实验身份已存在且规格不同",
        ));
    }
    if canonical_pair.is_some_and(|(json, _)| json != canonical_json)
        || existing
            .input_data_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| fingerprint != input_data_fingerprint)
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "既有多市场盘口实验已绑定不同数据或 CanonicalRunSpec",
        ));
    }
    if existing.to_json()? != payload {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "既有多市场盘口实验报告与规范序列化不一致",
        ));
    }
    verify_multi_market_depth_experiment_candidates(
        &existing,
        input_data_fingerprint,
        canonical,
        root,
    )?;
    let envelope_fields = [
        existing.result_hash.is_some(),
        existing.run_manifest.is_some(),
        existing.run_record.is_some(),
        existing.artifact_locations.is_some(),
    ];
    if envelope_fields.iter().all(|present| *present) {
        if existing.canonical_run_spec_json.as_deref() != Some(canonical_json)
            || existing.canonical_run_spec_fingerprint.as_deref() != Some(canonical_fingerprint)
            || existing.input_data_fingerprint.as_deref() != Some(input_data_fingerprint)
        {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                "已发布的多市场盘口实验缺少或错配标准身份",
            ));
        }
        verify_persisted_multi_market_depth_experiment_result(&existing)?;
        let has_input_spec = existing
            .artifact_locations
            .as_ref()
            .is_some_and(|locations| locations.contains_key("experiment_input"));
        if has_input_spec {
            super::experiment_spec_artifact::verify_spec_artifact(
                spec_artifact_path,
                canonical_spec_artifact,
                "多市场盘口实验",
            )?;
            return Ok(existing);
        }
    }
    if envelope_fields.iter().any(|present| *present)
        && !envelope_fields.iter().all(|present| *present)
    {
        return Err(AppError::new(
            AppErrorCategory::StorageFailure,
            "既有多市场盘口实验的 result_hash/manifest/record/artifact_locations 必须完整或全部缺省",
        ));
    }
    existing.canonical_run_spec_json = Some(canonical_json.to_owned());
    existing.canonical_run_spec_fingerprint = Some(canonical_fingerprint.to_owned());
    existing.input_data_fingerprint = Some(input_data_fingerprint.to_owned());
    super::experiment_spec_artifact::write_spec_artifact(
        spec_artifact_path,
        canonical_spec_artifact,
        "多市场盘口实验",
    )?;
    let report_index_path = existing.artifact_path.clone();
    let upgraded = persist_multi_market_depth_experiment_result(
        existing,
        canonical,
        root,
        &report_index_path,
        spec_artifact_path,
    )?;
    write_text(&upgraded.artifact_path, &upgraded.to_json()?)?;
    verify_persisted_multi_market_depth_experiment_result(&upgraded)?;
    Ok(upgraded)
}

fn failed_candidate(
    ordinal: usize,
    kind: BuiltinStrategyKind,
    include_kind: bool,
    parameters: BTreeMap<String, i128>,
    error: AppError,
) -> MultiMarketDepthExperimentCandidate {
    MultiMarketDepthExperimentCandidate {
        ordinal,
        strategy_kind: include_kind.then(|| kind.name().to_string()),
        parameters,
        outcome: None,
        error: Some(error.to_json()),
    }
}

impl RunMultiMarketDepthExperimentResult {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload)
            .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error.to_string()))
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|error| invariant(format!("多市场盘口实验结果序列化失败: {error}")))
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
fn invariant(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}
