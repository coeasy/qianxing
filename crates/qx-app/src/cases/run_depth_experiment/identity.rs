use super::super::run_experiment::{
    normalize_experiment_identity_values, quote_unsafe_integer_tokens,
};
use super::{invalid, invariant, RunDepthExperimentSpec};
use crate::{AppError, RunContext, StrategyArtifactIdentity};
use qx_spec::{
    CanonicalDatasetIdentity, CanonicalRunClock, CanonicalRunSpec, CanonicalStrategyIdentity,
};
use qx_xingban::DepthFrame;
use serde_json::Value;
use std::collections::BTreeMap;

pub(super) fn build_depth_experiment_canonical_run_spec(
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
                .unwrap_or_else(|| {
                    super::super::run_backtest::strategy_slot::strategy_version(&strategy)
                }),
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
