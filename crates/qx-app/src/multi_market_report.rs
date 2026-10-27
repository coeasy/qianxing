//! Application-owned verification for shared-account multi-market report facts.

use crate::report_read::{event_fill_identity, update_fill_sequence_digest};
use crate::{
    AppError, AppErrorCategory, MultiMarketBacktestMarket, MultiMarketBacktestSpec,
    MultiMarketDepthBacktestMarket, MultiMarketDepthBacktestSpec, StrategyArtifactIdentity,
};
use qx_core::{
    DecimalRounding, ExactDecimal, ExactValuationContext, Fnv1a, InstrumentId, Ledger, Price,
    TradingInstrumentSpec, ValuationContext,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_MULTI_MARKET_PNL_SAMPLES: usize = qx_xingban::MAX_INSTRUMENT_EQUITY_SAMPLES;

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

/// A verified point in an instrument's contribution to shared portfolio equity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedPnlContributionSample {
    pub equity_index: u64,
    pub ts: u64,
    pub pnl_contribution_raw: i128,
    pub pnl_contribution_exact: Option<ExactDecimal>,
}

/// Replayed and cross-checked contribution curves keyed by canonical instrument ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMultiMarketPnlContributions {
    pub total_equity_points: u64,
    pub curves: BTreeMap<String, Vec<VerifiedPnlContributionSample>>,
}

/// Independently verified identity and quality status for one report BarFrame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMultiMarketBarInputV1 {
    pub instrument: String,
    pub fingerprint: String,
    pub row_count: u64,
    pub source_end_ts: u64,
    pub final_close_raw: i128,
    pub quality_status: String,
    pub quality_fingerprint: Option<String>,
}

/// Independently verified identity and optional quality evidence for one report DepthFrame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMultiMarketDepthInputV1 {
    pub instrument: String,
    pub data_tier: String,
    pub source: String,
    pub fingerprint: String,
    pub input_hash: u64,
    pub snapshot_count: u64,
    pub start_ts_ms: u64,
    pub end_ts_ms: u64,
    pub quality_status: String,
    pub quality_fingerprint: Option<String>,
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

/// Facts recomputed from a shared-account multi-market Bar report's sidecars.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMultiMarketBarSidecarsV1 {
    pub equity_points: u64,
    pub first_equity_raw: i128,
    pub final_equity_raw: i128,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub fill_rows: u64,
}

struct VerifiedMultiMarketBarEquityV1 {
    equity_points: u64,
    first_equity_raw: i128,
    final_equity_raw: i128,
    return_bps: i32,
    max_drawdown_bps: u32,
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

/// Recompute portfolio metrics and bind the fills sidecar to replayed Filled facts.
///
/// The caller still owns rooted file reads and EventLog replay. This use case receives the
/// already loaded facts and verifies the outcome-facing financial summary before projection.
pub fn verify_multi_market_bar_sidecars_v1(
    equity: &Value,
    fills: &Value,
    events: &[Value],
    spec: &MultiMarketBacktestSpec,
    expected_equity_points: u64,
    expected_fill_count: u64,
    expected_final_equity_raw: i128,
    expected_return_bps: i32,
    expected_max_drawdown_bps: u32,
) -> Result<VerifiedMultiMarketBarSidecarsV1, AppError> {
    verify_multi_market_bar_sidecars_with_exact_v1(
        equity,
        None,
        fills,
        events,
        spec,
        expected_equity_points,
        expected_fill_count,
        expected_final_equity_raw,
        expected_return_bps,
        expected_max_drawdown_bps,
    )
}

/// Verify Bar sidecars when the run carries an exact initial principal/equity curve.
pub fn verify_multi_market_bar_sidecars_with_exact_v1(
    equity: &Value,
    exact_equity: Option<&Value>,
    fills: &Value,
    events: &[Value],
    spec: &MultiMarketBacktestSpec,
    expected_equity_points: u64,
    expected_fill_count: u64,
    expected_final_equity_raw: i128,
    expected_return_bps: i32,
    expected_max_drawdown_bps: u32,
) -> Result<VerifiedMultiMarketBarSidecarsV1, AppError> {
    let equity = verify_multi_market_bar_equity_metrics_v1(
        equity,
        exact_equity,
        spec,
        expected_equity_points,
        expected_final_equity_raw,
        expected_return_bps,
        expected_max_drawdown_bps,
    )?;
    let fill_rows = fills
        .as_array()
        .ok_or_else(|| invalid("多市场成交 sidecar 必须是数组"))?;
    let event_fills: Vec<Value> = events
        .iter()
        .filter_map(|event| event.pointer("/kind/Filled/fill").cloned())
        .collect();
    let fill_count =
        u64::try_from(fill_rows.len()).map_err(|_| invalid("多市场成交行数超出 u64 范围"))?;
    if fills != &Value::Array(event_fills) || fill_count != expected_fill_count {
        return Err(invalid("多市场成交 sidecar 与已重放 Filled 事件不一致"));
    }

    Ok(VerifiedMultiMarketBarSidecarsV1 {
        equity_points: equity.equity_points,
        first_equity_raw: equity.first_equity_raw,
        final_equity_raw: equity.final_equity_raw,
        return_bps: equity.return_bps,
        max_drawdown_bps: equity.max_drawdown_bps,
        fill_rows: fill_count,
    })
}

/// Reconcile a fills sidecar with the streamed, ordered Filled facts from a verified EventLog.
pub fn verify_multi_market_bar_sidecars_from_event_report_v1(
    equity: &Value,
    fills: &Value,
    expected_fill_count: u64,
    expected_fill_sequence_sha256: &str,
    spec: &MultiMarketBacktestSpec,
    expected_equity_points: u64,
    expected_final_equity_raw: i128,
    expected_return_bps: i32,
    expected_max_drawdown_bps: u32,
) -> Result<VerifiedMultiMarketBarSidecarsV1, AppError> {
    verify_multi_market_bar_sidecars_from_event_report_with_exact_v1(
        equity,
        None,
        fills,
        expected_fill_count,
        expected_fill_sequence_sha256,
        spec,
        expected_equity_points,
        expected_final_equity_raw,
        expected_return_bps,
        expected_max_drawdown_bps,
    )
}

/// Event-report variant of [`verify_multi_market_bar_sidecars_with_exact_v1`].
pub fn verify_multi_market_bar_sidecars_from_event_report_with_exact_v1(
    equity: &Value,
    exact_equity: Option<&Value>,
    fills: &Value,
    expected_fill_count: u64,
    expected_fill_sequence_sha256: &str,
    spec: &MultiMarketBacktestSpec,
    expected_equity_points: u64,
    expected_final_equity_raw: i128,
    expected_return_bps: i32,
    expected_max_drawdown_bps: u32,
) -> Result<VerifiedMultiMarketBarSidecarsV1, AppError> {
    let equity = verify_multi_market_bar_equity_metrics_v1(
        equity,
        exact_equity,
        spec,
        expected_equity_points,
        expected_final_equity_raw,
        expected_return_bps,
        expected_max_drawdown_bps,
    )?;
    let fill_count = verify_multi_market_event_fill_sidecar_v1(
        fills,
        expected_fill_count,
        expected_fill_sequence_sha256,
    )?;
    Ok(VerifiedMultiMarketBarSidecarsV1 {
        equity_points: equity.equity_points,
        first_equity_raw: equity.first_equity_raw,
        final_equity_raw: equity.final_equity_raw,
        return_bps: equity.return_bps,
        max_drawdown_bps: equity.max_drawdown_bps,
        fill_rows: fill_count,
    })
}

pub fn verify_multi_market_event_fill_sidecar_v1(
    fills: &Value,
    expected_fill_count: u64,
    expected_fill_sequence_sha256: &str,
) -> Result<u64, AppError> {
    let fill_rows = fills
        .as_array()
        .ok_or_else(|| invalid("多市场成交 sidecar 必须是数组"))?;
    let fill_count =
        u64::try_from(fill_rows.len()).map_err(|_| invalid("多市场成交行数超出 u64 范围"))?;
    let mut digest = qx_strategy::file_digest::Sha256Accumulator::new();
    digest.update(b"qx-event-fill-sequence-v1\0");
    for row in fill_rows {
        let fill: qx_core::Fill = serde_json::from_value(row.clone())
            .map_err(|error| invalid(format!("多市场成交 sidecar 行无效: {error}")))?;
        let normalized = serde_json::to_value(&fill)
            .map_err(|error| invalid(format!("多市场成交 sidecar 归一化失败: {error}")))?;
        if normalized != *row {
            return Err(invalid("多市场成交 sidecar 含事件事实之外的字段或非规范值"));
        }
        update_fill_sequence_digest(&mut digest, &event_fill_identity(&fill))?;
    }
    if fill_count != expected_fill_count || digest.finish_hex() != expected_fill_sequence_sha256 {
        return Err(invalid(
            "多市场成交 sidecar 与流式重放的 Filled 事件序列不一致",
        ));
    }
    Ok(fill_count)
}

/// Preserve the v1 wire contract for inline EventLog artifacts while reconciling depth fills.
/// Older reports compare the sidecar's complete JSON rows directly with embedded Filled events;
/// this helper keeps that compatibility check in the application layer.
pub fn verify_multi_market_legacy_event_fill_sidecar_v1(
    fills: &Value,
    events: &[Value],
    expected_fill_count: u64,
) -> Result<(), AppError> {
    let fill_rows = fills
        .as_array()
        .ok_or_else(|| invalid("多市场深度成交 sidecar 必须是数组"))?;
    let event_fills = events
        .iter()
        .filter_map(|event| event.pointer("/kind/Filled/fill").cloned())
        .collect::<Vec<_>>();
    let fill_count =
        u64::try_from(fill_rows.len()).map_err(|_| invalid("多市场深度成交行数溢出"))?;
    if fills != &Value::Array(event_fills) || fill_count != expected_fill_count {
        return Err(invalid("多市场深度成交 sidecar 与重放 Filled 事件不一致"));
    }
    Ok(())
}

fn verify_multi_market_bar_equity_metrics_v1(
    equity: &Value,
    exact_equity: Option<&Value>,
    spec: &MultiMarketBacktestSpec,
    expected_equity_points: u64,
    expected_final_equity_raw: i128,
    expected_return_bps: i32,
    expected_max_drawdown_bps: u32,
) -> Result<VerifiedMultiMarketBarEquityV1, AppError> {
    let equity_rows = equity
        .as_array()
        .ok_or_else(|| invalid("多市场权益曲线必须是数组"))?;
    let equity_points =
        u64::try_from(equity_rows.len()).map_err(|_| invalid("多市场权益点数超出 u64 范围"))?;
    if equity_points != expected_equity_points || equity_rows.is_empty() {
        return Err(invalid("多市场权益点数与结果摘要不一致"));
    }
    let first_equity_raw = equity_rows
        .first()
        .and_then(|row| row.get(1))
        .and_then(raw_i128)
        .ok_or_else(|| invalid("多市场权益首点无效"))?;
    let final_equity_raw = equity_rows
        .last()
        .and_then(|row| row.get(1))
        .and_then(raw_i128)
        .ok_or_else(|| invalid("多市场权益末点无效"))?;
    if first_equity_raw != spec.initial_cash_raw || final_equity_raw != expected_final_equity_raw {
        return Err(invalid("多市场权益曲线与初始资金/期末权益不一致"));
    }
    let initial_cash_exact = spec.initial_cash_exact_value()?;
    let (return_bps, max_drawdown_bps) = if let Some(exact_equity) = exact_equity {
        let rows = exact_equity
            .as_array()
            .ok_or_else(|| invalid("多市场 exact 权益 sidecar 必须是数组"))?;
        if rows.len() != equity_rows.len() || rows.is_empty() {
            return Err(invalid("多市场 exact 权益点数与 raw 权益曲线不一致"));
        }
        let mut exact_values = Vec::with_capacity(rows.len());
        for (index, (row, raw_row)) in rows.iter().zip(equity_rows).enumerate() {
            let object = row
                .as_object()
                .ok_or_else(|| invalid("多市场 exact 权益采样点必须是对象"))?;
            let equity_index = object
                .get("equity_index")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("多市场 exact 权益索引无效"))?;
            let ts = object
                .get("ts")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("多市场 exact 权益时间戳无效"))?;
            let amount = object
                .get("equity_exact")
                .and_then(Value::as_str)
                .map(ExactDecimal::parse)
                .transpose()
                .map_err(|error| invalid(format!("多市场 exact 权益金额无效: {error}")))?
                .ok_or_else(|| invalid("多市场 exact 权益金额缺失"))?;
            if object.len() != 3
                || equity_index != index as u64
                || raw_row.get(0).and_then(Value::as_u64) != Some(ts)
            {
                return Err(invalid("多市场 exact 权益 sidecar 索引/时间轴无效"));
            }
            let projected_raw = amount
                .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
                .and_then(|projected| projected.to_raw(9).ok());
            if raw_row.get(1).and_then(raw_i128) != projected_raw {
                return Err(invalid("多市场 exact 权益与 raw 权益投影不一致"));
            }
            exact_values.push(amount);
        }
        if exact_values.first().copied() != Some(initial_cash_exact) {
            return Err(invalid("多市场 exact 权益首点与初始资金不一致"));
        }
        let return_bps = crate::report_analysis::exact_equity_total_return_bps_v1(
            initial_cash_exact,
            *exact_values
                .last()
                .ok_or_else(|| invalid("多市场 exact 权益曲线为空"))?,
        )
        .map_err(invalid)?;
        let max_drawdown_bps =
            crate::report_analysis::exact_equity_max_drawdown_bps_v1(&exact_values)
                .map_err(invalid)?;
        (return_bps, max_drawdown_bps)
    } else {
        if spec.initial_cash_exact.is_some() {
            return Err(invalid(
                "MultiMarketBacktestSpec v2 复核必须提供 exact 权益 sidecar",
            ));
        }
        let return_bps = final_equity_raw
            .checked_sub(first_equity_raw)
            .and_then(|value| value.checked_mul(10_000))
            .and_then(|value| value.checked_div(first_equity_raw))
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| invalid("多市场收益基点无法复算"))?;
        (return_bps, equity_drawdown_bps(equity_rows)?)
    };
    if return_bps != expected_return_bps || max_drawdown_bps != expected_max_drawdown_bps {
        return Err(invalid("多市场聚合收益或最大回撤与权益曲线不一致"));
    }

    Ok(VerifiedMultiMarketBarEquityV1 {
        equity_points,
        first_equity_raw,
        final_equity_raw,
        return_bps,
        max_drawdown_bps,
    })
}

