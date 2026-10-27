//! Versioned, bounded core read model for verified research runs.
//!
//! HTTP, CLI, Python and SDK report surfaces may add presentation-specific drilldowns, but
//! the identity, verification verdict and bounded equity/fill samples come from this use case.

use crate::artifact_store::RunRecordArtifactLocators;
use crate::artifact_store::{
    artifact_too_large, read_bounded_line, read_text_bounded, read_text_bounded_by_id, ArtifactId,
    ArtifactLocatorV1, ArtifactReader, ArtifactStream, FileArtifactReader, MAX_ARTIFACT_ROWS,
};
use crate::cases::{
    verify_depth_run_with_reader, verify_run_with_reader, ProviderBacktestOutcome,
    ProviderDepthBacktestOutcome,
};
use crate::{
    AppError, AppErrorCategory, BacktestOutcome, CallerCapability, DatasetVerdict,
    DepthBacktestOutcome, MultiMarketBacktestOutcome, MultiMarketBacktestSpec,
    MultiMarketDepthBacktestOutcome, MultiMarketDepthBacktestSpec, RunContext,
    StrategyArtifactIdentity, VerificationResult,
};
use qx_provider::DatasetManifestV2;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;

pub const RESEARCH_RUN_REPORT_SCHEMA_VERSION: u32 = 1;
pub const PROVIDER_BACKTEST_REPORT_SCHEMA_VERSION_V1: u32 = 1;
pub const PROVIDER_DEPTH_BACKTEST_REPORT_SCHEMA_VERSION_V1: u32 = 1;
const MAX_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SUMMARY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_EQUITY_ROWS: usize = 1_000;
const MAX_FILL_ROWS: usize = 200;
pub const EVENT_REPORT_PAGE_SCHEMA_VERSION: u32 = 1;
pub const MAX_EVENT_REPORT_PAGE_ROWS: usize = 300;

/// Build the market-bar context visible to a report event under the engine's causal ordering.
/// Strategy commands use a strict timestamp prefix, so the bar that triggered a decision is not
/// included in its completed-history context.
pub fn multi_market_bar_context(
    frame: &qx_datastruct::BarFrame,
    event_ts: u64,
    event_priority: u8,
) -> Option<Value> {
    let strict_prior =
        event_priority == qx_core::Priority::COMMAND || event_priority <= qx_core::Priority::MARKET;
    let visible_end = frame.ts.partition_point(|bar_ts| {
        if strict_prior {
            *bar_ts < event_ts
        } else {
            *bar_ts <= event_ts
        }
    });
    let bar_index = visible_end.checked_sub(1)?;
    let bar_ts = *frame.ts.get(bar_index)?;
    Some(serde_json::json!({
        "event_ts": event_ts,
        "bar_index": bar_index,
        "relation": if bar_ts == event_ts { "same_timestamp_visible" } else { "previous_visible" },
        "bar": {
            "ts": bar_ts,
            "open_raw": frame.open_raw.get(bar_index)?.to_string(),
            "high_raw": frame.high_raw.get(bar_index)?.to_string(),
            "low_raw": frame.low_raw.get(bar_index)?.to_string(),
            "close_raw": frame.close_raw.get(bar_index)?.to_string(),
            "volume_raw": frame.volume_raw.get(bar_index)?.to_string()
        }
    }))
}

/// Stable identity for matching a replayed Filled fact to one row in `fills.csv`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EventFillIdentityV1 {
    pub ts: u64,
    pub order_id: u64,
    pub qty_raw: i128,
    pub price_raw: i128,
    pub fee_raw: i128,
    pub strategy_id: String,
}

/// A replay-verified page and the complete fill multiset derived from its EventLog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventReportPageV1 {
    pub schema_version: u32,
    pub total: u64,
    pub offset: u64,
    pub truncated: bool,
    pub rows: Vec<Value>,
    pub fill_counts: Vec<EventFillCountV1>,
}

/// Stream-verified shared-account EventLog facts for a multi-market report.
#[derive(Clone, Debug)]
pub struct VerifiedMultiMarketEventReportV1 {
    pub total: u64,
    pub offset: u64,
    pub truncated: bool,
    pub rows: Vec<Value>,
    pub fill_count: u64,
    pub fill_sequence_sha256: String,
    pub order_markets: BTreeMap<u64, String>,
    pub ledger: qx_core::Ledger,
}

/// Bounded JSON request for the replay-verified multi-market EventLog page reader.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiMarketEventReportReadRequestV1 {
    pub schema_version: u32,
    pub outcome: MultiMarketBacktestOutcome,
    #[serde(default)]
    pub page: Option<MultiMarketEventReportPageRequestV1>,
}

/// Bounded request for a shared-account multi-market L1/L2 EventLog report page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiMarketDepthEventReportReadRequestV1 {
    pub schema_version: u32,
    pub outcome: MultiMarketDepthBacktestOutcome,
    #[serde(default)]
    pub page: Option<MultiMarketEventReportPageRequestV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiMarketEventReportPageRequestV1 {
    pub offset: u64,
    pub limit: usize,
}

/// Versioned, bounded facts from a fully replayed multi-market EventLog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiMarketEventReportPageV1 {
    pub schema_version: u32,
    pub run_id: String,
    pub total: u64,
    pub offset: u64,
    pub truncated: bool,
    pub rows: Vec<Value>,
    pub fill_count: u64,
    pub fill_sequence_sha256: String,
    pub order_markets: BTreeMap<u64, String>,
    pub replayed_ledger_entries: u64,
}

/// Verified application inputs retained for API report projection after event replay.
///
/// `artifact` is bounded by `MAX_ARTIFACT_BYTES`; the event rows inside `page` remain bounded by
/// the requested page limit. Keeping the already-verified spec and manifest here prevents API
/// adapters from independently parsing another identity path.
pub struct MultiMarketEventReportReadModelV1 {
    pub artifact: Value,
    pub spec: MultiMarketBacktestSpec,
    pub manifest: qx_core::RunManifest,
    pub page: MultiMarketEventReportPageV1,
    pub ledger: qx_core::Ledger,
}

/// Verified inputs retained by an API adapter after multi-market Depth event replay.
pub struct MultiMarketDepthEventReportReadModelV1 {
    pub artifact: Value,
    pub spec: MultiMarketDepthBacktestSpec,
    pub manifest: qx_core::RunManifest,
    pub page: MultiMarketEventReportPageV1,
}

pub const MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventFillCountV1 {
    pub identity: EventFillIdentityV1,
    pub count: usize,
}

pub(crate) fn update_fill_sequence_digest(
    digest: &mut qx_strategy::file_digest::Sha256Accumulator,
    identity: &EventFillIdentityV1,
) -> Result<(), AppError> {
    let encoded = serde_json::to_vec(identity).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("成交身份摘要序列化失败: {error}"),
        )
    })?;
    let len = u64::try_from(encoded.len()).map_err(|_| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            "成交身份摘要长度超出 u64",
        )
    })?;
    digest.update(&len.to_le_bytes());
    digest.update(&encoded);
    Ok(())
}

pub(crate) fn event_fill_identity(fill: &qx_core::Fill) -> EventFillIdentityV1 {
    EventFillIdentityV1 {
        ts: fill.ts,
        order_id: fill.order_id,
        qty_raw: fill.qty.raw(),
        price_raw: fill.price.raw(),
        fee_raw: fill.fee.raw(),
        strategy_id: fill.strategy_id.clone().unwrap_or_default(),
    }
}

/// Common bounded row set. Row values are strings to preserve CSV precision and old wire
/// behavior across the Rust, Python, C++ and Web consumers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResearchReportRowsV1 {
    pub total: u64,
    pub truncated: bool,
    pub rows: Vec<Value>,
}

/// Core report facts shared by every report entry point. More detailed event/bar/depth data is
/// attached by the corresponding presentation surface only after this verdict succeeds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResearchRunReportV1 {
    pub schema_version: u32,
    pub run_id: String,
    pub instrument: String,
    pub verification: VerificationResult,
    pub summary: Value,
    pub equity: ResearchReportRowsV1,
    pub fills: ResearchReportRowsV1,
}

/// Provider Bar report envelope. The nested core report remains identical to ordinary Bar
/// reports; the additional fields expose only the frozen, identity-bound provider manifest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderBacktestReportV1 {
    pub schema_version: u32,
    pub report: ResearchRunReportV1,
    pub dataset_verdict: DatasetVerdict,
    pub manifest: DatasetManifestV2,
}

/// Provider Tick/OrderBook report envelope with the frozen data identity verified against the
/// provider provenance carried by the outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderDepthBacktestReportV1 {
    pub schema_version: u32,
    pub report: ResearchRunReportV1,
    pub provider_id: String,
    pub provider_version: String,
    pub request_id: String,
    pub retry_chain: Vec<String>,
    pub source_hash: String,
    pub query_digest: String,
    pub depth_input_hash: String,
}

pub fn read_run_report_v1(
    outcome: &BacktestOutcome,
    context: &RunContext,
) -> Result<ResearchRunReportV1, AppError> {
    read_run_report_v1_with_reader(outcome, context, &FileArtifactReader)
}

