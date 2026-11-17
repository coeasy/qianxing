//! Path-independent multi-market spec, strategy, and dataset identity functions.

use super::{invalid, AppError, MultiMarketBacktestSpec, MultiMarketDepthBacktestSpec};
use crate::StrategyArtifactIdentity;
use qx_core::Fnv1a;
use std::collections::{BTreeMap, BTreeSet};

/// Calculate the stable identity of a multi-market run without machine-local file paths.
///
/// Strategy identities are sorted by `strategy_id` before hashing, so equivalent strategy sets
/// produce the same fingerprint regardless of artifact ordering.
pub fn multi_market_spec_fingerprint_v1(
    spec: &MultiMarketBacktestSpec,
    strategy_identities: &[StrategyArtifactIdentity],
) -> Result<String, AppError> {
    let mut identity_spec = spec.clone();
    identity_spec.output_dir.clear();
    for market in &mut identity_spec.markets {
        market.bars_path.clear();
        market.dataset_manifest_path = None;
    }
    let spec_json = identity_spec.to_json()?;
    let mut base_hasher = Fnv1a::new();
    base_hasher.write_text(&spec_json);
    Ok(multi_market_strategy_set_fingerprint(
        base_hasher.finish(),
        strategy_identities,
    ))
}

pub(crate) fn multi_market_strategy_set_fingerprint(
    base_fingerprint: u64,
    strategy_identities: &[StrategyArtifactIdentity],
) -> String {
    if strategy_identities.is_empty() {
        return format!("{base_fingerprint:016x}");
    }

    let mut identities = strategy_identities.iter().collect::<Vec<_>>();
    identities.sort_by(|left, right| left.strategy_id.cmp(&right.strategy_id));
    let mut hasher = Fnv1a::new();
    hasher.write_u64(base_fingerprint);
    for identity in identities {
        hasher.write_text(&identity.strategy_id);
        hasher.write_text(&identity.strategy_version);
        hasher.write_text(&identity.source_sha256);
    }
    format!("{:016x}", hasher.finish())
}

/// Stable data identity facts for one configured market in a shared-account run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiMarketInputIdentityV1 {
    pub instrument: String,
    pub bar_fingerprint: String,
    pub quality_fingerprint: Option<String>,
}

/// Canonical aggregate identity written to a multi-market RunManifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiMarketInputSetIdentityV1 {
    pub data_fingerprint: String,
    pub input_components: BTreeMap<String, String>,
}

/// Stable identity facts for one configured market in a shared-account depth run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiMarketDepthInputIdentityV1 {
    pub instrument: String,
    pub data_tier: String,
    pub source: String,
    pub input_hash: u64,
    pub start_ts_ms: u64,
    pub end_ts_ms: u64,
    pub quality_fingerprint: Option<String>,
}

/// Build the shared-account data identity in configured market order.
///
/// Both the run producer and report verifier use this function so ordering, quality inclusion,
/// and manifest component names cannot drift between write and read paths.
pub fn multi_market_input_set_identity_v1(
    spec: &MultiMarketBacktestSpec,
    inputs: &[MultiMarketInputIdentityV1],
) -> Result<MultiMarketInputSetIdentityV1, AppError> {
    if spec.markets.is_empty() || inputs.len() != spec.markets.len() {
        return Err(invalid("多市场输入身份集合与 spec 数量不一致"));
    }

    let mut input_by_instrument = BTreeMap::new();
    for input in inputs {
        if input.bar_fingerprint.trim().is_empty()
            || input
                .quality_fingerprint
                .as_ref()
                .is_some_and(|fingerprint| fingerprint.trim().is_empty())
            || input_by_instrument
                .insert(input.instrument.as_str(), input)
                .is_some()
        {
            return Err(invalid("多市场输入身份存在空指纹或重复标的"));
        }
    }

    let mut hasher = Fnv1a::new();
    let mut input_components = BTreeMap::new();
    for market in &spec.markets {
        let input = input_by_instrument
            .remove(market.instrument.as_str())
            .ok_or_else(|| invalid(format!("多市场输入身份缺少 {}", market.instrument)))?;
        if market.dataset_manifest_path.is_some() != input.quality_fingerprint.is_some() {
            return Err(invalid(format!(
                "{} 数据质量指纹与 spec 声明不一致",
                market.instrument
            )));
        }
        hasher.write_text(&input.instrument);
        hasher.write_text(&input.bar_fingerprint);
        input_components.insert(
            format!("barframe:{}", input.instrument),
            input.bar_fingerprint.clone(),
        );
        if let Some(quality_fingerprint) = &input.quality_fingerprint {
            hasher.write_text(quality_fingerprint);
            input_components.insert(
                format!("dataset-quality:{}", input.instrument),
                quality_fingerprint.clone(),
            );
        }
    }
    if !input_by_instrument.is_empty() {
        return Err(invalid("多市场输入身份含有 spec 之外的标的"));
    }

    Ok(MultiMarketInputSetIdentityV1 {
        data_fingerprint: format!("{:016x}", hasher.finish()),
        input_components,
    })
}