/// Require the persisted quality-manifest set to match the markets that declared manifests.
/// Unknown manifest records are rejected instead of being silently ignored.
pub fn validate_multi_market_quality_manifest_set_v1(
    spec: &MultiMarketBacktestSpec,
    quality_records: &Value,
) -> Result<(), AppError> {
    let records = quality_records
        .as_object()
        .ok_or_else(|| invalid("多市场数据质量记录必须是对象"))?;
    let expected = spec
        .markets
        .iter()
        .filter(|market| market.dataset_manifest_path.is_some())
        .map(|market| market.instrument.clone())
        .collect::<BTreeSet<_>>();
    let actual = records.keys().cloned().collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(invalid("多市场数据质量记录集合与 spec 声明不一致"));
    }
    Ok(())
}

/// Verify one market's BarFrame identity and optional DatasetManifestV2 quality evidence.
///
/// File authorization and bounded reads belong to the caller. This function receives the exact
/// loaded payloads, independently recomputes their content identities, and checks every
/// cross-artifact declaration before report data is projected.
pub fn verify_multi_market_bar_input_v1(
    configured: &MultiMarketBacktestMarket,
    recorded_market: &Value,
    frame_payload: &str,
    frame: &qx_datastruct::BarFrame,
    quality_record: Option<&Value>,
    quality_payload: Option<&str>,
) -> Result<VerifiedMultiMarketBarInputV1, AppError> {
    let instrument = recorded_market
        .get("instrument")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("多市场 market_data 缺少 instrument"))?;
    let recorded_fingerprint = recorded_market
        .get("content_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{instrument} BarFrame content_hash 无效")))?;
    let recorded_row_count = recorded_market
        .get("row_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(format!("{instrument} BarFrame row_count 无效")))?;
    let fingerprint = qx_data::fingerprint_bar_frame(frame)
        .map_err(|error| invalid(format!("{instrument} BarFrame 指纹失败: {error}")))?;
    let row_count =
        u64::try_from(frame.ts.len()).map_err(|_| invalid("BarFrame 行数超出 u64 范围"))?;
    let source_start_ts = frame
        .ts
        .first()
        .copied()
        .ok_or_else(|| invalid(format!("{instrument} BarFrame 不能为空")))?;
    let source_end_ts = frame
        .ts
        .last()
        .copied()
        .ok_or_else(|| invalid(format!("{instrument} BarFrame 不能为空")))?;
    if frame.instrument.to_string() != instrument
        || configured.instrument != instrument
        || recorded_fingerprint != fingerprint
        || recorded_row_count != row_count
        || recorded_market
            .get("start_ts_ms")
            .and_then(Value::as_u64)
            .is_some_and(|start| start != source_start_ts)
        || recorded_market
            .get("end_ts_ms")
            .and_then(Value::as_u64)
            .is_some_and(|end| end != source_end_ts)
    {
        return Err(invalid(format!(
            "{instrument} BarFrame 与运行输入身份不一致"
        )));
    }
    let final_close_raw = frame
        .close_raw
        .last()
        .copied()
        .ok_or_else(|| invalid(format!("{instrument} BarFrame 缺少末尾收盘价")))?;

    let (quality_status, quality_fingerprint) = match (quality_record, quality_payload) {
        (None, None) => {
            if configured.dataset_manifest_path.is_some() {
                return Err(invalid(format!(
                    "{instrument} spec 声明了质量清单但产物未记录"
                )));
            }
            ("not_provided".to_owned(), None)
        }
        (Some(recorded), Some(payload)) => {
            let manifest_path = recorded
                .get("manifest_path")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(format!("{instrument} 质量清单缺少路径")))?;
            if configured.dataset_manifest_path.as_deref() != Some(manifest_path) {
                return Err(invalid(format!("{instrument} 质量清单路径与 spec 不一致")));
            }
            let manifest = qx_data::DatasetManifestV2::from_json(payload)
                .map_err(|error| invalid(format!("数据质量清单无效: {error}")))?;
            let manifest_sha256 = qx_data::sha256_hex_bytes(payload.as_bytes());
            let frame_sha256 = qx_data::sha256_hex_bytes(frame_payload.as_bytes());
            let interval = manifest
                .quality_report
                .interval_ms
                .filter(|value| *value > 0)
                .ok_or_else(|| invalid("数据质量清单缺少正 interval_ms"))?;
            let missing = frame
                .ts
                .windows(2)
                .try_fold(0_u64, |total, pair| {
                    let distance = pair[1].checked_sub(pair[0])?;
                    total.checked_add(distance.div_ceil(interval).saturating_sub(1))
                })
                .ok_or_else(|| invalid("BarFrame 缺口计数溢出或时间戳倒退"))?;
            let first_ts = frame
                .ts
                .first()
                .copied()
                .ok_or_else(|| invalid(format!("{instrument} BarFrame 不能为空")))?;
            let manifest_fingerprint = manifest
                .fingerprint()
                .map_err(|error| invalid(format!("数据质量清单指纹失败: {error}")))?;
            let expected_quality_report = serde_json::to_value(&manifest.quality_report)
                .map_err(|error| invalid(error.to_string()))?;
            if manifest.instrument != instrument
                || manifest.tier != qx_data::DatasetTier::Bar
                || manifest.provider_version != qx_data::BARFRAME_DATASET_VERSION
                || manifest.dataset.fingerprint != fingerprint
                || manifest.content_hash != frame_sha256
                || manifest.quality_report.row_count != row_count
                || manifest.dataset.start_timestamp != first_ts
                || manifest.dataset.end_timestamp != source_end_ts
                || manifest.quality_report.missing_intervals != missing
                || manifest.quality_report.duplicate_rows != 0
                || manifest.quality_report.out_of_order_rows != 0
                || recorded.get("manifest_sha256").and_then(Value::as_str)
                    != Some(manifest_sha256.as_str())
                || recorded.get("manifest_fingerprint").and_then(Value::as_str)
                    != Some(manifest_fingerprint.as_str())
                || recorded.get("quality_report") != Some(&expected_quality_report)
            {
                return Err(invalid(format!("{instrument} 数据质量清单与输入不一致")));
            }
            ("verified".to_owned(), Some(manifest_fingerprint))
        }
        _ => return Err(invalid(format!("{instrument} 数据质量清单证据不完整"))),
    };

    Ok(VerifiedMultiMarketBarInputV1 {
        instrument: instrument.to_owned(),
        fingerprint,
        row_count,
        source_end_ts,
        final_close_raw,
        quality_status,
        quality_fingerprint,
    })
}