/// Read and verify a Bar report through an application-owned storage adapter.
pub fn read_run_report_v1_with_reader(
    outcome: &BacktestOutcome,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<ResearchRunReportV1, AppError> {
    let verification = verify_run_with_reader(outcome, context, reader)?;
    let declared_artifacts = if verification.verified {
        let summary = read_json_bounded(
            reader,
            &outcome.artifacts.summary,
            MAX_SUMMARY_BYTES,
            "研究摘要",
        )?;
        crate::artifact_store::declared_run_record_artifact_locators(&summary, reader)?
    } else {
        None
    };
    read_report(
        &outcome.run_id,
        &outcome.instrument,
        &outcome.artifacts.summary,
        &outcome.artifacts.equity,
        &outcome.artifacts.fills,
        verification,
        declared_artifacts.as_ref(),
        reader,
    )
}

pub fn read_depth_run_report_v1(
    outcome: &DepthBacktestOutcome,
    context: &RunContext,
) -> Result<ResearchRunReportV1, AppError> {
    read_depth_run_report_v1_with_reader(outcome, context, &FileArtifactReader)
}

/// Read and verify a depth report through an application-owned storage adapter.
pub fn read_depth_run_report_v1_with_reader(
    outcome: &DepthBacktestOutcome,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<ResearchRunReportV1, AppError> {
    let verification = verify_depth_run_with_reader(outcome, context, reader)?;
    let declared_artifacts = if verification.verified {
        let summary = read_json_bounded(
            reader,
            &outcome.artifacts.summary,
            MAX_SUMMARY_BYTES,
            "depth summary",
        )?;
        crate::artifact_store::declared_run_record_artifact_locators(&summary, reader)?
    } else {
        None
    };
    read_report(
        &outcome.run_id,
        &outcome.instrument,
        &outcome.artifacts.summary,
        &outcome.artifacts.equity,
        &outcome.artifacts.fills,
        verification,
        declared_artifacts.as_ref(),
        reader,
    )
}

pub fn read_provider_backtest_report_v1(
    outcome: &ProviderBacktestOutcome,
    context: &RunContext,
) -> Result<ProviderBacktestReportV1, AppError> {
    read_provider_backtest_report_v1_with_reader(outcome, context, &FileArtifactReader)
}

/// Read a Provider Bar run through the shared verifier and bind the outer Provider envelope to
/// the same canonical lineage and manifest embedded in the verified run summary.
pub fn read_provider_backtest_report_v1_with_reader(
    outcome: &ProviderBacktestOutcome,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<ProviderBacktestReportV1, AppError> {
    let report = read_run_report_v1_with_reader(&outcome.outcome, context, reader)?;
    let lineage = report
        .summary
        .get("input")
        .and_then(Value::as_object)
        .and_then(|input| input.get("provider_lineage"))
        .and_then(Value::as_object)
        .ok_or_else(|| provider_report_error("Provider lineage 在报告摘要中缺失"))?;
    let manifest_json = outcome.manifest.to_json().map_err(|error| {
        provider_report_error(&format!("Provider manifest 序列化失败: {error}"))
    })?;
    let expected_sha256 = qx_data::sha256_hex_bytes(manifest_json.as_bytes());
    if lineage.get("dataset_manifest_json").and_then(Value::as_str) != Some(&manifest_json)
        || lineage
            .get("dataset_manifest_sha256")
            .and_then(Value::as_str)
            != Some(expected_sha256.as_str())
        || lineage.get("provider_id").and_then(Value::as_str)
            != Some(outcome.manifest.source_lineage.primary_source.as_str())
        || lineage.get("provider_version").and_then(Value::as_str)
            != Some(outcome.manifest.provider_version.as_str())
        || outcome.manifest.instrument != outcome.outcome.instrument
        || outcome.dataset_verdict.instrument != outcome.outcome.instrument
        || outcome.dataset_verdict.content_fingerprint != outcome.outcome.data_fingerprint
        || outcome.manifest.dataset.fingerprint
            != outcome
                .outcome
                .data_fingerprint
                .strip_prefix("barframe:")
                .unwrap_or(outcome.outcome.data_fingerprint.as_str())
    {
        return Err(provider_report_error(
            "Provider outcome、冻结 manifest 与已验证回测输入身份不一致",
        ));
    }
    Ok(ProviderBacktestReportV1 {
        schema_version: PROVIDER_BACKTEST_REPORT_SCHEMA_VERSION_V1,
        report,
        dataset_verdict: outcome.dataset_verdict.clone(),
        manifest: outcome.manifest.clone(),
    })
}

pub fn read_provider_depth_backtest_report_v1(
    outcome: &ProviderDepthBacktestOutcome,
    context: &RunContext,
) -> Result<ProviderDepthBacktestReportV1, AppError> {
    read_provider_depth_backtest_report_v1_with_readers(
        outcome,
        context,
        &FileArtifactReader,
        &FileArtifactReader,
    )
}

/// Read a Provider depth report with separate readers for run artifacts and the frozen input.
/// This preserves rooted API storage boundaries, where report artifacts and dataset inputs may
/// live under different authorized roots.
pub fn read_provider_depth_backtest_report_v1_with_readers(
    outcome: &ProviderDepthBacktestOutcome,
    context: &RunContext,
    artifact_reader: &dyn ArtifactReader,
    input_reader: &dyn ArtifactReader,
) -> Result<ProviderDepthBacktestReportV1, AppError> {
    let report = read_depth_run_report_v1_with_reader(&outcome.outcome, context, artifact_reader)?;
    let body = read_text_bounded(
        input_reader,
        &outcome.depth_path,
        MAX_ARTIFACT_BYTES,
        "Provider DepthFrame",
    )?;
    let frame = qx_xingban::DepthFrame::from_json(&body)
        .map_err(|error| provider_report_error(&format!("Provider DepthFrame 无效: {error}")))?;
    let query_digest = verify_provider_depth_source(outcome, &frame.source)?;
    let input_hash = format!("{:016x}", frame.input_hash());
    let expected_fingerprint = format!("depth:{}:{}", frame.source, input_hash);
    let summary_input = report.summary.get("input");
    if frame.instrument.to_string() != outcome.outcome.instrument
        || outcome.outcome.data_fingerprint != expected_fingerprint
        || summary_input
            .and_then(|input| input.get("fingerprint"))
            .and_then(Value::as_str)
            != Some(input_hash.as_str())
        || summary_input
            .and_then(|input| input.get("kind"))
            .and_then(Value::as_str)
            != Some("depth-frame")
    {
        return Err(provider_report_error(
            "Provider DepthFrame、报告摘要与回测输入指纹不一致",
        ));
    }
    Ok(ProviderDepthBacktestReportV1 {
        schema_version: PROVIDER_DEPTH_BACKTEST_REPORT_SCHEMA_VERSION_V1,
        report,
        provider_id: outcome.provider_id.clone(),
        provider_version: outcome.provider_version.clone(),
        request_id: outcome.request_id.clone(),
        retry_chain: outcome.retry_chain.clone(),
        source_hash: outcome.source_hash.clone(),
        query_digest,
        depth_input_hash: input_hash,
    })
}

fn verify_provider_depth_source(
    outcome: &ProviderDepthBacktestOutcome,
    source: &str,
) -> Result<String, AppError> {
    let valid_hash = |value: &str| {
        value.len() == 16
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    if outcome.provider_id.trim().is_empty()
        || outcome.provider_version.trim().is_empty()
        || outcome.request_id.trim().is_empty()
        || !valid_hash(&outcome.source_hash)
    {
        return Err(provider_report_error("Provider Depth outcome 来源身份无效"));
    }
    let prefix = format!(
        "{}@{}#query=",
        outcome.provider_id, outcome.provider_version
    );
    let rest = source
        .strip_prefix(&prefix)
        .ok_or_else(|| provider_report_error("DepthFrame Provider 身份与 outcome 不一致"))?;
    let (query_digest, request_and_source) = rest
        .split_once("#request=")
        .ok_or_else(|| provider_report_error("DepthFrame 缺少 Provider query/request 身份"))?;
    if !valid_hash(query_digest)
        || request_and_source != format!("{}#source={}", outcome.request_id, outcome.source_hash)
    {
        return Err(provider_report_error(
            "DepthFrame Provider query/request/source 身份不一致",
        ));
    }
    Ok(query_digest.to_string())
}

fn provider_report_error(message: &str) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

/// Stream and replay a shared-account EventLog without materializing its JSON array.
/// The expected event count, digest and ledger are read from already-rooted run artifacts;
/// the requested page is the only event-row data retained after replay.
pub fn read_multi_market_event_report_v1_with_reader(
    reference: &str,
    expected_count: u64,
    expected_digest: &str,
    expected_ledger_entries: &Value,
    strategy_markets: &BTreeMap<String, String>,
    requested_page: Option<(u64, usize)>,
    reader: &dyn ArtifactReader,
) -> Result<VerifiedMultiMarketEventReportV1, AppError> {
    if expected_count > MAX_ARTIFACT_ROWS {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            "多市场摘要声明的事件数超过 500 万行上限",
        ));
    }
    let expected_count_usize = usize::try_from(expected_count)
        .map_err(|_| malformed_event_log("多市场摘要事件数超出本机索引范围"))?;
    let expected_digest = u64::from_str_radix(expected_digest, 16)
        .map_err(|_| malformed_event_log("RunManifest.output_event_hash 不是十六进制摘要"))?;
    if let Some((_, limit)) = requested_page {
        if limit == 0 || limit > MAX_EVENT_REPORT_PAGE_ROWS {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!("事件报告分页 limit 必须在 1 到 {MAX_EVENT_REPORT_PAGE_ROWS} 之间"),
            ));
        }
    }
    let page = requested_page.unwrap_or((
        expected_count.saturating_sub(MAX_EVENT_REPORT_PAGE_ROWS as u64),
        MAX_EVENT_REPORT_PAGE_ROWS,
    ));
    let offset = page.0.min(expected_count);
    let end = offset.saturating_add(page.1 as u64).min(expected_count);
    let mut rows = Vec::with_capacity(usize::try_from(end - offset).unwrap_or(0));
    let mut fill_count = 0_u64;
    let mut fill_digest = qx_strategy::file_digest::Sha256Accumulator::new();
    fill_digest.update(b"qx-event-fill-sequence-v1\0");
    let mut trace_bytes_digest = qx_strategy::file_digest::Sha256Accumulator::new();
    trace_bytes_digest.update(b"qx-multi-market-event-trace-bytes-v1\0");
    let mut replay = qx_core::StreamingReplayVerifier::new(expected_count_usize);
    let mut artifact = reader.open(reference)?;
    if artifact.byte_len > MAX_ARTIFACT_BYTES {
        return Err(artifact_too_large(reference, MAX_ARTIFACT_BYTES));
    }
    let mut bytes_seen = 0_u64;
    let mut total = 0_u64;
    while let Some((line, line_bytes)) = read_bounded_line(artifact.reader.as_mut(), reference)? {
        bytes_seen = bytes_seen.saturating_add(line_bytes as u64);
        trace_bytes_digest.update(line.as_bytes());
        if bytes_seen > MAX_ARTIFACT_BYTES {
            return Err(artifact_too_large(reference, MAX_ARTIFACT_BYTES));
        }
        if line.trim().is_empty() {
            continue;
        }
        total = total.saturating_add(1);
        if total > MAX_ARTIFACT_ROWS {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                "多市场事件轨迹超过 500 万行上限",
            ));
        }
        let event: qx_core::Event = serde_json::from_str(line.trim_end()).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("多市场事件轨迹第 {total} 行格式损坏: {error}"),
            )
        })?;
        if let qx_core::EventKind::Filled { fill } = &event.kind {
            let identity = event_fill_identity(fill);
            update_fill_sequence_digest(&mut fill_digest, &identity)?;
            fill_count = fill_count
                .checked_add(1)
                .ok_or_else(|| malformed_event_log("多市场成交事件计数溢出"))?;
        }
        let event_index = total - 1;
        if event_index >= offset && event_index < end {
            let event_value = serde_json::to_value(&event).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("多市场事件行投影失败: {error}"),
                )
            })?;
            rows.push(event_value);
        }
        replay.append(event).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("多市场事件轨迹重放失败: {error}"),
            )
        })?;
    }
    let replayed = replay.finish().map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("多市场事件轨迹重放失败: {error}"),
        )
    })?;
    if total != expected_count {
        return Err(malformed_event_log(
            "多市场事件轨迹行数与产物 event_count 不一致",
        ));
    }
    if replayed.log_digest != expected_digest {
        return Err(malformed_event_log(
            "多市场事件轨迹摘要与 RunManifest.output_event_hash 不一致",
        ));
    }
    let replayed_ledger_entries =
        serde_json::to_value(replayed.ledger.entries()).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("多市场重放账簿序列化失败: {error}"),
            )
        })?;
    if replayed_ledger_entries != *expected_ledger_entries {
        return Err(malformed_event_log(
            "多市场事件轨迹重放账簿与产物 ledger_entries 不一致",
        ));
    }
    let unresolved_page_order_ids = rows
        .iter()
        .filter(|event| {
            event_market_instrument(event, strategy_markets, &BTreeMap::new()).is_none()
        })
        .filter_map(event_order_id)
        .collect::<BTreeSet<_>>();
    let order_markets = if unresolved_page_order_ids.is_empty() {
        BTreeMap::new()
    } else {
        read_multi_market_order_market_index_v1(
            reference,
            &trace_bytes_digest.finish_hex(),
            &unresolved_page_order_ids,
            strategy_markets,
            reader,
        )?
    };
    Ok(VerifiedMultiMarketEventReportV1 {
        total,
        offset,
        truncated: rows.len() < total as usize,
        rows,
        fill_count,
        fill_sequence_sha256: fill_digest.finish_hex(),
        order_markets,
        ledger: replayed.ledger,
    })
}