/// Build the shared-account depth input identity in configured market order.
///
/// The producer and report verifier share this function so the per-market DepthFrame identity,
/// optional DatasetManifestV2 identity, and RunManifest components cannot drift apart.
pub fn multi_market_depth_input_set_identity_v1(
    spec: &MultiMarketDepthBacktestSpec,
    inputs: &[MultiMarketDepthInputIdentityV1],
) -> Result<MultiMarketInputSetIdentityV1, AppError> {
    if spec.markets.is_empty() || inputs.len() != spec.markets.len() {
        return Err(invalid("多市场深度输入身份集合与 spec 数量不一致"));
    }

    let mut input_by_instrument = BTreeMap::new();
    for input in inputs {
        if input.instrument.trim().is_empty()
            || input.source.trim().is_empty()
            || !matches!(input.data_tier.as_str(), "l1" | "l2")
            || input.start_ts_ms > input.end_ts_ms
            || input
                .quality_fingerprint
                .as_ref()
                .is_some_and(|fingerprint| fingerprint.trim().is_empty())
            || input_by_instrument
                .insert(input.instrument.as_str(), input)
                .is_some()
        {
            return Err(invalid("多市场深度输入身份存在空值或重复标的"));
        }
    }

    let mut hasher = Fnv1a::new();
    let mut input_components = BTreeMap::new();
    for market in &spec.markets {
        let input = input_by_instrument
            .remove(market.instrument.as_str())
            .ok_or_else(|| invalid(format!("多市场深度输入身份缺少 {}", market.instrument)))?;
        let configured_tier = market.data_tier.as_deref().unwrap_or("l2");
        if configured_tier != input.data_tier
            || market.dataset_manifest_path.is_some() != input.quality_fingerprint.is_some()
        {
            return Err(invalid(format!(
                "{} 深度数据档位或质量声明与输入身份不一致",
                market.instrument
            )));
        }

        let fingerprint = format!(
            "depth:{}:{}:{:016x}",
            input.data_tier, input.source, input.input_hash
        );
        hasher.write_text(&input.instrument);
        hasher.write_text(&input.data_tier);
        hasher.write_text(&input.source);
        hasher.write_u64(input.input_hash);
        input_components.insert(format!("depthframe:{}", input.instrument), fingerprint);
        if let Some(quality_fingerprint) = &input.quality_fingerprint {
            hasher.write_text(quality_fingerprint);
            input_components.insert(
                format!("dataset-quality:{}", input.instrument),
                quality_fingerprint.clone(),
            );
        }
    }
    if !input_by_instrument.is_empty() {
        return Err(invalid("多市场深度输入身份含有 spec 之外的标的"));
    }

    Ok(MultiMarketInputSetIdentityV1 {
        data_fingerprint: format!("{:016x}", hasher.finish()),
        input_components,
    })
}

/// Calculate a path-independent multi-market depth spec and strategy identity.
pub fn multi_market_depth_spec_fingerprint_v1(
    spec: &MultiMarketDepthBacktestSpec,
    strategy_identities: &[StrategyArtifactIdentity],
) -> Result<String, AppError> {
    let mut identity_spec = spec.clone();
    identity_spec.output_dir.clear();
    for market in &mut identity_spec.markets {
        market.depth_path.clear();
        market.dataset_manifest_path = None;
    }
    let spec_json = identity_spec.to_json()?;
    let mut hasher = Fnv1a::new();
    hasher.write_text(&spec_json);

    let strategy_ids = spec
        .markets
        .iter()
        .map(|market| market.strategy.strategy_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut seen_strategy_ids = BTreeSet::new();
    for identity in strategy_identities {
        if !strategy_ids.contains(identity.strategy_id.as_str())
            || !seen_strategy_ids.insert(identity.strategy_id.as_str())
        {
            return Err(invalid("多市场深度自定义策略身份与 spec 不一致或重复"));
        }
    }

    let mut ordered_identities = strategy_identities.iter().collect::<Vec<_>>();
    ordered_identities.sort_by(|left, right| left.strategy_id.cmp(&right.strategy_id));
    for identity in ordered_identities {
        hasher.write_text(&identity.strategy_id);
        hasher.write_text(&identity.strategy_version);
        hasher.write_text(&identity.source_sha256);
    }
    Ok(format!("{:016x}", hasher.finish()))
}