/// Verify one multi-market DepthFrame and its optional DatasetManifestV2 evidence.
///
/// The API adapter authorizes and opens the payloads. This use case owns depth identity, tier
/// shape, dataset quality, and continuity checks so report readers share the same rules.
pub fn verify_multi_market_depth_input_v1(
    configured: &MultiMarketDepthBacktestMarket,
    recorded_market: &Value,
    frame_payload: &str,
    frame: &qx_xingban::DepthFrame,
    quality_record: Option<&Value>,
    quality_payload: Option<&str>,
) -> Result<VerifiedMultiMarketDepthInputV1, AppError> {
    let instrument = recorded_market
        .get("instrument")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("多市场深度 market_data 缺少 instrument"))?;
    let data_tier = configured.data_tier.as_deref().unwrap_or("l2");
    let expected_hash = recorded_market
        .get("content_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{instrument} DepthFrame content_hash 无效")))?;
    let expected_source = recorded_market
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{instrument} DepthFrame source 无效")))?;
    let expected_tier = recorded_market
        .get("data_tier")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{instrument} DepthFrame data_tier 无效")))?;
    let expected_snapshot_count = recorded_market
        .get("snapshot_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(format!("{instrument} DepthFrame snapshot_count 无效")))?;
    let input_hash = frame.input_hash();
    let frame_hash = format!("{input_hash:016x}");
    let snapshot_count =
        u64::try_from(frame.snapshots.len()).map_err(|_| invalid("深度快照数量超出 u64"))?;
    if snapshot_count > crate::artifact_store::MAX_ARTIFACT_ROWS {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            format!(
                "{instrument} DepthFrame 超过 {} 条快照上限",
                crate::artifact_store::MAX_ARTIFACT_ROWS
            ),
        ));
    }
    if configured.instrument != instrument
        || frame.instrument.to_string() != instrument
        || expected_tier != data_tier
        || expected_source != frame.source
        || expected_hash != frame_hash
        || expected_snapshot_count != snapshot_count
        || recorded_market
            .get("start_ts_ms")
            .and_then(Value::as_u64)
            .is_some_and(|start| {
                frame
                    .snapshots
                    .first()
                    .is_none_or(|snapshot| snapshot.ts != start)
            })
        || recorded_market
            .get("end_ts_ms")
            .and_then(Value::as_u64)
            .is_some_and(|end| {
                frame
                    .snapshots
                    .last()
                    .is_none_or(|snapshot| snapshot.ts != end)
            })
    {
        return Err(invalid(format!("{instrument} DepthFrame 输入身份不一致")));
    }
    if !matches!(data_tier, "l1" | "l2") {
        return Err(invalid(format!("{instrument} 深度 data_tier 无效")));
    }
    if data_tier == "l1"
        && frame
            .snapshots
            .iter()
            .any(|snapshot| snapshot.bids.len() != 1 || snapshot.asks.len() != 1)
    {
        return Err(invalid(format!("{instrument} 声明 L1 但输入含多档盘口")));
    }
    let start_ts_ms = frame
        .snapshots
        .first()
        .map(|snapshot| snapshot.ts)
        .ok_or_else(|| invalid(format!("{instrument} DepthFrame 不能为空")))?;
    let end_ts_ms = frame
        .snapshots
        .last()
        .map(|snapshot| snapshot.ts)
        .ok_or_else(|| invalid(format!("{instrument} DepthFrame 不能为空")))?;
    let fingerprint = format!("depth:{data_tier}:{}:{frame_hash}", frame.source);
    let (quality_status, quality_fingerprint) = match (
        configured.dataset_manifest_path.as_deref(),
        quality_record,
        quality_payload,
    ) {
        (None, None, None) => ("not_provided".to_owned(), None),
        (None, Some(_), _) => {
            return Err(invalid(format!(
                "{instrument} 产物包含 spec 未声明的质量清单"
            )));
        }
        (None, None, Some(_)) => {
            return Err(invalid(format!(
                "{instrument} 提供了 spec 未声明的质量清单内容"
            )));
        }
        (Some(_), None, _) | (Some(_), _, None) => {
            return Err(invalid(format!(
                "{instrument} spec 声明质量清单但证据不完整"
            )));
        }
        (Some(_), Some(recorded), Some(payload)) => {
            let manifest = qx_data::DatasetManifestV2::from_json(payload)
                .map_err(|error| invalid(format!("{instrument} 质量清单无效: {error}")))?;
            let manifest_sha = qx_data::sha256_hex_bytes(payload.as_bytes());
            let manifest_fingerprint = manifest
                .fingerprint()
                .map_err(|error| invalid(format!("{instrument} 质量清单指纹失败: {error}")))?;
            let expected_tier = if data_tier == "l1" {
                qx_data::DatasetTier::L1
            } else {
                qx_data::DatasetTier::L2
            };
            let first_ts = start_ts_ms;
            let last_ts = end_ts_ms;
            if manifest.instrument != instrument
                || manifest.tier != expected_tier
                || manifest.provider_version != qx_data::DEPTHFRAME_DATASET_VERSION
                || manifest.dataset.dataset_id != format!("depth-bars:{instrument}")
                || manifest.dataset.version != qx_data::DEPTHFRAME_DATASET_VERSION
                || manifest.dataset.source != frame.source
                || manifest.dataset.schema_version != frame.schema_version
                || manifest.source_lineage.primary_source != frame.source
                || manifest.content_hash != qx_data::sha256_hex_bytes(frame_payload.as_bytes())
                || manifest.dataset.fingerprint != frame_hash
                || manifest.dataset.start_timestamp != first_ts
                || manifest.dataset.end_timestamp != last_ts
                || manifest.quality_report.row_count != snapshot_count
                || manifest.quality_report.usable_tiers != [data_tier]
                || recorded.get("manifest_sha256").and_then(Value::as_str)
                    != Some(manifest_sha.as_str())
                || recorded.get("manifest_fingerprint").and_then(Value::as_str)
                    != Some(manifest_fingerprint.as_str())
                || recorded.get("content_hash").and_then(Value::as_str)
                    != Some(manifest.content_hash.as_str())
                || recorded.get("quality_report")
                    != Some(
                        &serde_json::to_value(&manifest.quality_report)
                            .map_err(|error| invalid(error.to_string()))?,
                    )
            {
                return Err(invalid(format!("{instrument} 深度质量清单与输入不一致")));
            }
            let interval = manifest
                .quality_report
                .interval_ms
                .filter(|value| *value > 0)
                .ok_or_else(|| invalid(format!("{instrument} 深度质量清单缺少正 interval_ms")))?;
            let (duplicates, out_of_order, missing) = frame
                .snapshots
                .windows(2)
                .try_fold(
                    (0_u64, 0_u64, 0_u64),
                    |(duplicates, out_of_order, missing), pair| {
                        if pair[1].ts < pair[0].ts {
                            Some((duplicates, out_of_order.checked_add(1)?, missing))
                        } else if pair[1].ts == pair[0].ts {
                            Some((duplicates.checked_add(1)?, out_of_order, missing))
                        } else {
                            Some((
                                duplicates,
                                out_of_order,
                                missing.checked_add(
                                    (pair[1].ts - pair[0].ts).div_ceil(interval) - 1,
                                )?,
                            ))
                        }
                    },
                )
                .ok_or_else(|| invalid(format!("{instrument} 深度质量计数溢出")))?;
            if manifest.quality_report.duplicate_rows != duplicates
                || manifest.quality_report.out_of_order_rows != out_of_order
                || manifest.quality_report.missing_intervals != missing
            {
                return Err(invalid(format!("{instrument} 深度质量报告与输入不一致")));
            }
            ("verified".to_owned(), Some(manifest_fingerprint))
        }
    };
    Ok(VerifiedMultiMarketDepthInputV1 {
        instrument: instrument.to_owned(),
        data_tier: data_tier.to_owned(),
        source: frame.source.clone(),
        fingerprint,
        input_hash,
        snapshot_count,
        start_ts_ms,
        end_ts_ms,
        quality_status,
        quality_fingerprint,
    })
}