/// Read a bounded multi-market EventLog page through the caller's artifact port and return the
/// language-neutral JSON core. The complete trace is replayed and checked before any page is
/// returned; only the requested rows are retained in the result.
pub fn read_multi_market_event_report_page_v1_with_reader(
    request: &MultiMarketEventReportReadRequestV1,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<MultiMarketEventReportPageV1, AppError> {
    read_multi_market_event_report_read_model_v1_with_reader(request, context, reader)
        .map(|model| model.page)
}

/// Verify a multi-market report once and retain the bounded page plus verified inputs used by
/// an application adapter to add its own authorized data/context projection.
pub fn read_multi_market_event_report_read_model_v1_with_reader(
    request: &MultiMarketEventReportReadRequestV1,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<MultiMarketEventReportReadModelV1, AppError> {
    context.require(CallerCapability::Research, "多市场事件报告读取")?;
    if request.schema_version != MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1 {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "多市场事件报告读取请求 schema_version 不受支持",
        ));
    }
    let outcome = &request.outcome;
    if outcome.run_id.trim().is_empty() || outcome.artifact_path.trim().is_empty() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "多市场事件报告读取请求缺少有效 outcome 身份",
        ));
    }
    let artifact_text = read_text_bounded(
        reader,
        &outcome.artifact_path,
        MAX_ARTIFACT_BYTES,
        "多市场回测产物",
    )?;
    let artifact: Value = serde_json::from_str(&artifact_text).map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场回测产物 JSON 无效: {error}"),
        )
    })?;
    let artifact_version = artifact.get("schema_version").and_then(Value::as_u64);
    if !matches!(artifact_version, Some(1 | 2)) {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场回测产物 schema_version 不受支持",
        ));
    }
    let embedded_outcome: MultiMarketBacktestOutcome = serde_json::from_value(
        artifact
            .get("outcome")
            .cloned()
            .ok_or_else(|| AppError::new(AppErrorCategory::Conflict, "多市场产物缺少 outcome"))?,
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场产物 outcome 无效: {error}"),
        )
    })?;
    if embedded_outcome != *outcome {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "请求 outcome 与多市场回测产物身份不一致",
        ));
    }
    let spec: MultiMarketBacktestSpec = serde_json::from_value(
        artifact
            .get("spec")
            .cloned()
            .ok_or_else(|| AppError::new(AppErrorCategory::Conflict, "多市场产物缺少 spec"))?,
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场产物 spec 无效: {error}"),
        )
    })?;
    spec.validate()?;
    let strategies_total = spec.markets.iter().try_fold(0_usize, |total, market| {
        total
            .checked_add(market.strategies.len())
            .ok_or_else(|| AppError::new(AppErrorCategory::Conflict, "多市场策略总数溢出"))
    })?;
    if spec.run_id != outcome.run_id
        || spec.account_id != outcome.account_id
        || spec.markets.len() != outcome.markets_total
        || strategies_total != outcome.strategies_total
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场 spec 与 outcome 摘要不一致",
        ));
    }
    let strategy_identities: Vec<StrategyArtifactIdentity> = serde_json::from_value(
        artifact
            .get("strategy_identities")
            .cloned()
            .ok_or_else(|| {
                AppError::new(AppErrorCategory::Conflict, "多市场产物缺少策略身份列表")
            })?,
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场策略身份列表无效: {error}"),
        )
    })?;
    let spec_fingerprint = crate::multi_market_spec_fingerprint_v1(&spec, &strategy_identities)?;
    if spec_fingerprint != outcome.spec_fingerprint {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场 spec fingerprint 与 outcome 不一致",
        ));
    }
    let manifest_path = artifact
        .get("run_manifest_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AppError::new(
                AppErrorCategory::Conflict,
                "多市场产物缺少 RunManifest 路径",
            )
        })?;
    let manifest_text = read_text_bounded(
        reader,
        manifest_path,
        MAX_ARTIFACT_BYTES,
        "多市场 RunManifest",
    )?;
    let manifest = qx_core::RunManifest::from_json(&manifest_text).map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场 RunManifest 无效: {error:?}"),
        )
    })?;
    crate::artifact_store::verify_canonical_run_spec_for_manifest(&manifest, &artifact).map_err(
        |error| {
            AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场 CanonicalRunSpec 复核失败: {error}"),
            )
        },
    )?;
    if manifest.run_id != outcome.run_id
        || manifest.data_fingerprint != outcome.data_fingerprint
        || manifest.result_hash != outcome.shared_result_hash
        || manifest.input_event_hash != outcome.data_fingerprint
        || manifest.strategy_version != format!("multi-market-strategy-set:{spec_fingerprint}")
        || manifest.global_seed != spec.seed
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场 RunManifest 与 outcome/spec 身份不一致",
        ));
    }
    let expected_ledger_entries = artifact
        .get("ledger_entries")
        .filter(|entries| entries.is_array())
        .ok_or_else(|| {
            AppError::new(
                AppErrorCategory::Conflict,
                "多市场产物 ledger_entries 必须是数组",
            )
        })?;
    if expected_ledger_entries
        .as_array()
        .is_some_and(|entries| entries.len() > MAX_ARTIFACT_ROWS as usize)
    {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            "多市场产物 ledger_entries 超过行数上限",
        ));
    }
    let mut strategy_markets = BTreeMap::new();
    for market in &spec.markets {
        for strategy in &market.strategies {
            if strategy_markets
                .insert(
                    strategy.strategy.strategy_id.clone(),
                    market.instrument.clone(),
                )
                .is_some()
            {
                return Err(AppError::new(
                    AppErrorCategory::Conflict,
                    "多市场策略标识重复，不能唯一归属事件标的",
                ));
            }
        }
    }
    let page = request.page.map(|page| (page.offset, page.limit));
    let report = if artifact_version == Some(2) {
        if artifact.get("event_log").is_some() {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                "多市场 v2 产物不得在 JSON 中嵌入完整 event_log",
            ));
        }
        let event_trace_path = artifact
            .get("event_trace_path")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::Conflict,
                    "多市场 v2 产物缺少 event_trace_path",
                )
            })?;
        let expected_count = artifact
            .get("event_count")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                AppError::new(AppErrorCategory::Conflict, "多市场 v2 产物缺少 event_count")
            })?;
        if page.is_some_and(|(offset, _)| offset > expected_count) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "事件报告分页 offset 超出多市场事件流范围",
            ));
        }
        read_multi_market_event_report_v1_with_reader(
            event_trace_path,
            expected_count,
            &manifest.output_event_hash,
            expected_ledger_entries,
            &strategy_markets,
            page,
            reader,
        )?
    } else {
        let events = artifact
            .get("event_log")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                AppError::new(AppErrorCategory::Conflict, "多市场 v1 产物缺少 event_log")
            })?;
        read_multi_market_legacy_event_report_v1(
            events,
            &manifest.output_event_hash,
            expected_ledger_entries,
            &strategy_markets,
            page,
        )?
    };
    if report.fill_count != outcome.successful_fills {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场 Filled 事件数与 outcome 成交数不一致",
        ));
    }
    let replayed_ledger_entries = u64::try_from(report.ledger.entries().len()).map_err(|_| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            "多市场重放账簿条目数超出 u64 范围",
        )
    })?;
    let page = MultiMarketEventReportPageV1 {
        schema_version: MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1,
        run_id: outcome.run_id.clone(),
        total: report.total,
        offset: report.offset,
        truncated: report.truncated,
        rows: report.rows,
        fill_count: report.fill_count,
        fill_sequence_sha256: report.fill_sequence_sha256,
        order_markets: report.order_markets,
        replayed_ledger_entries,
    };
    Ok(MultiMarketEventReportReadModelV1 {
        artifact,
        spec,
        manifest,
        page,
        ledger: report.ledger,
    })
}

