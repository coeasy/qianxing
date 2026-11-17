use super::super::run_experiment::{
    normalize_experiment_identity_values, quote_unsafe_integer_tokens,
};
use super::super::run_multi_market_depth_backtest::build_multi_market_depth_canonical_run_spec;
use super::{invalid, invariant, RunMultiMarketDepthExperimentSpec};
use crate::{
    multi_market_depth_input_set_identity_v1, AppError, AppErrorCategory,
    MultiMarketDepthInputIdentityV1, RunContext, StrategyArtifactIdentity,
};
use qx_spec::CanonicalRunSpec;
use qx_xingban::{DepthFrame, OrderBookExecutionDescriptor};
use std::collections::BTreeMap;
use std::path::Path;

pub(super) fn build_multi_market_depth_experiment_identity(
    spec: &RunMultiMarketDepthExperimentSpec,
    identity: Option<&StrategyArtifactIdentity>,
    context: &RunContext,
) -> Result<(CanonicalRunSpec, String), AppError> {
    let mut data_identities = Vec::with_capacity(spec.base.markets.len());
    let mut execution_models = BTreeMap::<String, OrderBookExecutionDescriptor>::new();
    let mut clock_start = u64::MAX;
    let mut clock_end = 0_u64;
    for market in &spec.base.markets {
        let payload = super::super::artifacts::read_text(&market.depth_path)?;
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
                super::super::run_depth_backtest::quality::verified_depth_dataset_quality(
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

pub(super) fn build_multi_market_depth_experiment_manifest(
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