/// Verify contribution sidecar points against the shared equity curve and replayed ledger.
///
/// The sidecar must include exactly one curve for every market in `spec`. Every sampled
/// portfolio point must be reconstructed by summing instrument contributions, and the final
/// sample is independently revalued from the replayed ledger and frozen instrument specs.
pub fn verify_multi_market_pnl_contributions_v1(
    sidecar: &Value,
    equity_rows: &[Value],
    spec: &MultiMarketBacktestSpec,
    ledger: &Ledger,
    market_specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
    final_market_marks: &BTreeMap<InstrumentId, Price>,
) -> Result<VerifiedMultiMarketPnlContributions, AppError> {
    verify_multi_market_pnl_contributions_with_exact_v1(
        sidecar,
        equity_rows,
        None,
        spec,
        ledger,
        market_specs,
        final_market_marks,
    )
}

pub fn verify_multi_market_pnl_contributions_with_exact_v1(
    sidecar: &Value,
    equity_rows: &[Value],
    exact_equity_rows: Option<&[Value]>,
    spec: &MultiMarketBacktestSpec,
    ledger: &Ledger,
    market_specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
    final_market_marks: &BTreeMap<InstrumentId, Price>,
) -> Result<VerifiedMultiMarketPnlContributions, AppError> {
    if equity_rows.is_empty() {
        return Err(invalid("共享权益曲线为空，不能校验逐标的权益贡献"));
    }
    let total_equity_points =
        u64::try_from(equity_rows.len()).map_err(|_| invalid("共享权益点数超出 u64 范围"))?;
    if exact_equity_rows.is_some_and(|rows| rows.len() != equity_rows.len()) {
        return Err(invalid("exact 权益 sidecar 点数与 raw 权益曲线不一致"));
    }
    let exact_equity_values = exact_equity_rows
        .map(|rows| {
            rows.iter()
                .zip(equity_rows)
                .enumerate()
                .map(|(index, (row, raw_row))| {
                    let fields = row
                        .as_object()
                        .ok_or_else(|| invalid("exact 权益采样点必须是对象"))?;
                    let equity_index = fields
                        .get("equity_index")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| invalid("exact 权益索引无效"))?;
                    let ts = fields
                        .get("ts")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| invalid("exact 权益时间戳无效"))?;
                    let amount = fields
                        .get("equity_exact")
                        .and_then(Value::as_str)
                        .map(ExactDecimal::parse)
                        .transpose()
                        .map_err(|error| invalid(format!("exact 权益金额无效: {error}")))?
                        .ok_or_else(|| invalid("exact 权益金额缺失"))?;
                    if fields.len() != 3
                        || equity_index != index as u64
                        || raw_row.get(0).and_then(Value::as_u64) != Some(ts)
                    {
                        return Err(invalid("exact 权益 sidecar 索引或时间轴无效"));
                    }
                    Ok(amount)
                })
                .collect::<Result<Vec<_>, AppError>>()
        })
        .transpose()?;
    let last_equity_index = total_equity_points - 1;
    let expected_instruments = spec
        .markets
        .iter()
        .map(|market| market.instrument.clone())
        .collect::<BTreeSet<_>>();
    if expected_instruments.len() != spec.markets.len() {
        return Err(invalid("多市场规格包含重复标的，不能校验权益贡献"));
    }
    let sidecar = sidecar
        .as_object()
        .ok_or_else(|| invalid("逐标的权益贡献必须是对象"))?;
    let sidecar_instruments = sidecar.keys().cloned().collect::<BTreeSet<_>>();
    if sidecar_instruments != expected_instruments {
        return Err(invalid("逐标的权益贡献标的集合与市场输入不一致"));
    }

    let mut curves = BTreeMap::<String, Vec<VerifiedPnlContributionSample>>::new();
    for instrument in &expected_instruments {
        let rows = sidecar
            .get(instrument)
            .and_then(Value::as_array)
            .ok_or_else(|| invalid(format!("{instrument} 权益贡献必须是数组")))?;
        if rows.is_empty() || rows.len() > MAX_MULTI_MARKET_PNL_SAMPLES {
            return Err(invalid(format!("{instrument} 权益贡献采样点数无效")));
        }
        let mut curve = Vec::with_capacity(rows.len());
        for row in rows {
            let equity_index = row
                .get("equity_index")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid(format!("{instrument} 权益贡献索引无效")))?;
            let ts = row
                .get("ts")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid(format!("{instrument} 权益贡献时间戳无效")))?;
            let pnl_contribution_raw = row
                .get("pnl_contribution_raw")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<i128>().ok())
                .ok_or_else(|| invalid(format!("{instrument} 权益贡献金额无效")))?;
            let pnl_contribution_exact = row
                .get("pnl_contribution_exact")
                .and_then(Value::as_str)
                .map(ExactDecimal::parse)
                .transpose()
                .map_err(|error| invalid(format!("{instrument} 精确权益贡献金额无效: {error}")))?;
            let expected_field_count = if pnl_contribution_exact.is_some() {
                4
            } else {
                3
            };
            if row.as_object().is_none_or(|fields| {
                fields.len() != expected_field_count
                    || !fields.contains_key("equity_index")
                    || !fields.contains_key("ts")
                    || !fields.contains_key("pnl_contribution_raw")
                    || fields.contains_key("pnl_contribution_exact")
                        != pnl_contribution_exact.is_some()
                    || (exact_equity_rows.is_some() && pnl_contribution_exact.is_none())
                    || (exact_equity_rows.is_none() && pnl_contribution_exact.is_some())
            }) {
                return Err(invalid(format!("{instrument} 权益贡献点字段不受支持")));
            }
            if curve
                .last()
                .is_some_and(|previous: &VerifiedPnlContributionSample| {
                    equity_index <= previous.equity_index
                })
            {
                return Err(invalid(format!("{instrument} 权益贡献索引未严格递增")));
            }
            curve.push(VerifiedPnlContributionSample {
                equity_index,
                ts,
                pnl_contribution_raw,
                pnl_contribution_exact,
            });
        }
        if curve.first().map(|sample| sample.equity_index) != Some(0)
            || curve.last().map(|sample| sample.equity_index) != Some(last_equity_index)
        {
            return Err(invalid(format!(
                "{instrument} 权益贡献采样没有覆盖权益曲线首尾"
            )));
        }
        curves.insert(instrument.clone(), curve);
    }

    let sample_count = curves
        .values()
        .next()
        .map(Vec::len)
        .ok_or_else(|| invalid("逐标的权益贡献没有曲线"))?;
    if curves.values().any(|curve| curve.len() != sample_count) {
        return Err(invalid("逐标的权益贡献采样索引集合不一致"));
    }
    let initial_cash_exact = spec.initial_cash_exact_value()?;
    if let Some(exact_values) = exact_equity_values.as_ref() {
        if exact_values.first().copied() != Some(initial_cash_exact) {
            return Err(invalid("逐标的权益贡献 exact 曲线首点与初始资金不一致"));
        }
        for (equity_row, exact_value) in equity_rows.iter().zip(exact_values) {
            let projected_raw = exact_value
                .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
                .and_then(|projected| projected.to_raw(9).ok());
            if equity_row.get(1).and_then(raw_i128) != projected_raw {
                return Err(invalid("逐标的 exact 权益与 raw 权益投影不一致"));
            }
        }
    } else if spec.initial_cash_exact.is_some() {
        return Err(invalid(
            "MultiMarketBacktestSpec v2 复核必须提供 exact 权益 sidecar",
        ));
    }

    for sample_index in 0..sample_count {
        let first_curve_sample = curves
            .values()
            .next()
            .and_then(|curve| curve.get(sample_index))
            .ok_or_else(|| invalid("逐标的权益贡献采样点缺失"))?;
        let equity_index = first_curve_sample.equity_index;
        let ts = first_curve_sample.ts;
        let equity_row = equity_rows
            .get(usize::try_from(equity_index).map_err(|_| invalid("权益索引超出范围"))?)
            .ok_or_else(|| invalid("权益贡献索引超出共享权益曲线"))?;
        if equity_row.get(0).and_then(Value::as_u64) != Some(ts) {
            return Err(invalid("逐标的权益贡献时间戳与共享权益曲线不一致"));
        }

        let mut contribution_sum = 0_i128;
        let mut exact_contribution_sum = exact_equity_rows.map(|_| initial_cash_exact);
        for (instrument, curve) in &curves {
            let sample = curve[sample_index];
            if sample.equity_index != equity_index || sample.ts != ts {
                return Err(invalid("各标的权益贡献采样轴不一致"));
            }
            contribution_sum = contribution_sum
                .checked_add(sample.pnl_contribution_raw)
                .ok_or_else(|| invalid("逐标的权益贡献求和溢出"))?;
            if let Some(exact_sum) = exact_contribution_sum.as_mut() {
                let exact_contribution = sample
                    .pnl_contribution_exact
                    .ok_or_else(|| invalid("exact 权益 sidecar 缺少逐标的精确贡献"))?;
                *exact_sum = exact_sum
                    .checked_add(exact_contribution)
                    .ok_or_else(|| invalid("逐标的精确权益贡献求和溢出"))?;
            }
            if sample_index + 1 == sample_count {
                let instrument_id = InstrumentId::parse(instrument)
                    .ok_or_else(|| invalid(format!("无效标的标识: {instrument}")))?;
                let instrument_spec = market_specs
                    .get(&instrument_id)
                    .ok_or_else(|| invalid(format!("{instrument} 缺少有效市场规格")))?;
                let mark = final_market_marks
                    .get(&instrument_id)
                    .ok_or_else(|| invalid(format!("{instrument} 缺少末尾行情标记")))?;
                let recomputed = ledger
                    .instrument_equity_contribution(
                        &spec.account_id,
                        &instrument_id,
                        Some(mark),
                        &spec.settlement_currency,
                        instrument_spec,
                        &BTreeMap::new(),
                    )
                    .map_err(|error| {
                        invalid(format!("{instrument} 期末盈亏贡献复算失败: {error}"))
                    })?;
                if recomputed != sample.pnl_contribution_raw {
                    return Err(invalid(format!(
                        "{instrument} 期末盈亏贡献与重放账簿估值不一致"
                    )));
                }
                if let Some(expected_exact) = sample.pnl_contribution_exact {
                    let mut exact_cash = BTreeMap::<String, ExactDecimal>::new();
                    for entry in ledger.entries().iter().filter(|entry| {
                        entry.account_id == spec.account_id
                            && entry.instrument.as_ref() == Some(&instrument_id)
                    }) {
                        let amount = match entry.exact_amounts {
                            Some(amounts) => amounts.amount,
                            None => ExactDecimal::from_parts(entry.amount.raw(), 9)
                                .map_err(|error| invalid(error.to_owned()))?,
                        };
                        let total = exact_cash
                            .entry(entry.currency.clone())
                            .or_insert(ExactDecimal::ZERO);
                        *total = total
                            .checked_add(amount)
                            .ok_or_else(|| invalid("精确标的现金归因累计溢出"))?;
                    }
                    let context = ValuationContext::spot(
                        &spec.account_id,
                        final_market_marks,
                        &spec.settlement_currency,
                    )
                    .with_spec(Some(instrument_spec));
                    let exact_recomputed = ledger
                        .instrument_equity_contribution_from_exact_cash(&context, &exact_cash)
                        .map_err(|error| {
                            invalid(format!("{instrument} 期末精确贡献复算失败: {error}"))
                        })?;
                    if exact_recomputed != expected_exact {
                        return Err(invalid(format!(
                            "{instrument} 期末精确贡献与重放账簿估值不一致"
                        )));
                    }
                }
            }
        }

        let reconstructed_equity = spec
            .initial_cash_raw
            .checked_add(contribution_sum)
            .ok_or_else(|| invalid("共享权益归因重建溢出"))?;
        let expected_equity = equity_row
            .get(1)
            .and_then(raw_i128)
            .ok_or_else(|| invalid("权益贡献校验目标无效"))?;
        if reconstructed_equity != expected_equity {
            return Err(invalid(format!(
                "第 {equity_index} 个采样点的逐标的贡献与共享账户权益不守恒"
            )));
        }
        if let (Some(exact_values), Some(exact_sum)) =
            (exact_equity_values.as_ref(), exact_contribution_sum)
        {
            let exact_row_value = *exact_values
                .get(usize::try_from(equity_index).map_err(|_| invalid("exact 权益索引超出范围"))?)
                .ok_or_else(|| invalid("exact 权益采样索引越界"))?;
            if exact_sum != exact_row_value {
                return Err(invalid(format!(
                    "第 {equity_index} 个采样点的逐标的 exact 贡献与 exact 共享权益不守恒"
                )));
            }
            if sample_index + 1 == sample_count {
                let valuation = ValuationContext::spot(
                    &spec.account_id,
                    final_market_marks,
                    &spec.settlement_currency,
                )
                .with_market_specs(market_specs);
                let exact_total = ledger
                    .valuate_exact(&ExactValuationContext::new(valuation))
                    .map_err(|error| invalid(format!("期末 exact 权益复算失败: {error}")))?;
                if exact_total != exact_row_value {
                    return Err(invalid("期末 exact 权益与重放账簿估值不一致"));
                }
            }
        }
    }

    Ok(VerifiedMultiMarketPnlContributions {
        total_equity_points,
        curves,
    })
}