/// Local-file convenience wrapper around [`read_multi_market_event_report_page_v1_with_reader`].
pub fn read_multi_market_event_report_page_v1(
    request: &MultiMarketEventReportReadRequestV1,
    context: &RunContext,
) -> Result<MultiMarketEventReportPageV1, AppError> {
    read_multi_market_event_report_page_v1_with_reader(request, context, &FileArtifactReader)
}

/// Read a replay-verified multi-market Depth page using local artifact paths.
pub fn read_multi_market_depth_event_report_page_v1(
    request: &MultiMarketDepthEventReportReadRequestV1,
    context: &RunContext,
) -> Result<MultiMarketEventReportPageV1, AppError> {
    read_multi_market_depth_event_report_page_v1_with_reader(request, context, &FileArtifactReader)
}

/// Read and verify the identity-bearing multi-market Depth report inputs through an authorized
/// artifact port. The complete event stream is replayed before the bounded page is returned.
pub fn read_multi_market_depth_event_report_page_v1_with_reader(
    request: &MultiMarketDepthEventReportReadRequestV1,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<MultiMarketEventReportPageV1, AppError> {
    read_multi_market_depth_event_report_read_model_v1_with_reader(request, context, reader)
        .map(|model| model.page)
}

/// Verify a multi-market Depth artifact once and retain its bounded page plus the verified
/// artifact/spec/manifest used by an API adapter to add authorized market-data projections.
pub fn read_multi_market_depth_event_report_read_model_v1_with_reader(
    request: &MultiMarketDepthEventReportReadRequestV1,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<MultiMarketDepthEventReportReadModelV1, AppError> {
    context.require(CallerCapability::Research, "多市场深度事件报告读取")?;
    if request.schema_version != MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1 {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "多市场深度事件报告读取请求 schema_version 不受支持",
        ));
    }
    let outcome = &request.outcome;
    if outcome.run_id.trim().is_empty() || outcome.artifact_path.trim().is_empty() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "多市场深度事件报告读取请求缺少有效 outcome 身份",
        ));
    }
    let artifact_text = read_text_bounded(
        reader,
        &outcome.artifact_path,
        MAX_ARTIFACT_BYTES,
        "多市场深度回测产物",
    )?;
    let artifact: Value = serde_json::from_str(&artifact_text).map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场深度回测产物 JSON 无效: {error}"),
        )
    })?;
    let artifact_version = artifact.get("schema_version").and_then(Value::as_u64);
    if !matches!(artifact_version, Some(1 | 2 | 3 | 4)) {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场深度产物 schema_version 不受支持",
        ));
    }
    let embedded_outcome: MultiMarketDepthBacktestOutcome =
        serde_json::from_value(artifact.get("outcome").cloned().ok_or_else(|| {
            AppError::new(AppErrorCategory::Conflict, "多市场深度产物缺少 outcome")
        })?)
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场深度产物 outcome 无效: {error}"),
            )
        })?;
    if embedded_outcome != *outcome {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "请求 outcome 与多市场深度产物身份不一致",
        ));
    }
    let spec: MultiMarketDepthBacktestSpec = serde_json::from_value(
        artifact
            .get("spec")
            .cloned()
            .ok_or_else(|| AppError::new(AppErrorCategory::Conflict, "多市场深度产物缺少 spec"))?,
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场深度 spec 无效: {error}"),
        )
    })?;
    spec.validate()?;
    if spec.run_id != outcome.run_id
        || spec.account_id != outcome.account_id
        || spec.markets.len() != outcome.markets_total
        || spec.markets.len() > crate::MAX_MULTI_MARKET_DEPTH_MARKETS
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场深度 spec 与 outcome 摘要不一致",
        ));
    }
    let strategy_identities: Vec<StrategyArtifactIdentity> = serde_json::from_value(
        artifact
            .get("strategy_identities")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场深度策略身份列表无效: {error}"),
        )
    })?;
    let spec_fingerprint =
        crate::multi_market_depth_spec_fingerprint_v1(&spec, &strategy_identities)?;
    if spec_fingerprint != outcome.spec_fingerprint {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场深度 spec fingerprint 与 outcome 不一致",
        ));
    }
    let mut identity_spec = spec.clone();
    identity_spec.output_dir.clear();
    for market in &mut identity_spec.markets {
        market.depth_path.clear();
        market.dataset_manifest_path = None;
    }
    let config_bytes = serde_json::to_vec(&identity_spec).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多市场深度配置编码失败: {error}"),
        )
    })?;
    let config_hash = qx_strategy::sha256_hex(&config_bytes);
    let manifest_path = artifact
        .get("run_manifest_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AppError::new(
                AppErrorCategory::Conflict,
                "多市场深度产物缺少 RunManifest 路径",
            )
        })?;
    let manifest_text = read_text_bounded(
        reader,
        manifest_path,
        MAX_ARTIFACT_BYTES,
        "多市场深度 RunManifest",
    )?;
    let manifest = qx_core::RunManifest::from_json(&manifest_text).map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场深度 RunManifest 无效: {error:?}"),
        )
    })?;
    crate::artifact_store::verify_canonical_run_spec_for_manifest(&manifest, &artifact).map_err(
        |error| {
            AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场深度 CanonicalRunSpec 复核失败: {error}"),
            )
        },
    )?;
    if manifest.run_id != outcome.run_id
        || manifest.config_hash != config_hash
        || manifest.data_fingerprint != outcome.data_fingerprint
        || manifest.result_hash != outcome.shared_result_hash
        || manifest.input_event_hash != outcome.data_fingerprint
        || manifest.strategy_version != format!("multi-market-depth:{spec_fingerprint}")
        || manifest.instrument_spec_version != "multi-market-depth-spec-v1"
        || manifest.model_fingerprint
            != qx_strategy::sha256_hex(
                b"OrderBookPortfolioBacktestEngine/OrderBookExecutionModel/v1",
            )
        || manifest.global_seed != 0
        || !manifest.determinism_mode
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场深度 RunManifest 与 outcome/spec 身份不一致",
        ));
    }
    if !outcome.execution_models.is_empty() {
        let declared = spec
            .markets
            .iter()
            .map(|market| {
                Ok((
                    market.instrument.clone(),
                    market.execution_model()?.audit_descriptor(),
                ))
            })
            .collect::<Result<BTreeMap<_, _>, AppError>>()?;
        if outcome.execution_models != declared {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                "多市场深度 outcome 生效撮合参数与 spec 不一致",
            ));
        }
    }
    let expected_ledger_entries = artifact
        .get("ledger_entries")
        .filter(|entries| entries.is_array())
        .ok_or_else(|| {
            AppError::new(
                AppErrorCategory::Conflict,
                "多市场深度产物 ledger_entries 必须是数组",
            )
        })?;
    if expected_ledger_entries
        .as_array()
        .is_some_and(|entries| entries.len() > MAX_ARTIFACT_ROWS as usize)
    {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            "多市场深度产物 ledger_entries 超过行数上限",
        ));
    }
    let mut strategy_markets = BTreeMap::new();
    for market in &spec.markets {
        if strategy_markets
            .insert(
                market.strategy.strategy_id.clone(),
                market.instrument.clone(),
            )
            .is_some()
        {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                "多市场深度策略标识重复，不能唯一归属事件标的",
            ));
        }
    }
    let requested_page = request.page.map(|page| (page.offset, page.limit));
    let report = if matches!(artifact_version, Some(2 | 3 | 4)) {
        if artifact.get("event_log").is_some() {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                "多市场深度 v2 产物不得在 JSON 中嵌入完整 event_log",
            ));
        }
        let event_trace_path = artifact
            .get("event_trace_path")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::Conflict,
                    "多市场深度 v2 产物缺少 event_trace_path",
                )
            })?;
        let expected_count = artifact
            .get("event_count")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::Conflict,
                    "多市场深度 v2 产物缺少 event_count",
                )
            })?;
        if requested_page.is_some_and(|(offset, _)| offset > expected_count) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "事件报告分页 offset 超出多市场深度事件流范围",
            ));
        }
        read_multi_market_event_report_v1_with_reader(
            event_trace_path,
            expected_count,
            &manifest.output_event_hash,
            expected_ledger_entries,
            &strategy_markets,
            requested_page,
            reader,
        )?
    } else {
        let events = artifact
            .get("event_log")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::Conflict,
                    "多市场深度 v1 产物缺少 event_log",
                )
            })?;
        read_multi_market_legacy_event_report_v1(
            events,
            &manifest.output_event_hash,
            expected_ledger_entries,
            &strategy_markets,
            requested_page,
        )?
    };
    if report.fill_count != outcome.successful_fills {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场深度 Filled 事件数与 outcome 成交数不一致",
        ));
    }
    let replayed_ledger_entries = u64::try_from(report.ledger.entries().len()).map_err(|_| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            "多市场深度重放账簿条目数超出 u64 范围",
        )
    })?;
    let page = MultiMarketEventReportPageV1 {
        schema_version: MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1,
        run_id: outcome.run_id.clone(),
        total: report.total,
        offset: report.offset,
        truncated: report.truncated,
        rows: report.rows,
        fill_count: report.fill_count,
        fill_sequence_sha256: report.fill_sequence_sha256,
        order_markets: report.order_markets,
        replayed_ledger_entries,
    };
    Ok(MultiMarketDepthEventReportReadModelV1 {
        artifact,
        spec,
        manifest,
        page,
    })
}

/// Verify and page a legacy multi-market artifact whose EventLog is embedded in its JSON value.
///
/// Newer artifacts store an event trace separately and use
/// [`read_multi_market_event_report_v1_with_reader`]. This compatibility reader keeps the v1
/// event replay, fill identity and order-to-market facts in the same application layer.
pub fn read_multi_market_legacy_event_report_v1(
    events: &[Value],
    expected_digest: &str,
    expected_ledger_entries: &Value,
    strategy_markets: &BTreeMap<String, String>,
    requested_page: Option<(u64, usize)>,
) -> Result<VerifiedMultiMarketEventReportV1, AppError> {
    if events.len() as u64 > MAX_ARTIFACT_ROWS {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            "多市场内嵌事件流超过 500 万行上限",
        ));
    }
    let total = u64::try_from(events.len())
        .map_err(|_| malformed_event_log("多市场内嵌事件数超出 u64 范围"))?;
    if let Some((_, limit)) = requested_page {
        if limit == 0 || limit > MAX_EVENT_REPORT_PAGE_ROWS {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!("事件报告分页 limit 必须在 1 到 {MAX_EVENT_REPORT_PAGE_ROWS} 之间"),
            ));
        }
    }
    let page = requested_page.unwrap_or((
        total.saturating_sub(MAX_EVENT_REPORT_PAGE_ROWS as u64),
        MAX_EVENT_REPORT_PAGE_ROWS,
    ));
    if page.0 > total {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "事件报告分页 offset 超出多市场事件流范围",
        ));
    }
    let offset = page.0;
    let end = offset.saturating_add(page.1 as u64).min(total);
    let start_index = usize::try_from(offset)
        .map_err(|_| malformed_event_log("多市场事件分页 offset 超出本机索引范围"))?;
    let end_index = usize::try_from(end)
        .map_err(|_| malformed_event_log("多市场事件分页终点超出本机索引范围"))?;
    let rows = events[start_index..end_index].to_vec();
    let replay = crate::verify_event_replay_v1(events, expected_digest, expected_ledger_entries)?;
    let mut fill_count = 0_u64;
    let mut fill_digest = qx_strategy::file_digest::Sha256Accumulator::new();
    fill_digest.update(b"qx-event-fill-sequence-v1\0");
    let mut order_markets = BTreeMap::<u64, String>::new();
    for event in events {
        if let Some(fill_value) = event.pointer("/kind/Filled/fill") {
            let fill: qx_core::Fill =
                serde_json::from_value(fill_value.clone()).map_err(|error| {
                    let message = format!("多市场成交事件结构无效: {error}");
                    malformed_event_log(&message)
                })?;
            update_fill_sequence_digest(&mut fill_digest, &event_fill_identity(&fill))?;
            fill_count = fill_count
                .checked_add(1)
                .ok_or_else(|| malformed_event_log("多市场成交事件计数溢出"))?;
        }
        let (Some(order_id), Some(instrument)) = (
            event_order_id(event),
            event_market_instrument(event, strategy_markets, &BTreeMap::new()),
        ) else {
            continue;
        };
        if order_markets
            .get(&order_id)
            .is_some_and(|existing| existing != &instrument)
        {
            return Err(malformed_event_log(
                "同一订单在多市场内嵌事件流中关联到不同标的",
            ));
        }
        order_markets.insert(order_id, instrument);
    }
    Ok(VerifiedMultiMarketEventReportV1 {
        total,
        offset,
        truncated: (rows.len() as u64) < total,
        rows,
        fill_count,
        fill_sequence_sha256: fill_digest.finish_hex(),
        order_markets,
        ledger: replay.ledger,
    })
}

/// Recover instrument attribution only for orders referenced by the requested event page.
///
/// The first pass already verifies replay and collects that bounded page. This second pass
/// builds a page-sized order index, then checks that the trace bytes did not change between
/// reads. The returned index therefore grows with the requested page, not the full run.
fn read_multi_market_order_market_index_v1(
    reference: &str,
    expected_trace_sha256: &str,
    required_order_ids: &BTreeSet<u64>,
    strategy_markets: &BTreeMap<String, String>,
    reader: &dyn ArtifactReader,
) -> Result<BTreeMap<u64, String>, AppError> {
    let mut artifact = reader.open(reference)?;
    if artifact.byte_len > MAX_ARTIFACT_BYTES {
        return Err(artifact_too_large(reference, MAX_ARTIFACT_BYTES));
    }
    let mut bytes_seen = 0_u64;
    let mut total = 0_u64;
    let mut digest = qx_strategy::file_digest::Sha256Accumulator::new();
    digest.update(b"qx-multi-market-event-trace-bytes-v1\0");
    let mut order_markets = BTreeMap::<u64, String>::new();
    while let Some((line, line_bytes)) = read_bounded_line(artifact.reader.as_mut(), reference)? {
        bytes_seen = bytes_seen
            .checked_add(line_bytes as u64)
            .ok_or_else(|| malformed_event_log("多市场事件轨迹字节数溢出"))?;
        if bytes_seen > MAX_ARTIFACT_BYTES {
            return Err(artifact_too_large(reference, MAX_ARTIFACT_BYTES));
        }
        digest.update(line.as_bytes());
        if line.trim().is_empty() {
            continue;
        }
        total = total
            .checked_add(1)
            .ok_or_else(|| malformed_event_log("多市场事件轨迹行数溢出"))?;
        if total > MAX_ARTIFACT_ROWS {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                "多市场事件轨迹超过 500 万行上限",
            ));
        }
        let event: qx_core::Event = serde_json::from_str(line.trim_end()).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("多市场事件轨迹第 {total} 行格式损坏: {error}"),
            )
        })?;
        let Some(order_id) =
            event_order_id_typed(&event).filter(|order_id| required_order_ids.contains(order_id))
        else {
            continue;
        };
        let event_value = serde_json::to_value(&event).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("多市场事件行投影失败: {error}"),
            )
        })?;
        let Some(instrument) =
            event_market_instrument(&event_value, strategy_markets, &BTreeMap::new())
        else {
            continue;
        };
        if order_markets
            .get(&order_id)
            .is_some_and(|existing| existing != &instrument)
        {
            return Err(malformed_event_log(
                "同一订单在多市场事件轨迹中关联到不同标的",
            ));
        }
        order_markets.insert(order_id, instrument);
    }
    if digest.finish_hex() != expected_trace_sha256 {
        return Err(malformed_event_log(
            "多市场事件轨迹在分页归属复核期间发生变化",
        ));
    }
    Ok(order_markets)
}

fn event_order_id_typed(event: &qx_core::Event) -> Option<u64> {
    match &event.kind {
        qx_core::EventKind::OrderRiskDecision { decision } => Some(decision.client_order_id),
        qx_core::EventKind::OrderSubmitted { order } => Some(order.client_id),
        qx_core::EventKind::Submit { client_order_id }
        | qx_core::EventKind::Accepted {
            client_order_id, ..
        }
        | qx_core::EventKind::Rejected {
            client_order_id, ..
        }
        | qx_core::EventKind::CancelPending { client_order_id }
        | qx_core::EventKind::Cancelled { client_order_id }
        | qx_core::EventKind::ReconcileRequired { client_order_id } => Some(*client_order_id),
        qx_core::EventKind::Filled { fill } => Some(fill.order_id),
        qx_core::EventKind::LedgerApplied { entry } => entry.order_id,
        _ => None,
    }
}