fn raw_i128(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<i128>().ok()))
}

fn equity_drawdown_bps(rows: &[Value]) -> Result<u32, AppError> {
    let mut peak = 0_i128;
    let mut maximum = 0_u32;
    for row in rows {
        let value = row
            .get(1)
            .and_then(raw_i128)
            .ok_or_else(|| invalid("权益曲线包含无效金额"))?;
        peak = peak.max(value);
        if peak <= 0 || value >= peak {
            continue;
        }
        let drawdown = peak
            .checked_sub(value)
            .and_then(|loss| loss.checked_mul(10_000))
            .and_then(|loss| loss.checked_div(peak))
            .unwrap_or(10_000)
            .clamp(0, 10_000) as u32;
        maximum = maximum.max(drawdown);
    }
    Ok(maximum)
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::Conflict, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MultiMarketBacktestMarket, MultiMarketDepthBacktestMarket};

    fn spec() -> MultiMarketBacktestSpec {
        let market = |instrument: &str| MultiMarketBacktestMarket {
            instrument: instrument.to_owned(),
            bars_path: "unused.json".into(),
            dataset_manifest_path: None,
            priority: 0,
            market_spec: None,
            asset_class: None,
            fee_schedule: None,
            order_policy: None,
            margin_model: None,
            funding_bps: 0,
            funding_events: Vec::new(),
            strategies: Vec::new(),
        };
        MultiMarketBacktestSpec {
            schema_version: crate::MULTI_MARKET_BACKTEST_SCHEMA_VERSION,
            run_id: "pnl-report-test".into(),
            account_id: "report-account".into(),
            settlement_currency: "USDT".into(),
            initial_cash_raw: 100,
            initial_cash_exact: None,
            seed: 1,
            output_dir: "unused".into(),
            markets: vec![market("BTCUSDT.BINANCE"), market("ETHUSDT.BINANCE")],
        }
    }

    fn depth_spec() -> MultiMarketDepthBacktestSpec {
        let market = |instrument: &str, tier: &str, dataset_manifest_path| {
            let instrument_id = InstrumentId::parse(instrument).unwrap();
            MultiMarketDepthBacktestMarket {
                instrument: instrument.to_owned(),
                depth_path: format!("{instrument}.depth.json"),
                dataset_manifest_path,
                data_tier: Some(tier.to_owned()),
                priority: 0,
                market_spec: TradingInstrumentSpec::spot_default(&instrument_id, "USDT"),
                fee_bps: 0,
                funding_bps: 0,
                funding_events: Vec::new(),
                latency_snapshots: 0,
                queue_position_bps: 0,
                queue_position_model: Default::default(),
                market_impact_bps: 0,
                strategy: crate::BuiltinStrategySpec::new(
                    qx_strategy::BuiltinStrategyKind::SmaCross,
                    format!("strategy-{instrument}"),
                ),
                order_policy: None,
            }
        };
        MultiMarketDepthBacktestSpec {
            schema_version: crate::MULTI_MARKET_DEPTH_BACKTEST_SCHEMA_VERSION,
            run_id: "multi-market-depth-identity".into(),
            account_id: "report-account".into(),
            settlement_currency: "USDT".into(),
            initial_cash_raw: 100,
            initial_cash_exact: None,
            output_dir: "unused".into(),
            markets: vec![
                market("BTCUSDT.BINANCE", "l1", Some("btc-quality.json".into())),
                market("ETHUSDT.BINANCE", "l2", None),
            ],
        }
    }

    fn environment() -> (
        Ledger,
        BTreeMap<InstrumentId, TradingInstrumentSpec>,
        BTreeMap<InstrumentId, Price>,
    ) {
        let markets = ["BTCUSDT.BINANCE", "ETHUSDT.BINANCE"]
            .into_iter()
            .map(|name| InstrumentId::parse(name).unwrap())
            .collect::<Vec<_>>();
        let specs = markets
            .iter()
            .map(|instrument| {
                (
                    instrument.clone(),
                    TradingInstrumentSpec::spot_default(instrument, "USDT"),
                )
            })
            .collect();
        let marks = markets
            .into_iter()
            .map(|instrument| (instrument, Price::from_raw(10)))
            .collect();
        (Ledger::new(), specs, marks)
    }

    fn sidecar(btc_samples: &[(u64, u64, &str)], eth_samples: &[(u64, u64, &str)]) -> Value {
        let rows = |samples: &[(u64, u64, &str)]| {
            samples
                .iter()
                .map(|(equity_index, ts, amount)| {
                    serde_json::json!({
                        "equity_index": equity_index,
                        "ts": ts,
                        "pnl_contribution_raw": amount
                    })
                })
                .collect::<Vec<_>>()
        };
        serde_json::json!({
            "BTCUSDT.BINANCE": rows(btc_samples),
            "ETHUSDT.BINANCE": rows(eth_samples)
        })
    }

    #[test]
    fn multi_market_contributions_reconcile_every_sample_and_final_ledger_value() {
        let spec = spec();
        let (ledger, market_specs, marks) = environment();
        let curves = sidecar(&[(0, 10, "0"), (1, 20, "0")], &[(0, 10, "0"), (1, 20, "0")]);
        let equity = [serde_json::json!([10, 100]), serde_json::json!([20, 100])];
        let verified = verify_multi_market_pnl_contributions_v1(
            &curves,
            &equity,
            &spec,
            &ledger,
            &market_specs,
            &marks,
        )
        .expect("contributions should reconcile");
        assert_eq!(verified.total_equity_points, 2);
        assert_eq!(verified.curves.len(), 2);
        assert_eq!(
            verified.curves["BTCUSDT.BINANCE"][1].pnl_contribution_raw,
            0
        );
    }

    #[test]
    fn multi_market_exact_contributions_preserve_subnano_cash_and_replay_check() {
        let spec = spec();
        let (mut ledger, market_specs, marks) = environment();
        ledger
            .deposit("report-account", "USDT", qx_core::Money::from_raw(100), 1)
            .unwrap();
        let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let amount = ExactDecimal::parse("0.0000000001").unwrap();
        ledger
            .apply_entry(qx_core::LedgerEntry {
                id: 1,
                account_id: "report-account".into(),
                currency: "USDT".into(),
                kind: qx_core::LedgerEntryKind::TradeCash,
                amount: qx_core::Money::ZERO,
                instrument: Some(btc),
                quantity: qx_core::Quantity::ZERO,
                price: None,
                order_id: None,
                ts: 2,
                multiplier: 1,
                position_side: None,
                leverage: None,
                exact_amounts: Some(qx_core::ExactLedgerAmounts {
                    amount,
                    quantity: ExactDecimal::ZERO,
                    price: None,
                    contract_size: None,
                    inverse: None,
                }),
            })
            .unwrap();
        let curves = serde_json::json!({
            "BTCUSDT.BINANCE": [
                {"equity_index": 0, "ts": 10, "pnl_contribution_raw": "0", "pnl_contribution_exact": "0"},
                {"equity_index": 1, "ts": 20, "pnl_contribution_raw": "0", "pnl_contribution_exact": "0.0000000001"}
            ],
            "ETHUSDT.BINANCE": [
                {"equity_index": 0, "ts": 10, "pnl_contribution_raw": "0", "pnl_contribution_exact": "0"},
                {"equity_index": 1, "ts": 20, "pnl_contribution_raw": "0", "pnl_contribution_exact": "0"}
            ]
        });
        let equity = [serde_json::json!([10, 100]), serde_json::json!([20, 100])];
        let exact_equity = [
            serde_json::json!({"equity_index": 0, "ts": 10, "equity_exact": "0.0000001"}),
            serde_json::json!({"equity_index": 1, "ts": 20, "equity_exact": "0.0000001001"}),
        ];

        let verified = verify_multi_market_pnl_contributions_with_exact_v1(
            &curves,
            &equity,
            Some(&exact_equity),
            &spec,
            &ledger,
            &market_specs,
            &marks,
        )
        .unwrap();

        assert_eq!(
            verified.curves["BTCUSDT.BINANCE"][1].pnl_contribution_exact,
            Some(amount)
        );
        let mut inconsistent = exact_equity;
        inconsistent[1]["equity_exact"] = Value::String("0.0000001002".into());
        assert!(verify_multi_market_pnl_contributions_with_exact_v1(
            &curves,
            &equity,
            Some(&inconsistent),
            &spec,
            &ledger,
            &market_specs,
            &marks,
        )
        .is_err());
    }

    #[test]
    fn multi_market_contributions_reject_a_non_conserving_equity_sample() {
        let spec = spec();
        let (ledger, market_specs, marks) = environment();
        let curves = sidecar(&[(0, 10, "1"), (1, 20, "0")], &[(0, 10, "0"), (1, 20, "0")]);
        let equity = [serde_json::json!([10, 100]), serde_json::json!([20, 100])];
        assert!(verify_multi_market_pnl_contributions_v1(
            &curves,
            &equity,
            &spec,
            &ledger,
            &market_specs,
            &marks,
        )
        .is_err());
    }

    #[test]
    fn multi_market_contributions_reject_terminal_values_not_in_the_replayed_ledger() {
        let spec = spec();
        let (ledger, market_specs, marks) = environment();
        let curves = sidecar(
            &[(0, 10, "0"), (1, 20, "1")],
            &[(0, 10, "0"), (1, 20, "-1")],
        );
        let equity = [serde_json::json!([10, 100]), serde_json::json!([20, 100])];
        assert!(verify_multi_market_pnl_contributions_v1(
            &curves,
            &equity,
            &spec,
            &ledger,
            &market_specs,
            &marks,
        )
        .is_err());
    }

    #[test]
    fn multi_market_spec_fingerprint_ignores_local_paths_and_sorts_strategy_identities() {
        let spec = spec();
        let mut relocated = spec.clone();
        relocated.output_dir = "another/output".into();
        relocated.markets[0].bars_path = "another/bars.json".into();
        relocated.markets[0].dataset_manifest_path = Some("another/quality.json".into());
        let alpha = StrategyArtifactIdentity {
            strategy_id: "alpha".into(),
            strategy_version: "v1".into(),
            source_sha256: "a".repeat(64),
        };
        let beta = StrategyArtifactIdentity {
            strategy_id: "beta".into(),
            strategy_version: "v2".into(),
            source_sha256: "b".repeat(64),
        };
        assert_eq!(
            multi_market_spec_fingerprint_v1(&spec, &[alpha.clone(), beta.clone()]).unwrap(),
            multi_market_spec_fingerprint_v1(&relocated, &[beta, alpha]).unwrap(),
        );
        assert_eq!(
            multi_market_spec_fingerprint_v1(&spec, &[]).unwrap(),
            multi_market_spec_fingerprint_v1(&relocated, &[]).unwrap(),
        );
    }

    #[test]
    fn multi_market_input_identity_uses_spec_order_and_quality_components() {
        let mut spec = spec();
        spec.markets[1].dataset_manifest_path = Some("quality.json".into());
        let identity = multi_market_input_set_identity_v1(
            &spec,
            &[
                MultiMarketInputIdentityV1 {
                    instrument: "ETHUSDT.BINANCE".into(),
                    bar_fingerprint: "eth-bars".into(),
                    quality_fingerprint: Some("eth-quality".into()),
                },
                MultiMarketInputIdentityV1 {
                    instrument: "BTCUSDT.BINANCE".into(),
                    bar_fingerprint: "btc-bars".into(),
                    quality_fingerprint: None,
                },
            ],
        )
        .expect("the exact configured market set should produce an identity");
        let mut expected = Fnv1a::new();
        expected.write_text("BTCUSDT.BINANCE");
        expected.write_text("btc-bars");
        expected.write_text("ETHUSDT.BINANCE");
        expected.write_text("eth-bars");
        expected.write_text("eth-quality");
        assert_eq!(
            identity.data_fingerprint,
            format!("{:016x}", expected.finish())
        );
        assert_eq!(identity.input_components.len(), 3);
        assert!(multi_market_input_set_identity_v1(
            &spec,
            &[MultiMarketInputIdentityV1 {
                instrument: "BTCUSDT.BINANCE".into(),
                bar_fingerprint: "btc-bars".into(),
                quality_fingerprint: None,
            }]
        )
        .is_err());
    }

    #[test]
    fn multi_market_depth_input_identity_is_shared_and_path_independent() {
        let mut spec = depth_spec();
        let inputs = [
            MultiMarketDepthInputIdentityV1 {
                instrument: "ETHUSDT.BINANCE".into(),
                data_tier: "l2".into(),
                source: "eth-feed".into(),
                input_hash: 22,
                start_ts_ms: 1,
                end_ts_ms: 2,
                quality_fingerprint: None,
            },
            MultiMarketDepthInputIdentityV1 {
                instrument: "BTCUSDT.BINANCE".into(),
                data_tier: "l1".into(),
                source: "btc-feed".into(),
                input_hash: 11,
                start_ts_ms: 10,
                end_ts_ms: 20,
                quality_fingerprint: Some("btc-quality".into()),
            },
        ];
        let identity = multi_market_depth_input_set_identity_v1(&spec, &inputs)
            .expect("the exact configured depth input set should produce an identity");
        let mut expected = Fnv1a::new();
        expected.write_text("BTCUSDT.BINANCE");
        expected.write_text("l1");
        expected.write_text("btc-feed");
        expected.write_u64(11);
        expected.write_text("btc-quality");
        expected.write_text("ETHUSDT.BINANCE");
        expected.write_text("l2");
        expected.write_text("eth-feed");
        expected.write_u64(22);
        assert_eq!(
            identity.data_fingerprint,
            format!("{:016x}", expected.finish())
        );
        assert_eq!(identity.input_components.len(), 3);
        assert_eq!(
            identity.input_components["depthframe:BTCUSDT.BINANCE"],
            "depth:l1:btc-feed:000000000000000b"
        );

        spec.output_dir = "relocated-output".into();
        spec.markets[0].depth_path = "relocated-btc.json".into();
        spec.markets[0].dataset_manifest_path = Some("relocated-quality.json".into());
        assert_eq!(
            identity.data_fingerprint,
            multi_market_depth_input_set_identity_v1(&spec, &inputs)
                .unwrap()
                .data_fingerprint
        );

        let mut mismatched_tier = inputs.clone();
        mismatched_tier[1].data_tier = "l2".into();
        assert!(multi_market_depth_input_set_identity_v1(&spec, &mismatched_tier).is_err());
        assert!(multi_market_depth_input_set_identity_v1(&spec, &inputs[..1]).is_err());
    }

    #[test]
    fn multi_market_depth_spec_fingerprint_sorts_and_binds_strategy_identities() {
        let spec = depth_spec();
        let alpha = StrategyArtifactIdentity {
            strategy_id: "strategy-BTCUSDT.BINANCE".into(),
            strategy_version: "1".into(),
            source_sha256: "a".repeat(64),
        };
        let beta = StrategyArtifactIdentity {
            strategy_id: "strategy-ETHUSDT.BINANCE".into(),
            strategy_version: "2".into(),
            source_sha256: "b".repeat(64),
        };
        let mut identity_spec = spec.clone();
        identity_spec.output_dir.clear();
        for market in &mut identity_spec.markets {
            market.depth_path.clear();
            market.dataset_manifest_path = None;
        }
        let mut legacy_shape_hasher = Fnv1a::new();
        legacy_shape_hasher.write_text(&identity_spec.to_json().unwrap());
        for identity in [&alpha, &beta] {
            legacy_shape_hasher.write_text(&identity.strategy_id);
            legacy_shape_hasher.write_text(&identity.strategy_version);
            legacy_shape_hasher.write_text(&identity.source_sha256);
        }
        assert_eq!(
            multi_market_depth_spec_fingerprint_v1(&spec, &[alpha.clone(), beta.clone()]).unwrap(),
            format!("{:016x}", legacy_shape_hasher.finish())
        );
        assert_eq!(
            multi_market_depth_spec_fingerprint_v1(&spec, &[alpha.clone(), beta.clone()]).unwrap(),
            multi_market_depth_spec_fingerprint_v1(&spec, &[beta.clone(), alpha.clone()]).unwrap()
        );
        let mut relocated = spec.clone();
        relocated.output_dir = "relocated-output".into();
        relocated.markets[0].depth_path = "relocated-btc.json".into();
        relocated.markets[0].dataset_manifest_path = Some("relocated-quality.json".into());
        assert_eq!(
            multi_market_depth_spec_fingerprint_v1(&spec, &[alpha.clone(), beta.clone()]).unwrap(),
            multi_market_depth_spec_fingerprint_v1(&relocated, &[beta.clone(), alpha.clone()])
                .unwrap()
        );
        let mut unknown = alpha.clone();
        unknown.strategy_id = "orphan-strategy".into();
        assert!(multi_market_depth_spec_fingerprint_v1(&spec, &[unknown]).is_err());
        assert!(multi_market_depth_spec_fingerprint_v1(&spec, &[alpha.clone(), alpha]).is_err());
    }

    #[test]
    fn multi_market_bar_sidecars_reconcile_metrics_and_filled_events() {
        let spec = spec();
        let fill = serde_json::json!({
            "ts": 30,
            "order_id": 7,
            "qty_raw": "2",
            "price_raw": "5",
            "fee_raw": "1",
            "strategy_id": "sma"
        });
        let events = [serde_json::json!({
            "seq": 9,
            "kind": { "Filled": { "fill": fill.clone() } }
        })];
        let equity = serde_json::json!([[10, 100], [20, 110], [30, 105]]);
        let fills = serde_json::json!([fill]);
        let verified = verify_multi_market_bar_sidecars_v1(
            &equity, &fills, &events, &spec, 3, 1, 105, 500, 454,
        )
        .expect("equity metrics and Filled facts should reconcile");
        assert_eq!(verified.equity_points, 3);
        assert_eq!(verified.first_equity_raw, 100);
        assert_eq!(verified.final_equity_raw, 105);
        assert_eq!(verified.return_bps, 500);
        assert_eq!(verified.max_drawdown_bps, 454);
        assert_eq!(verified.fill_rows, 1);

        let tampered_fills = serde_json::json!([]);
        assert!(verify_multi_market_bar_sidecars_v1(
            &equity,
            &tampered_fills,
            &events,
            &spec,
            3,
            1,
            105,
            500,
            454,
        )
        .is_err());
        assert!(verify_multi_market_bar_sidecars_v1(
            &equity, &fills, &events, &spec, 3, 1, 105, 500, 400,
        )
        .is_err());
    }

    #[test]
    fn raw_event_report_sidecars_keep_the_legacy_verification_path() {
        let spec = spec();
        let fill = qx_core::Fill {
            order_id: 7,
            qty: qx_core::Quantity::from_raw(2),
            price: qx_core::Price::from_raw(5),
            fee: qx_core::Money::from_raw(1),
            ts: 30,
            account_id: String::new(),
            strategy_id: Some("sma".into()),
            ..Default::default()
        };
        let mut digest = qx_strategy::file_digest::Sha256Accumulator::new();
        digest.update(b"qx-event-fill-sequence-v1\0");
        update_fill_sequence_digest(&mut digest, &event_fill_identity(&fill))
            .expect("fill identity digest");
        let fills = serde_json::json!([serde_json::to_value(&fill).unwrap()]);
        let equity = serde_json::json!([[10, 100], [20, 110], [30, 105]]);

        let verified = verify_multi_market_bar_sidecars_from_event_report_v1(
            &equity,
            &fills,
            1,
            &digest.finish_hex(),
            &spec,
            3,
            105,
            500,
            454,
        )
        .expect("legacy raw EventReport sidecars should verify");
        assert_eq!(verified.fill_rows, 1);
        assert_eq!(verified.final_equity_raw, 105);
    }

    #[test]
    fn legacy_multi_market_depth_fills_match_the_inline_filled_event_rows() {
        let fill = serde_json::json!({
            "ts": 30,
            "order_id": 7,
            "qty_raw": "2",
            "price_raw": "5",
            "fee_raw": "1",
            "strategy_id": "sma"
        });
        let events = [serde_json::json!({
            "seq": 9,
            "kind": { "Filled": { "fill": fill.clone() } }
        })];
        let fills = serde_json::json!([fill]);

        verify_multi_market_legacy_event_fill_sidecar_v1(&fills, &events, 1)
            .expect("legacy sidecar rows should match embedded Filled events");
        assert!(verify_multi_market_legacy_event_fill_sidecar_v1(&fills, &events, 2).is_err());
        assert!(verify_multi_market_legacy_event_fill_sidecar_v1(
            &serde_json::json!([]),
            &events,
            1
        )
        .is_err());
    }

    fn bar_frame_fixture() -> (String, qx_datastruct::BarFrame) {
        let payload = serde_json::json!({
            "schema_version": 1,
            "instrument": "BTCUSDT.BINANCE",
            "source": "multi-market-report-test",
            "ts": [1_000, 2_000],
            "open_raw": [10, 11],
            "high_raw": [11, 12],
            "low_raw": [9, 10],
            "close_raw": [10, 11],
            "volume_raw": [100, 110]
        })
        .to_string();
        let frame = qx_datastruct::BarFrame::from_json(&payload).expect("BarFrame fixture");
        (payload, frame)
    }

    #[test]
    fn multi_market_bar_input_reconciles_bar_identity_and_quality_manifest() {
        let (frame_payload, frame) = bar_frame_fixture();
        let fingerprint = qx_data::fingerprint_bar_frame(&frame).expect("BarFrame fingerprint");
        let recorded_market = serde_json::json!({
            "instrument": "BTCUSDT.BINANCE",
            "content_hash": fingerprint,
            "row_count": 2
        });
        let mut configured = spec().markets[0].clone();
        let bar_only = verify_multi_market_bar_input_v1(
            &configured,
            &recorded_market,
            &frame_payload,
            &frame,
            None,
            None,
        )
        .expect("BarFrame identity should reconcile without optional quality evidence");
        assert_eq!(bar_only.quality_status, "not_provided");
        assert_eq!(bar_only.row_count, 2);
        assert_eq!(bar_only.source_end_ts, 2_000);
        assert_eq!(bar_only.final_close_raw, 11);

        configured.dataset_manifest_path = Some("quality.json".into());
        let frame_sha256 = qx_data::sha256_hex_bytes(frame_payload.as_bytes());
        let manifest = qx_data::DatasetManifestV2 {
            manifest_version: qx_data::DATASET_MANIFEST_V2_SCHEMA_VERSION,
            dataset: qx_data::DatasetManifest {
                dataset_id: "multi-market-test".into(),
                version: "1".into(),
                source: "test-provider".into(),
                fingerprint: qx_data::fingerprint_bar_frame(&frame).unwrap(),
                schema_version: qx_datastruct::BAR_FRAME_JSON_SCHEMA_VERSION,
                start_timestamp: 1_000,
                end_timestamp: 2_000,
            },
            instrument: "BTCUSDT.BINANCE".into(),
            timezone: "UTC".into(),
            tier: qx_data::DatasetTier::Bar,
            provider_version: qx_data::BARFRAME_DATASET_VERSION.into(),
            content_hash: frame_sha256,
            quality_report: qx_data::DatasetQualityReport {
                row_count: 2,
                duplicate_rows: 0,
                out_of_order_rows: 0,
                missing_intervals: 0,
                interval_ms: Some(1_000),
                timezone: "UTC".into(),
                corporate_action_coverage: false,
                usable_tiers: vec!["bar".into()],
            },
            source_lineage: qx_data::DatasetSourceLineage {
                primary_source: "test-provider".into(),
                backup_sources: Vec::new(),
            },
        };
        let manifest_payload = manifest.to_json().expect("manifest JSON");
        let manifest_fingerprint = manifest.fingerprint().expect("manifest fingerprint");
        let quality_record = serde_json::json!({
            "manifest_path": "quality.json",
            "manifest_sha256": qx_data::sha256_hex_bytes(manifest_payload.as_bytes()),
            "manifest_fingerprint": manifest_fingerprint,
            "quality_report": manifest.quality_report
        });
        let verified = verify_multi_market_bar_input_v1(
            &configured,
            &recorded_market,
            &frame_payload,
            &frame,
            Some(&quality_record),
            Some(&manifest_payload),
        )
        .expect("BarFrame and DatasetManifestV2 should reconcile");
        assert_eq!(verified.quality_status, "verified");
        assert!(verified.quality_fingerprint.is_some());

        let mut tampered_record = quality_record;
        tampered_record["manifest_sha256"] = serde_json::json!("0".repeat(64));
        assert!(verify_multi_market_bar_input_v1(
            &configured,
            &recorded_market,
            &frame_payload,
            &frame,
            Some(&tampered_record),
            Some(&manifest_payload),
        )
        .is_err());
    }

    #[test]
    fn multi_market_quality_records_reject_missing_or_orphan_market_entries() {
        let mut spec = spec();
        spec.markets[0].dataset_manifest_path = Some("quality.json".into());
        let matching = serde_json::json!({"BTCUSDT.BINANCE": {}});
        assert!(validate_multi_market_quality_manifest_set_v1(&spec, &matching).is_ok());
        assert!(
            validate_multi_market_quality_manifest_set_v1(&spec, &serde_json::json!({})).is_err()
        );
        assert!(validate_multi_market_quality_manifest_set_v1(
            &spec,
            &serde_json::json!({"BTCUSDT.BINANCE": {}, "UNKNOWN.BINANCE": {}})
        )
        .is_err());
    }
}