fn event_market_instrument(
    event: &Value,
    strategy_markets: &BTreeMap<String, String>,
    order_markets: &BTreeMap<u64, String>,
) -> Option<String> {
    let kind = event.get("kind")?.as_object()?;
    if let Some(intents) = kind
        .get("StrategyDecision")
        .and_then(|event| event.get("decision"))
        .and_then(|decision| decision.get("intents"))
        .and_then(Value::as_array)
    {
        let intent_markets = intents
            .iter()
            .filter_map(|intent| intent.get("instrument").and_then(canonical_instrument))
            .collect::<std::collections::BTreeSet<_>>();
        if intent_markets.len() > 1 {
            return None;
        }
        if let Some(instrument) = intent_markets.into_iter().next() {
            return Some(instrument);
        }
    }
    let instrument = kind
        .get("OrderRiskDecision")
        .and_then(|event| event.get("decision"))
        .and_then(|decision| decision.get("instrument"))
        .or_else(|| {
            kind.get("OrderSubmitted")
                .and_then(|event| event.get("order"))
                .and_then(|order| order.get("instrument"))
        })
        .or_else(|| {
            kind.get("Filled")
                .and_then(|event| event.get("fill"))
                .and_then(|fill| fill.get("instrument"))
        })
        .or_else(|| {
            kind.get("LedgerApplied")
                .and_then(|event| event.get("entry"))
                .and_then(|entry| entry.get("instrument"))
        })
        .or_else(|| kind.values().find_map(|payload| payload.get("instrument")));
    if let Some(instrument) = instrument.and_then(canonical_instrument) {
        return Some(instrument);
    }
    if let Some(instrument) =
        event_order_id(event).and_then(|order_id| order_markets.get(&order_id))
    {
        return Some(instrument.clone());
    }
    let strategy_id = kind
        .get("OrderRiskDecision")
        .and_then(|event| event.get("decision"))
        .and_then(|decision| decision.get("strategy_id"))
        .and_then(Value::as_str)
        .or_else(|| {
            kind.get("StrategyDecision")
                .and_then(|event| event.get("decision"))
                .and_then(|decision| decision.get("strategy_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            event
                .pointer("/metadata/context/strategy_id")
                .and_then(Value::as_str)
        })?;
    strategy_markets.get(strategy_id).cloned()
}

fn canonical_instrument(instrument: &Value) -> Option<String> {
    serde_json::from_value::<qx_core::InstrumentId>(instrument.clone())
        .ok()
        .map(|instrument| instrument.to_string())
        .or_else(|| instrument.as_str().map(str::to_owned))
}

fn event_order_id(event: &Value) -> Option<u64> {
    let kind = event.get("kind")?.as_object()?;
    kind.get("OrderRiskDecision")
        .and_then(|event| event.get("decision"))
        .and_then(|decision| decision.get("client_order_id"))
        .or_else(|| {
            kind.get("OrderSubmitted")
                .and_then(|event| event.get("order"))
                .and_then(|order| order.get("client_id"))
        })
        .or_else(|| {
            kind.get("Filled")
                .and_then(|event| event.get("fill"))
                .and_then(|fill| fill.get("order_id"))
        })
        .or_else(|| {
            kind.get("LedgerApplied")
                .and_then(|event| event.get("entry"))
                .and_then(|entry| entry.get("order_id"))
        })
        .or_else(|| {
            [
                "Submit",
                "Accepted",
                "Rejected",
                "Cancelled",
                "ReconcileRequired",
            ]
            .iter()
            .find_map(|variant| {
                kind.get(*variant)
                    .and_then(|event| event.get("client_order_id"))
            })
        })
        .and_then(Value::as_u64)
}

/// Read a bounded EventLog, replay it, and return a verified page plus its complete fill
/// multiset. Storage authorization is delegated to the caller's `ArtifactReader`; replay and
/// summary/manifest binding remain application responsibilities.
pub fn read_event_report_v1_with_reader(
    reference: &str,
    summary: &Value,
    manifest_reference: &str,
    requested_page: Option<(u64, usize)>,
    reader: &dyn ArtifactReader,
) -> Result<EventReportPageV1, AppError> {
    if let Some((_, limit)) = requested_page {
        if limit == 0 || limit > MAX_EVENT_REPORT_PAGE_ROWS {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!("事件报告分页 limit 必须在 1 到 {MAX_EVENT_REPORT_PAGE_ROWS} 之间"),
            ));
        }
    }
    let manifest = read_json_bounded(reader, manifest_reference, MAX_SUMMARY_BYTES, "RunManifest")?;
    let expected_digest = summary
        .pointer("/replay/log_digest")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed_event_log("摘要缺少 replay.log_digest，无法验证事件轨迹"))?;
    let expected_digest = u64::from_str_radix(expected_digest, 16)
        .map_err(|_| malformed_event_log("摘要 replay.log_digest 不是十六进制摘要"))?;
    let expected_count = summary
        .pointer("/replay/events")
        .and_then(Value::as_u64)
        .ok_or_else(|| malformed_event_log("摘要缺少 replay.events，无法验证事件数量"))?;
    if expected_count > MAX_ARTIFACT_ROWS {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            "摘要声明的事件数超过 500 万行上限",
        ));
    }
    let expected_count_usize = usize::try_from(expected_count)
        .map_err(|_| malformed_event_log("摘要事件数超出本机索引范围"))?;
    let page = requested_page.unwrap_or((
        expected_count.saturating_sub(MAX_EVENT_REPORT_PAGE_ROWS as u64),
        MAX_EVENT_REPORT_PAGE_ROWS,
    ));
    let offset = page.0.min(expected_count);
    let end = offset.saturating_add(page.1 as u64).min(expected_count);
    let mut rows = Vec::with_capacity(usize::try_from(end - offset).unwrap_or(0));
    let mut fill_counts = std::collections::BTreeMap::<EventFillIdentityV1, usize>::new();
    let mut replay = qx_core::StreamingReplayVerifier::new(expected_count_usize);
    let mut artifact = reader.open(reference)?;
    if artifact.byte_len > MAX_ARTIFACT_BYTES {
        return Err(artifact_too_large(reference, MAX_ARTIFACT_BYTES));
    }
    let mut bytes_seen = 0_u64;
    let mut total = 0_u64;
    while let Some((line, line_bytes)) = read_bounded_line(artifact.reader.as_mut(), reference)? {
        bytes_seen = bytes_seen.saturating_add(line_bytes as u64);
        if bytes_seen > MAX_ARTIFACT_BYTES {
            return Err(artifact_too_large(reference, MAX_ARTIFACT_BYTES));
        }
        if line.trim().is_empty() {
            continue;
        }
        total = total.saturating_add(1);
        if total > MAX_ARTIFACT_ROWS {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                "事件轨迹超过 500 万行上限",
            ));
        }
        let event: qx_core::Event = serde_json::from_str(line.trim_end()).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("事件轨迹第 {total} 行格式损坏: {error}"),
            )
        })?;
        if let qx_core::EventKind::Filled { fill } = &event.kind {
            let identity = EventFillIdentityV1 {
                ts: fill.ts,
                order_id: fill.order_id,
                qty_raw: fill.qty.raw(),
                price_raw: fill.price.raw(),
                fee_raw: fill.fee.raw(),
                strategy_id: fill.strategy_id.clone().unwrap_or_default(),
            };
            *fill_counts.entry(identity).or_default() += 1;
        }
        let event_index = total - 1;
        if event_index >= offset && event_index < end {
            rows.push(serde_json::to_value(&event).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("事件轨迹视图序列化失败: {error}"),
                )
            })?);
        }
        replay.append(event).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("事件轨迹重放失败: {error}"),
            )
        })?;
    }
    let replayed = replay.finish().map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("事件轨迹重放失败: {error}"),
        )
    })?;
    if total != expected_count {
        return Err(malformed_event_log(
            "事件轨迹行数与 summary.replay.events 不一致",
        ));
    }
    if replayed.log_digest != expected_digest {
        return Err(malformed_event_log(
            "事件轨迹摘要与 summary.replay.log_digest 不一致",
        ));
    }
    let expected_manifest_digest = format!("{:016x}", replayed.log_digest);
    if manifest
        .pointer("/output_event_hash")
        .and_then(Value::as_str)
        != Some(expected_manifest_digest.as_str())
    {
        return Err(malformed_event_log(
            "RunManifest.output_event_hash 与 replay-verified 事件轨迹不一致",
        ));
    }
    let replayed_ledger_count = replayed.ledger.entries().len() as u64;
    for (path, expected) in [
        ("/ledger_entries", summary.pointer("/ledger_entries")),
        (
            "/replay_ledger_entries",
            summary.pointer("/replay_ledger_entries"),
        ),
        (
            "/replay/ledger_entries",
            summary.pointer("/replay/ledger_entries"),
        ),
        (
            "/replay/run_ledger_entries",
            summary.pointer("/replay/run_ledger_entries"),
        ),
    ] {
        if expected.and_then(Value::as_u64) != Some(replayed_ledger_count) {
            return Err(malformed_event_log(&format!(
                "summary{path} 与重放重建账簿条数不一致"
            )));
        }
    }

    Ok(EventReportPageV1 {
        schema_version: EVENT_REPORT_PAGE_SCHEMA_VERSION,
        total,
        offset,
        truncated: rows.len() < total as usize,
        rows,
        fill_counts: fill_counts
            .into_iter()
            .map(|(identity, count)| EventFillCountV1 { identity, count })
            .collect(),
    })
}

fn read_report(
    run_id: &str,
    instrument: &str,
    summary_path: &str,
    equity_path: &str,
    fills_path: &str,
    verification: VerificationResult,
    declared_artifacts: Option<&RunRecordArtifactLocators>,
    reader: &dyn ArtifactReader,
) -> Result<ResearchRunReportV1, AppError> {
    let summary = if verification.verified {
        match declared_artifacts {
            Some(artifacts) => {
                read_json_text_by_id(reader, &artifacts.summary, MAX_SUMMARY_BYTES, "研究摘要")?
            }
            None => read_json_bounded(reader, summary_path, MAX_SUMMARY_BYTES, "研究摘要")?,
        }
    } else {
        read_json_bounded(reader, summary_path, MAX_SUMMARY_BYTES, "研究摘要")
            .unwrap_or(Value::Null)
    };
    let (equity, fills) = if verification.verified {
        match declared_artifacts {
            Some(artifacts) => (
                read_csv_view_by_id(reader, &artifacts.equity, MAX_EQUITY_ROWS, true)?,
                read_csv_view_by_id(reader, &artifacts.fills, MAX_FILL_ROWS, false)?,
            ),
            None => (
                read_csv_view(reader, equity_path, MAX_EQUITY_ROWS, true)?,
                read_csv_view(reader, fills_path, MAX_FILL_ROWS, false)?,
            ),
        }
    } else {
        (empty_rows(), empty_rows())
    };
    Ok(ResearchRunReportV1 {
        schema_version: RESEARCH_RUN_REPORT_SCHEMA_VERSION,
        run_id: run_id.to_owned(),
        instrument: instrument.to_owned(),
        verification,
        summary,
        equity,
        fills,
    })
}

fn read_json_bounded(
    reader: &dyn ArtifactReader,
    path: &str,
    byte_limit: u64,
    label: &str,
) -> Result<Value, AppError> {
    let body = read_text_bounded(reader, path, byte_limit, label)?;
    serde_json::from_str(&body).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("{label} {path} 格式损坏: {error}"),
        )
    })
}

fn read_json_text_by_id(
    reader: &dyn ArtifactReader,
    locator: &ArtifactLocatorV1,
    byte_limit: u64,
    label: &str,
) -> Result<Value, AppError> {
    let body = read_text_bounded_by_id(
        reader,
        &locator.id,
        Some(&locator.legacy_reference),
        byte_limit,
        label,
    )?;
    serde_json::from_str(&body).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("{label} {} 格式损坏: {error}", locator.id.as_str()),
        )
    })
}

fn read_csv_view(
    artifact_reader: &dyn ArtifactReader,
    path: &str,
    sample_limit: usize,
    equity: bool,
) -> Result<ResearchReportRowsV1, AppError> {
    read_csv_view_with_opener(path, sample_limit, equity, None, || {
        open_bounded(artifact_reader, path, MAX_ARTIFACT_BYTES)
    })
}

fn read_csv_view_by_id(
    artifact_reader: &dyn ArtifactReader,
    locator: &ArtifactLocatorV1,
    sample_limit: usize,
    equity: bool,
) -> Result<ResearchReportRowsV1, AppError> {
    read_csv_view_with_opener(
        locator.id.as_str(),
        sample_limit,
        equity,
        Some(&locator.id),
        || open_bounded_by_id(artifact_reader, locator, MAX_ARTIFACT_BYTES),
    )
}

fn read_csv_view_with_opener<F>(
    reference: &str,
    sample_limit: usize,
    equity: bool,
    expected_id: Option<&ArtifactId>,
    mut open: F,
) -> Result<ResearchReportRowsV1, AppError>
where
    F: FnMut() -> Result<ArtifactStream, AppError>,
{
    let mut shape_artifact = open()?;
    let (headers, expected_total) = csv_shape(
        shape_artifact.reader.as_mut(),
        shape_artifact.byte_len,
        reference,
    )?;
    let timestamp_column = headers
        .iter()
        .position(|value| matches!(value.as_str(), "ts" | "timestamp" | "index"));
    let equity_column = headers.iter().position(|value| value == "equity_raw");
    if equity && (timestamp_column.is_none() || equity_column.is_none()) {
        return Err(malformed_csv("权益 CSV 缺少 ts/index 或 equity_raw 列"));
    }
    let mut sample_artifact = open()?;
    let (header, header_bytes) = read_bounded_line(sample_artifact.reader.as_mut(), reference)?
        .ok_or_else(|| malformed_csv("CSV 产物为空"))?;
    let mut bytes_read = header_bytes as u64;
    let mut digest = expected_id.map(|_| qx_strategy::file_digest::Sha256Accumulator::new());
    if let Some(digest) = &mut digest {
        digest.update(header.as_bytes());
    }
    // Equity plots need an evenly distributed sample; fills preserve the historical API
    // contract of returning the first rows so the replay-validated API projection matches.
    let stride = if equity {
        expected_total
            .div_ceil(sample_limit.saturating_sub(1).max(1) as u64)
            .max(1)
    } else {
        1
    };
    let mut index = 0u64;
    let mut sampled = Vec::new();
    loop {
        let Some((line, bytes)) = read_bounded_line(sample_artifact.reader.as_mut(), reference)?
        else {
            break;
        };
        bytes_read = bytes_read.saturating_add(bytes as u64);
        if let Some(digest) = &mut digest {
            digest.update(line.as_bytes());
        }
        if bytes_read > MAX_ARTIFACT_BYTES {
            return Err(artifact_too_large(reference, MAX_ARTIFACT_BYTES));
        }
        if line.trim().is_empty() {
            continue;
        }
        index = index.saturating_add(1);
        if index > MAX_ARTIFACT_ROWS {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                "研究报告明细超过 500 万行上限",
            ));
        }
        if (index - 1).is_multiple_of(stride) || index == expected_total {
            let values = csv_record(line.trim_end())?;
            if values.len() != headers.len() {
                return Err(malformed_csv("CSV 行列数与表头不一致"));
            }
            if sampled.len() < sample_limit {
                if equity {
                    sampled.push(equity_object(
                        &headers,
                        values,
                        timestamp_column.unwrap(),
                        equity_column.unwrap(),
                    )?);
                } else {
                    sampled.push(csv_object(&headers, values));
                }
            }
        }
    }
    if index != expected_total {
        return Err(malformed_csv(
            "CSV 在读取期间发生变化，数据行数与预检不一致",
        ));
    }
    if let (Some(expected_id), Some(digest)) = (expected_id, digest) {
        let actual = digest.finish_hex();
        if actual != expected_id.as_str() {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!(
                    "报告 CSV 与 ArtifactId 内容摘要不一致 {reference}: 期望={} 实际={actual}",
                    expected_id.as_str()
                ),
            ));
        }
    }
    Ok(ResearchReportRowsV1 {
        total: expected_total,
        truncated: sampled.len() < expected_total as usize,
        rows: sampled,
    })
}

fn open_bounded(
    reader: &dyn ArtifactReader,
    reference: &str,
    byte_limit: u64,
) -> Result<ArtifactStream, AppError> {
    let artifact = reader.open(reference)?;
    if artifact.byte_len > byte_limit {
        return Err(artifact_too_large(reference, byte_limit));
    }
    Ok(artifact)
}

fn open_bounded_by_id(
    reader: &dyn ArtifactReader,
    locator: &ArtifactLocatorV1,
    byte_limit: u64,
) -> Result<ArtifactStream, AppError> {
    let artifact = reader.open_by_id(&locator.id, Some(&locator.legacy_reference))?;
    if artifact.byte_len > byte_limit {
        return Err(artifact_too_large(locator.id.as_str(), byte_limit));
    }
    Ok(artifact)
}

fn csv_record(line: &str) -> Result<Vec<String>, AppError> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars().peekable();
    let mut quoted = false;
    while let Some(character) = chars.next() {
        match character {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            value => field.push(value),
        }
    }
    if quoted {
        return Err(malformed_csv("CSV 引号未闭合"));
    }
    fields.push(field);
    Ok(fields)
}

fn csv_shape(
    reader: &mut dyn BufRead,
    byte_limit: u64,
    reference: &str,
) -> Result<(Vec<String>, u64), AppError> {
    let (line, bytes) =
        read_bounded_line(reader, reference)?.ok_or_else(|| malformed_csv("CSV 产物为空"))?;
    if bytes as u64 > byte_limit {
        return Err(artifact_too_large(reference, byte_limit));
    }
    let headers = csv_record(line.trim_end())?;
    if headers.is_empty() || headers.iter().any(|header| header.is_empty()) {
        return Err(malformed_csv("CSV 表头为空或存在空字段"));
    }
    let mut total = 0u64;
    let mut bytes_seen = bytes as u64;
    while let Some((line, bytes)) = read_bounded_line(reader, reference)? {
        bytes_seen = bytes_seen.saturating_add(bytes as u64);
        if bytes_seen > byte_limit {
            return Err(artifact_too_large(reference, byte_limit));
        }
        if !line.trim().is_empty() {
            total += 1;
            if total > MAX_ARTIFACT_ROWS {
                return Err(AppError::new(
                    AppErrorCategory::Timeout,
                    "研究报告明细超过 500 万行上限",
                ));
            }
        }
    }
    Ok((headers, total))
}

fn csv_object(headers: &[String], values: Vec<String>) -> Value {
    Value::Object(
        headers
            .iter()
            .cloned()
            .zip(values.into_iter().map(Value::String))
            .collect(),
    )
}

fn equity_object(
    headers: &[String],
    values: Vec<String>,
    timestamp_column: usize,
    equity_column: usize,
) -> Result<Value, AppError> {
    let timestamp = values
        .get(timestamp_column)
        .ok_or_else(|| malformed_csv("权益 CSV 时间列缺失"))?;
    timestamp
        .parse::<i128>()
        .map_err(|_| malformed_csv("权益 CSV 时间戳不是整数"))?;
    let equity_raw = values
        .get(equity_column)
        .ok_or_else(|| malformed_csv("权益 CSV equity_raw 缺失"))?;
    equity_raw
        .parse::<i128>()
        .map_err(|_| malformed_csv("权益 CSV equity_raw 不是整数"))?;
    let mut row = serde_json::json!({"ts": timestamp, "equity_raw": equity_raw});
    if let Some(position_column) = headers.iter().position(|value| value == "position_raw") {
        let position_raw = values
            .get(position_column)
            .ok_or_else(|| malformed_csv("权益 CSV position_raw 缺失"))?;
        position_raw
            .parse::<i128>()
            .map_err(|_| malformed_csv("权益 CSV position_raw 不是整数"))?;
        row["position_raw"] = Value::String(position_raw.clone());
    }
    Ok(row)
}

fn empty_rows() -> ResearchReportRowsV1 {
    ResearchReportRowsV1 {
        total: 0,
        truncated: false,
        rows: Vec::new(),
    }
}

fn malformed_csv(message: &str) -> AppError {
    AppError::new(AppErrorCategory::StorageFailure, message)
}

fn malformed_event_log(message: &str) -> AppError {
    AppError::new(AppErrorCategory::StorageFailure, message)
}

#[cfg(test)]
mod event_report_tests {
    use super::*;
    use crate::artifact_store::{ArtifactReader, ArtifactStream};
    use std::io::Cursor;

    struct BytesReader {
        event_log: Vec<u8>,
        manifest: Vec<u8>,
    }

    impl ArtifactReader for BytesReader {
        fn open(&self, reference: &str) -> Result<ArtifactStream, AppError> {
            let bytes = if reference == "memory:manifest" {
                self.manifest.clone()
            } else {
                self.event_log.clone()
            };
            Ok(ArtifactStream::new(bytes.len() as u64, Cursor::new(bytes)))
        }
    }

    fn empty_log_facts() -> (Value, Value, String) {
        let digest = format!("{:016x}", qx_core::EventLog::new().digest());
        let summary = serde_json::json!({
            "ledger_entries": 0,
            "replay_ledger_entries": 0,
            "replay": {
                "log_digest": digest,
                "events": 0,
                "ledger_entries": 0,
                "run_ledger_entries": 0
            }
        });
        let manifest = serde_json::json!({"output_event_hash": digest});
        (summary, manifest, digest)
    }

    fn memory_reader(manifest: &Value) -> BytesReader {
        BytesReader {
            event_log: Vec::new(),
            manifest: serde_json::to_vec(manifest).expect("serialize manifest fixture"),
        }
    }

    #[test]
    fn event_report_replays_and_returns_a_versioned_bounded_page() {
        let (summary, manifest, _) = empty_log_facts();
        let report = read_event_report_v1_with_reader(
            "memory:event-log",
            &summary,
            "memory:manifest",
            None,
            &memory_reader(&manifest),
        )
        .expect("empty replay-verified event log");
        assert_eq!(report.schema_version, EVENT_REPORT_PAGE_SCHEMA_VERSION);
        assert_eq!(report.total, 0);
        assert_eq!(report.offset, 0);
        assert!(!report.truncated);
        assert!(report.rows.is_empty());
        assert!(report.fill_counts.is_empty());
    }

    #[test]
    fn event_report_rejects_a_summary_digest_that_does_not_match_replay() {
        let (mut summary, manifest, digest) = empty_log_facts();
        summary["replay"]["log_digest"] = serde_json::Value::String(format!(
            "{:016x}",
            u64::from_str_radix(&digest, 16).unwrap() + 1
        ));
        let error = read_event_report_v1_with_reader(
            "memory:event-log",
            &summary,
            "memory:manifest",
            None,
            &memory_reader(&manifest),
        )
        .expect_err("a changed digest must fail closed");
        assert_eq!(error.category(), AppErrorCategory::StorageFailure);
    }

    #[test]
    fn event_report_rejects_a_page_larger_than_the_registered_limit() {
        let (summary, manifest, _) = empty_log_facts();
        let error = read_event_report_v1_with_reader(
            "memory:event-log",
            &summary,
            "memory:manifest",
            Some((0, MAX_EVENT_REPORT_PAGE_ROWS + 1)),
            &memory_reader(&manifest),
        )
        .expect_err("unbounded pages must be rejected");
        assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    }

    #[test]
    fn multi_market_order_index_keeps_only_order_ids_needed_by_the_page() {
        let submitted = |client_id, instrument: &str| {
            let order = qx_core::Order {
                client_id,
                instrument: qx_core::InstrumentId::parse(instrument)
                    .expect("valid fixture instrument"),
                side: qx_core::Side::Buy,
                qty: qx_core::Quantity::from_i64(1),
                limit: None,
                status: qx_core::OrderStatus::PendingSubmit,
                filled: qx_core::Quantity::from_i64(0),
                account_id: "account-a".into(),
                trace: None,
                policy: None,
                exact_amounts: None,
            };
            qx_core::Event::new(
                client_id,
                1,
                qx_core::Priority::MATCH,
                qx_core::EventKind::OrderSubmitted { order },
            )
        };
        let mut event_log = Vec::new();
        for event in [submitted(11, "MSFT.NASDAQ"), submitted(12, "NVDA.NASDAQ")] {
            serde_json::to_writer(&mut event_log, &event).expect("encode event fixture");
            event_log.push(b'\n');
        }
        let mut expected_digest = qx_strategy::file_digest::Sha256Accumulator::new();
        expected_digest.update(b"qx-multi-market-event-trace-bytes-v1\0");
        expected_digest.update(&event_log);
        let reader = BytesReader {
            event_log,
            manifest: Vec::new(),
        };
        let index = read_multi_market_order_market_index_v1(
            "memory:event-log",
            &expected_digest.finish_hex(),
            &BTreeSet::from([12]),
            &BTreeMap::new(),
            &reader,
        )
        .expect("index should be limited to the requested page order IDs");
        assert_eq!(index, BTreeMap::from([(12, "NVDA.NASDAQ".to_owned())]));
        let changed_error = read_multi_market_order_market_index_v1(
            "memory:event-log",
            &"0".repeat(64),
            &BTreeSet::from([12]),
            &BTreeMap::new(),
            &reader,
        )
        .expect_err("the second-pass bytes must match the replayed trace");
        assert!(changed_error.message().contains("分页归属复核期间发生变化"));
    }

    fn submitted_event(sequence: u64, client_id: u64, instrument: &str) -> qx_core::Event {
        let order = qx_core::Order {
            client_id,
            instrument: qx_core::InstrumentId::parse(instrument).expect("valid fixture instrument"),
            side: qx_core::Side::Buy,
            qty: qx_core::Quantity::from_i64(1),
            limit: None,
            status: qx_core::OrderStatus::PendingSubmit,
            filled: qx_core::Quantity::from_i64(0),
            account_id: "account-a".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        qx_core::Event::new(
            sequence,
            sequence + 1,
            qx_core::Priority::MATCH,
            qx_core::EventKind::OrderSubmitted { order },
        )
    }

    #[test]
    fn legacy_multi_market_report_replays_inline_events_and_projects_a_bounded_page() {
        let mut log = qx_core::EventLog::new();
        let events = [
            submitted_event(0, 11, "MSFT.NASDAQ"),
            submitted_event(1, 12, "NVDA.NASDAQ"),
        ]
        .into_iter()
        .map(|event| {
            log.append_checked(event.clone())
                .expect("valid ordered EventLog event");
            serde_json::to_value(event).expect("serialize EventLog event")
        })
        .collect::<Vec<_>>();
        let ledger_entries =
            serde_json::to_value(qx_core::Ledger::new().entries()).expect("serialize empty ledger");
        let report = read_multi_market_legacy_event_report_v1(
            &events,
            &format!("{:016x}", log.digest()),
            &ledger_entries,
            &BTreeMap::new(),
            Some((1, 1)),
        )
        .expect("verify and project legacy event page");

        assert_eq!(report.total, 2);
        assert_eq!(report.offset, 1);
        assert!(report.truncated);
        assert_eq!(report.rows, vec![events[1].clone()]);
        assert_eq!(report.fill_count, 0);
        assert_eq!(
            report.order_markets,
            BTreeMap::from([
                (11, "MSFT.NASDAQ".to_owned()),
                (12, "NVDA.NASDAQ".to_owned()),
            ])
        );
    }

    #[test]
    fn legacy_multi_market_report_rejects_one_order_attributed_to_two_markets() {
        let mut log = qx_core::EventLog::new();
        let events = [
            submitted_event(0, 11, "MSFT.NASDAQ"),
            submitted_event(1, 11, "NVDA.NASDAQ"),
        ]
        .into_iter()
        .map(|event| {
            log.append_checked(event.clone())
                .expect("ordered events are valid for replay");
            serde_json::to_value(event).expect("serialize EventLog event")
        })
        .collect::<Vec<_>>();
        let ledger_entries =
            serde_json::to_value(qx_core::Ledger::new().entries()).expect("serialize empty ledger");
        let error = read_multi_market_legacy_event_report_v1(
            &events,
            &format!("{:016x}", log.digest()),
            &ledger_entries,
            &BTreeMap::new(),
            Some((0, 2)),
        )
        .expect_err("conflicting order market attribution must be rejected");

        assert!(error.message().contains("关联到不同标的"));
    }

    #[test]
    fn fills_report_keeps_the_first_rows_for_api_replay_projection_compatibility() {
        let reader = BytesReader {
            event_log: b"id,value\n1,a\n2,b\n3,c\n4,d\n5,e\n".to_vec(),
            manifest: Vec::new(),
        };
        let view = read_csv_view(&reader, "memory:fills", 2, false).expect("bounded fills view");
        assert_eq!(view.total, 5);
        assert!(view.truncated);
        assert_eq!(view.rows[0]["id"], "1");
        assert_eq!(view.rows[1]["id"], "2");
    }

    struct IdOnlyReader(Vec<u8>);

    impl ArtifactReader for IdOnlyReader {
        fn open(&self, _reference: &str) -> Result<ArtifactStream, AppError> {
            Err(AppError::new(
                AppErrorCategory::DataUnavailable,
                "path lookup is intentionally unavailable",
            ))
        }

        fn open_by_id(
            &self,
            _artifact_id: &ArtifactId,
            _legacy_reference: Option<&str>,
        ) -> Result<ArtifactStream, AppError> {
            Ok(ArtifactStream::new(
                self.0.len() as u64,
                Cursor::new(self.0.clone()),
            ))
        }
    }

    #[test]
    fn report_projection_checks_csv_content_id_during_the_sampling_read() {
        let expected = b"ts,equity_raw,position_raw\n1,10,0\n2,20,1\n";
        let id = ArtifactId::from_sha256_hex(qx_strategy::sha256_hex(expected))
            .expect("valid content digest");
        let reader = IdOnlyReader(b"ts,equity_raw,position_raw\n1,10,0\n2,21,1\n".to_vec());
        let locator = ArtifactLocatorV1 {
            id,
            legacy_reference: "legacy-equity.csv".to_owned(),
        };

        let error = read_csv_view_by_id(&reader, &locator, 10, true)
            .expect_err("projection must reject content that does not match its ID");
        assert_eq!(error.category(), AppErrorCategory::Conflict);
    }

    #[test]
    fn strategy_event_context_does_not_expose_the_bar_that_triggered_it() {
        let frame = qx_datastruct::BarFrame {
            instrument: qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            source: qx_guanxing::DataSourceId::new("fixture:causal-bar-context"),
            ts: vec![100, 200],
            open_raw: vec![10, 20],
            high_raw: vec![12, 22],
            low_raw: vec![9, 19],
            close_raw: vec![11, 21],
            volume_raw: vec![100, 200],
            exact_ohlcv: None,
        };
        let decision = multi_market_bar_context(&frame, 200, qx_core::Priority::COMMAND)
            .expect("a previous visible bar exists");
        assert_eq!(decision["bar_index"], 0);
        assert_eq!(decision["relation"], "previous_visible");
        assert_eq!(decision["bar"]["close_raw"], "11");

        let fill = multi_market_bar_context(&frame, 200, qx_core::Priority::APPLY)
            .expect("the current bar is visible after matching");
        assert_eq!(fill["bar_index"], 1);
        assert_eq!(fill["relation"], "same_timestamp_visible");
    }

    #[test]
    fn event_before_the_first_bar_has_no_market_context() {
        let frame = qx_datastruct::BarFrame {
            instrument: qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            source: qx_guanxing::DataSourceId::new("fixture:empty-causal-prefix"),
            ts: vec![100],
            open_raw: vec![10],
            high_raw: vec![12],
            low_raw: vec![9],
            close_raw: vec![11],
            volume_raw: vec![100],
            exact_ohlcv: None,
        };
        assert!(multi_market_bar_context(&frame, 50, qx_core::Priority::COMMAND).is_none());
    }
}
