//! Report facts shared by multi-leg report readers and presentation adapters.

use crate::artifact_store::{read_text_bounded, ArtifactReader, FileArtifactReader};
use crate::{AppError, AppErrorCategory, CallerCapability, RunContext};
use qx_core::{DecimalRounding, ExactDecimal, Fill, Fnv1a, InstrumentId, TradingInstrumentSpec};
use qx_datastruct::BarFrame;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const MAX_MULTI_LEG_REPORT_ROWS: usize = 5_000_000;
pub const MAX_MULTI_LEG_EVENT_REPORT_PAGE_ROWS: usize = 300;
pub const MAX_MULTI_LEG_EQUITY_REPORT_ROWS: usize = 1_000;
pub const MAX_MULTI_LEG_FILL_REPORT_ROWS: usize = 200;
pub const MULTI_LEG_RESEARCH_REPORT_SCHEMA_VERSION_V1: u32 = 1;
pub const MULTI_LEG_EVENT_REPORT_SCHEMA_VERSION_V1: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiLegEquityReportFactsV1 {
    pub points: u64,
    pub initial_equity_raw: i128,
    pub final_equity_raw: i128,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiLegMarketFillReportFactsV1 {
    pub instrument: String,
    pub fills: u64,
    pub result_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiLegLegReportFactsV1 {
    pub instrument: String,
    pub fills: u64,
    pub result_hash: String,
    pub return_bps: i32,
    pub funding_cost_raw: i128,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiLegReportCostFactsV1 {
    pub total_fees_raw: i128,
    pub total_funding_raw: i128,
    pub total_turnover_raw: i128,
    pub cost_bps: Option<i64>,
}

/// Canonical facts shared by every surface that reads a persisted multi-leg run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiLegResearchReportFactsV1 {
    pub schema_version: u32,
    pub account_id: String,
    pub initial_cash_raw: i128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_cash_exact: Option<String>,
    pub account_ledger_summary: Value,
    pub event_count: u64,
    pub event_hash: String,
    pub order_markets: BTreeMap<u64, String>,
    pub primary: MultiLegLegReportFactsV1,
    pub reference: MultiLegLegReportFactsV1,
    pub costs: MultiLegReportCostFactsV1,
    pub equity: MultiLegEquityReportFactsV1,
    pub total_fills: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiLegEventPageRequestV1 {
    pub offset: u64,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiLegEventReportReadRequestV1 {
    pub schema_version: u32,
    pub outcome: crate::MultiLegBacktestOutcome,
    #[serde(default)]
    pub page: Option<MultiLegEventPageRequestV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiLegResearchReportReadRequestV1 {
    pub schema_version: u32,
    pub outcome: crate::MultiLegBacktestOutcome,
    #[serde(default)]
    pub page: Option<MultiLegEventPageRequestV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiLegEventReportPageV1 {
    pub schema_version: u32,
    pub run_id: String,
    pub total: u64,
    pub offset: u64,
    pub limit: usize,
    pub truncated: bool,
    pub rows: Vec<Value>,
    pub event_hash: String,
    pub fill_count: u64,
}

/// Bounded report rows derived from a fully verified multi-leg artifact.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiLegReportRowsV1 {
    pub total: u64,
    pub truncated: bool,
    pub rows: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiLegResearchReportLegSummaryV1 {
    pub instrument: String,
    pub fills: u64,
    pub result_hash: String,
    pub return_bps: i32,
    pub funding_cost_raw: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiLegResearchReportCostSummaryV1 {
    pub total_fees_raw: String,
    pub total_funding_raw: String,
    pub total_turnover_raw: String,
    pub cost_bps: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiLegResearchReportEquitySummaryV1 {
    pub points: u64,
    pub initial_equity_raw: String,
    pub final_equity_raw: String,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiLegResearchReportSummaryV1 {
    pub account_id: String,
    pub initial_cash_raw: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_cash_exact: Option<String>,
    pub account_ledger_summary: Value,
    pub event_count: u64,
    pub event_hash: String,
    pub primary: MultiLegResearchReportLegSummaryV1,
    pub reference: MultiLegResearchReportLegSummaryV1,
    pub costs: MultiLegResearchReportCostSummaryV1,
    pub equity: MultiLegResearchReportEquitySummaryV1,
    pub total_fills: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiLegResearchReportMarketV1 {
    pub instrument: String,
    pub data_fingerprint: String,
    pub quality_fingerprint: Option<String>,
    pub bar_count: u64,
    pub start_ts_ms: Option<u64>,
    pub end_ts_ms: Option<u64>,
    pub fills: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiLegResearchReportExecutionV1 {
    pub settlement_currency: String,
    pub fill_model: crate::MultiLegFillModel,
    pub primary_maker_fee_bps: i64,
    pub primary_taker_fee_bps: i64,
    pub reference_maker_fee_bps: i64,
    pub reference_taker_fee_bps: i64,
    pub funding_bps: i64,
    pub funding_events_count: u64,
    pub funding_events_sha256: String,
    /// JSON text keeps fixed-width integers exact across consumers that only have IEEE-754 numbers.
    pub market_specs_json: String,
}

/// Complete bounded core report shared by CLI, Python, API adapters, and reports tooling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiLegResearchReportV1 {
    pub schema_version: u32,
    pub run_id: String,
    pub strategy_id: String,
    pub data_fingerprint: String,
    pub summary: MultiLegResearchReportSummaryV1,
    pub execution: MultiLegResearchReportExecutionV1,
    pub markets: Vec<MultiLegResearchReportMarketV1>,
    pub equity: MultiLegReportRowsV1,
    pub fills: MultiLegReportRowsV1,
    pub events: MultiLegEventReportPageV1,
}

/// Verified inputs and report facts shared by multi-leg presentation adapters.
///
/// The artifact and dataset roots may be backed by different authorized readers.
#[derive(Clone, Debug)]
pub struct MultiLegResearchReportReadModelV1 {
    pub artifact: Value,
    pub spec: crate::MultiLegBacktestSpec,
    pub frames: BTreeMap<String, BarFrame>,
    pub fingerprints: BTreeMap<String, String>,
    pub quality_fingerprints: BTreeMap<String, String>,
    pub data_fingerprint: String,
    pub facts: MultiLegResearchReportFactsV1,
    pub report: MultiLegResearchReportV1,
}

pub struct MultiLegResearchReportInputV1<'a> {
    pub outcome: &'a crate::MultiLegBacktestOutcome,
    pub spec: &'a crate::MultiLegBacktestSpec,
    pub artifact: &'a Value,
    pub frames: &'a BTreeMap<String, BarFrame>,
}

/// Read a local multi-leg artifact bundle and return the same verified facts used by API reports.
pub fn read_multi_leg_report_facts_v1(
    outcome: &crate::MultiLegBacktestOutcome,
    context: &RunContext,
) -> Result<MultiLegResearchReportFactsV1, AppError> {
    read_multi_leg_report_facts_v1_with_reader(outcome, context, &FileArtifactReader)
}

/// Read and verify a multi-leg report through an application-owned storage adapter.
pub fn read_multi_leg_report_facts_v1_with_reader(
    outcome: &crate::MultiLegBacktestOutcome,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<MultiLegResearchReportFactsV1, AppError> {
    read_multi_leg_report_facts_v1_with_readers(outcome, context, reader, reader)
}

/// Read and verify a multi-leg report when artifacts and market data have separate roots.
pub fn read_multi_leg_report_facts_v1_with_readers(
    outcome: &crate::MultiLegBacktestOutcome,
    context: &RunContext,
    artifact_reader: &dyn ArtifactReader,
    data_reader: &dyn ArtifactReader,
) -> Result<MultiLegResearchReportFactsV1, AppError> {
    read_multi_leg_report_read_model_v1_with_readers(outcome, context, artifact_reader, data_reader)
        .map(|model| model.facts)
}

/// Read the verified artifact, data inputs, identities, and canonical report facts.
pub fn read_multi_leg_report_read_model_v1_with_readers(
    outcome: &crate::MultiLegBacktestOutcome,
    context: &RunContext,
    artifact_reader: &dyn ArtifactReader,
    data_reader: &dyn ArtifactReader,
) -> Result<MultiLegResearchReportReadModelV1, AppError> {
    read_multi_leg_report_read_model_v1_with_page_and_readers(
        outcome,
        context,
        MultiLegEventPageRequestV1 {
            offset: 0,
            limit: MAX_MULTI_LEG_EVENT_REPORT_PAGE_ROWS,
        },
        artifact_reader,
        data_reader,
    )
}

/// Read a multi-leg report with a bounded, replay-verified event page.
pub fn read_multi_leg_report_read_model_v1_with_page_and_readers(
    outcome: &crate::MultiLegBacktestOutcome,
    context: &RunContext,
    page_request: MultiLegEventPageRequestV1,
    artifact_reader: &dyn ArtifactReader,
    data_reader: &dyn ArtifactReader,
) -> Result<MultiLegResearchReportReadModelV1, AppError> {
    context.require(CallerCapability::Research, "多腿研究报告读取")?;
    validate_multi_leg_event_page_request(page_request)?;
    let artifact_text = read_text_bounded(
        artifact_reader,
        &outcome.attribution_artifact,
        128 * 1024 * 1024,
        "多腿归因产物",
    )?;
    let artifact: Value = serde_json::from_str(&artifact_text).map_err(|error| {
        multi_leg_report_error(
            AppErrorCategory::Conflict,
            format!("多腿归因产物 JSON 无效: {error}"),
        )
    })?;
    let artifact_version = artifact.get("schema_version").and_then(Value::as_u64);
    if !matches!(artifact_version, Some(3 | 4 | 5 | 6)) {
        return Err(multi_leg_report_error(
            AppErrorCategory::Conflict,
            "多腿回测产物 schema_version 不受支持",
        ));
    }
    let spec: crate::MultiLegBacktestSpec =
        serde_json::from_value(artifact.get("spec").cloned().ok_or_else(|| {
            multi_leg_report_error(AppErrorCategory::Conflict, "多腿产物缺少原始 spec")
        })?)
        .map_err(|error| {
            multi_leg_report_error(
                AppErrorCategory::Conflict,
                format!("多腿产物 spec 无效: {error}"),
            )
        })?;
    spec.validate()?;
    if (spec.schema_version == crate::MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION)
        != (artifact_version == Some(6))
    {
        return Err(multi_leg_report_error(
            AppErrorCategory::Conflict,
            "多腿 exact principal spec 与 v6 产物版本不一致",
        ));
    }
    if spec.run_id != outcome.run_id {
        return Err(multi_leg_report_error(
            AppErrorCategory::Conflict,
            "多腿 outcome 与产物 spec run_id 不一致",
        ));
    }

    let market_data = artifact
        .get("market_data")
        .and_then(Value::as_array)
        .filter(|markets| markets.len() == 2)
        .ok_or_else(|| {
            multi_leg_report_error(
                AppErrorCategory::Conflict,
                "多腿产物 market_data 必须恰含两条腿",
            )
        })?;
    let quality_objects = artifact
        .get("dataset_quality")
        .filter(|value| !value.is_null());
    let mut frames = BTreeMap::<String, BarFrame>::new();
    let mut fingerprints = BTreeMap::<String, String>::new();
    let mut quality_fingerprints = BTreeMap::<String, String>::new();
    for market in market_data {
        let instrument = market
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                multi_leg_report_error(
                    AppErrorCategory::Conflict,
                    "多腿 market_data 缺少 instrument",
                )
            })?;
        let leg = if instrument == outcome.primary_instrument {
            "primary"
        } else if instrument == outcome.reference_instrument {
            "reference"
        } else {
            return Err(multi_leg_report_error(
                AppErrorCategory::Conflict,
                format!("多腿输入包含未声明标的: {instrument}"),
            ));
        };
        if frames.contains_key(instrument) {
            return Err(multi_leg_report_error(
                AppErrorCategory::Conflict,
                format!("多腿输入标的重复: {instrument}"),
            ));
        }
        let path = market
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| {
                multi_leg_report_error(
                    AppErrorCategory::Conflict,
                    format!("{instrument} 缺少 BarFrame 路径"),
                )
            })?;
        let frame_text = read_text_bounded(
            data_reader,
            path,
            128 * 1024 * 1024,
            &format!("{instrument} BarFrame"),
        )?;
        let frame = BarFrame::from_json(&frame_text).map_err(|error| {
            multi_leg_report_error(
                AppErrorCategory::Conflict,
                format!("{instrument} BarFrame 无效: {error:?}"),
            )
        })?;
        if frame.ts.len() > MAX_MULTI_LEG_REPORT_ROWS {
            return Err(multi_leg_report_error(
                AppErrorCategory::Timeout,
                format!("{instrument} BarFrame 超过行数上限"),
            ));
        }
        let fingerprint = qx_data::fingerprint_bar_frame(&frame).map_err(|error| {
            multi_leg_report_error(
                AppErrorCategory::Conflict,
                format!("{instrument} BarFrame 指纹失败: {error}"),
            )
        })?;
        if frame.instrument.to_string() != instrument
            || market.get("content_hash").and_then(Value::as_str) != Some(fingerprint.as_str())
            || market.get("row_count").and_then(Value::as_u64) != Some(frame.ts.len() as u64)
            || (matches!(artifact_version, Some(4 | 5 | 6))
                && (market.get("start_ts_ms").and_then(Value::as_u64) != frame.ts.first().copied()
                    || market.get("end_ts_ms").and_then(Value::as_u64) != frame.ts.last().copied()))
        {
            return Err(multi_leg_report_error(
                AppErrorCategory::Conflict,
                format!("{instrument} BarFrame 与运行输入身份不一致"),
            ));
        }
        let declared_manifest = if leg == "primary" {
            spec.primary_dataset_manifest_path.as_deref()
        } else {
            spec.reference_dataset_manifest_path.as_deref()
        };
        let actual_manifest = market.get("dataset_manifest_path").and_then(Value::as_str);
        if actual_manifest != declared_manifest {
            return Err(multi_leg_report_error(
                AppErrorCategory::Conflict,
                format!("{instrument} 数据质量清单路径与 spec 不一致"),
            ));
        }
        if let Some(manifest_path) = declared_manifest {
            let evidence = quality_objects
                .and_then(|quality| quality.get(leg))
                .ok_or_else(|| {
                    multi_leg_report_error(
                        AppErrorCategory::Conflict,
                        format!("{instrument} 缺少数据质量核验结果"),
                    )
                })?;
            let manifest_text = read_text_bounded(
                data_reader,
                manifest_path,
                32 * 1024 * 1024,
                &format!("{instrument} 数据质量清单"),
            )?;
            let quality_fingerprint = crate::verify_multi_leg_report_quality_evidence_bytes_v1(
                manifest_text.as_bytes(),
                manifest_path,
                frame_text.as_bytes(),
                path,
                &frame,
                &fingerprint,
                manifest_path,
                evidence,
            )
            .map_err(|error| {
                multi_leg_report_error(
                    AppErrorCategory::Conflict,
                    format!("{instrument} 数据质量清单复核失败: {}", error.message()),
                )
            })?;
            quality_fingerprints.insert(instrument.to_owned(), quality_fingerprint);
        } else if quality_objects.is_some() {
            return Err(multi_leg_report_error(
                AppErrorCategory::Conflict,
                format!("{instrument} 未声明数据质量清单但产物含核验结果"),
            ));
        }
        fingerprints.insert(instrument.to_owned(), fingerprint);
        frames.insert(instrument.to_owned(), frame);
    }
    if frames.len() != 2
        || !frames.contains_key(&outcome.primary_instrument)
        || !frames.contains_key(&outcome.reference_instrument)
    {
        return Err(multi_leg_report_error(
            AppErrorCategory::Conflict,
            "多腿回测缺少主腿或参考腿输入",
        ));
    }
    let data_fingerprint = if quality_fingerprints.is_empty() {
        format!(
            "multi-bar:{}:{}",
            fingerprints[&outcome.primary_instrument], fingerprints[&outcome.reference_instrument]
        )
    } else {
        format!(
            "multi-bar:{}:{}:quality:{}:{}",
            fingerprints[&outcome.primary_instrument],
            fingerprints[&outcome.reference_instrument],
            quality_fingerprints[&outcome.primary_instrument],
            quality_fingerprints[&outcome.reference_instrument]
        )
    };
    if artifact.get("data_fingerprint").and_then(Value::as_str) != Some(data_fingerprint.as_str()) {
        return Err(multi_leg_report_error(
            AppErrorCategory::Conflict,
            "多腿数据指纹与已复核输入不一致",
        ));
    }
    if matches!(artifact_version, Some(4 | 5 | 6)) {
        let manifest_path = artifact
            .get("run_manifest_path")
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| {
                multi_leg_report_error(AppErrorCategory::Conflict, "多腿产物缺少 RunManifest 路径")
            })?;
        let manifest_text = read_text_bounded(
            artifact_reader,
            manifest_path,
            16 * 1024 * 1024,
            "多腿 RunManifest",
        )?;
        let manifest: qx_core::RunManifest =
            serde_json::from_str(&manifest_text).map_err(|error| {
                multi_leg_report_error(
                    AppErrorCategory::Conflict,
                    format!("多腿 RunManifest 无效: {error}"),
                )
            })?;
        manifest.validate().map_err(|error| {
            multi_leg_report_error(
                AppErrorCategory::Conflict,
                format!("多腿 RunManifest 校验失败: {error}"),
            )
        })?;
        crate::artifact_store::verify_canonical_run_spec_for_manifest(&manifest, &artifact)
            .map_err(|error| {
                multi_leg_report_error(
                    AppErrorCategory::Conflict,
                    format!("多腿 CanonicalRunSpec 复核失败: {error}"),
                )
            })?;
        if manifest.run_id != outcome.run_id
            || manifest.data_fingerprint != data_fingerprint
            || artifact.get("shared_result_hash").and_then(Value::as_str)
                != Some(manifest.result_hash.as_str())
            || artifact.get("event_hash").and_then(Value::as_str)
                != Some(manifest.output_event_hash.as_str())
        {
            return Err(multi_leg_report_error(
                AppErrorCategory::Conflict,
                "多腿 RunManifest 与回测产物身份/结果摘要不一致",
            ));
        }
    }
    let facts = verify_multi_leg_research_report_facts_v1(MultiLegResearchReportInputV1 {
        outcome,
        spec: &spec,
        artifact: &artifact,
        frames: &frames,
    })
    .map_err(|error| multi_leg_report_error(AppErrorCategory::Conflict, error))?;
    let events = artifact
        .get("shared_event_log")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            multi_leg_report_error(AppErrorCategory::Conflict, "多腿产物缺少共享 EventLog")
        })?;
    let total = u64::try_from(events.len())
        .map_err(|_| multi_leg_report_error(AppErrorCategory::Conflict, "多腿事件数量超出 u64"))?;
    if page_request.offset > total {
        return Err(multi_leg_report_error(
            AppErrorCategory::InvalidInput,
            "事件分页 offset 超出多腿事件流范围",
        ));
    }
    let start = usize::try_from(page_request.offset).map_err(|_| {
        multi_leg_report_error(
            AppErrorCategory::InvalidInput,
            "事件分页 offset 超出平台范围",
        )
    })?;
    let end = start.saturating_add(page_request.limit).min(events.len());
    let event_page = MultiLegEventReportPageV1 {
        schema_version: MULTI_LEG_EVENT_REPORT_SCHEMA_VERSION_V1,
        run_id: outcome.run_id.clone(),
        total,
        offset: page_request.offset,
        limit: page_request.limit,
        truncated: end < events.len(),
        rows: events[start..end].to_vec(),
        event_hash: facts.event_hash.clone(),
        fill_count: facts.total_fills,
    };
    let equity_curve = artifact
        .get("equity_curve")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            multi_leg_report_error(AppErrorCategory::Conflict, "多腿产物缺少共享权益曲线")
        })?;
    let equity_rows = if matches!(
        artifact.get("schema_version").and_then(Value::as_u64),
        Some(5 | 6)
    ) {
        let exact_curve = artifact
            .get("equity_curve_exact")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                multi_leg_report_error(AppErrorCategory::Conflict, "多腿 v5 产物缺少精确权益曲线")
            })?;
        multi_leg_equity_report_rows_exact_v1(equity_curve, exact_curve)?
    } else {
        multi_leg_equity_report_rows_v1(equity_curve)?
    };
    let fill_rows = multi_leg_fill_report_rows_v1(events, &facts.order_markets, facts.total_fills)?;
    let strategy_id = artifact
        .get("strategy_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| multi_leg_report_error(AppErrorCategory::Conflict, "多腿产物缺少策略身份"))?
        .to_owned();
    let effective_market_specs = artifact
        .get("effective_market_specs")
        .filter(|value| value.is_object())
        .ok_or_else(|| {
            multi_leg_report_error(AppErrorCategory::Conflict, "多腿产物缺少有效市场规格")
        })?;
    let market_specs_json = serde_json::to_string(effective_market_specs).map_err(|error| {
        multi_leg_report_error(
            AppErrorCategory::Conflict,
            format!("多腿有效市场规格序列化失败: {error}"),
        )
    })?;
    let funding_events_bytes = serde_json::to_vec(&spec.funding_events).map_err(|error| {
        multi_leg_report_error(
            AppErrorCategory::Conflict,
            format!("多腿 funding_events 编码失败: {error}"),
        )
    })?;
    let funding_events_sha256 = qx_data::sha256_hex_bytes(&funding_events_bytes);
    let market_report = |instrument: &str, fills: u64| {
        let frame = &frames[instrument];
        MultiLegResearchReportMarketV1 {
            instrument: instrument.to_owned(),
            data_fingerprint: fingerprints[instrument].clone(),
            quality_fingerprint: quality_fingerprints.get(instrument).cloned(),
            bar_count: frame.ts.len() as u64,
            start_ts_ms: frame.ts.first().copied(),
            end_ts_ms: frame.ts.last().copied(),
            fills,
        }
    };
    let report = MultiLegResearchReportV1 {
        schema_version: MULTI_LEG_RESEARCH_REPORT_SCHEMA_VERSION_V1,
        run_id: outcome.run_id.clone(),
        strategy_id,
        data_fingerprint: data_fingerprint.clone(),
        summary: MultiLegResearchReportSummaryV1 {
            account_id: facts.account_id.clone(),
            initial_cash_raw: facts.initial_cash_raw.to_string(),
            initial_cash_exact: facts.initial_cash_exact.clone(),
            account_ledger_summary: facts.account_ledger_summary.clone(),
            event_count: facts.event_count,
            event_hash: facts.event_hash.clone(),
            primary: MultiLegResearchReportLegSummaryV1 {
                instrument: facts.primary.instrument.clone(),
                fills: facts.primary.fills,
                result_hash: facts.primary.result_hash.clone(),
                return_bps: facts.primary.return_bps,
                funding_cost_raw: facts.primary.funding_cost_raw.to_string(),
            },
            reference: MultiLegResearchReportLegSummaryV1 {
                instrument: facts.reference.instrument.clone(),
                fills: facts.reference.fills,
                result_hash: facts.reference.result_hash.clone(),
                return_bps: facts.reference.return_bps,
                funding_cost_raw: facts.reference.funding_cost_raw.to_string(),
            },
            costs: MultiLegResearchReportCostSummaryV1 {
                total_fees_raw: facts.costs.total_fees_raw.to_string(),
                total_funding_raw: facts.costs.total_funding_raw.to_string(),
                total_turnover_raw: facts.costs.total_turnover_raw.to_string(),
                cost_bps: facts.costs.cost_bps,
            },
            equity: MultiLegResearchReportEquitySummaryV1 {
                points: facts.equity.points,
                initial_equity_raw: facts.equity.initial_equity_raw.to_string(),
                final_equity_raw: facts.equity.final_equity_raw.to_string(),
                return_bps: facts.equity.return_bps,
                max_drawdown_bps: facts.equity.max_drawdown_bps,
            },
            total_fills: facts.total_fills,
        },
        execution: MultiLegResearchReportExecutionV1 {
            settlement_currency: spec.settlement_currency.clone(),
            fill_model: spec.fill_model,
            primary_maker_fee_bps: spec.primary_maker_fee_bps.unwrap_or(spec.maker_fee_bps),
            primary_taker_fee_bps: spec.primary_taker_fee_bps.unwrap_or(spec.taker_fee_bps),
            reference_maker_fee_bps: spec.reference_maker_fee_bps.unwrap_or(spec.maker_fee_bps),
            reference_taker_fee_bps: spec.reference_taker_fee_bps.unwrap_or(spec.taker_fee_bps),
            funding_bps: spec.funding_bps,
            funding_events_count: spec.funding_events.len() as u64,
            funding_events_sha256,
            market_specs_json,
        },
        markets: vec![
            market_report(&outcome.primary_instrument, facts.primary.fills),
            market_report(&outcome.reference_instrument, facts.reference.fills),
        ],
        equity: equity_rows,
        fills: fill_rows,
        events: event_page,
    };
    Ok(MultiLegResearchReportReadModelV1 {
        artifact,
        spec,
        frames,
        fingerprints,
        quality_fingerprints,
        data_fingerprint,
        facts,
        report,
    })
}

fn multi_leg_equity_report_rows_v1(rows: &[Value]) -> Result<MultiLegReportRowsV1, AppError> {
    let total = u64::try_from(rows.len()).map_err(|_| {
        multi_leg_report_error(AppErrorCategory::Conflict, "多腿权益点数量超出 u64")
    })?;
    let stride = rows
        .len()
        .div_ceil(MAX_MULTI_LEG_EQUITY_REPORT_ROWS.saturating_sub(1).max(1))
        .max(1);
    let selected = (0..rows.len())
        .step_by(stride)
        .chain((!rows.is_empty()).then_some(rows.len() - 1))
        .collect::<std::collections::BTreeSet<_>>();
    let rows = selected
        .into_iter()
        .map(|index| {
            let row = rows.get(index).and_then(Value::as_array).ok_or_else(|| {
                multi_leg_report_error(AppErrorCategory::Conflict, "多腿权益曲线行结构无效")
            })?;
            let ts = row.first().and_then(raw_i128).ok_or_else(|| {
                multi_leg_report_error(AppErrorCategory::Conflict, "多腿权益曲线时间无效")
            })?;
            let equity_raw = row.get(1).and_then(raw_i128).ok_or_else(|| {
                multi_leg_report_error(AppErrorCategory::Conflict, "多腿权益曲线金额无效")
            })?;
            Ok(serde_json::json!({
                "ts": ts.to_string(),
                "equity_raw": equity_raw.to_string()
            }))
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(MultiLegReportRowsV1 {
        total,
        truncated: total > MAX_MULTI_LEG_EQUITY_REPORT_ROWS as u64,
        rows,
    })
}

fn multi_leg_equity_report_rows_exact_v1(
    rows: &[Value],
    exact_rows: &[Value],
) -> Result<MultiLegReportRowsV1, AppError> {
    if rows.len() != exact_rows.len() {
        return Err(multi_leg_report_error(
            AppErrorCategory::Conflict,
            "多腿 raw/exact 权益报告行数不一致",
        ));
    }
    let total = u64::try_from(rows.len()).map_err(|_| {
        multi_leg_report_error(AppErrorCategory::Conflict, "多腿权益点数量超出 u64")
    })?;
    let stride = rows
        .len()
        .div_ceil(MAX_MULTI_LEG_EQUITY_REPORT_ROWS.saturating_sub(1).max(1))
        .max(1);
    let selected = (0..rows.len())
        .step_by(stride)
        .chain((!rows.is_empty()).then_some(rows.len() - 1))
        .collect::<std::collections::BTreeSet<_>>();
    let report_rows = selected
        .into_iter()
        .map(|index| {
            let raw_row = rows.get(index).and_then(Value::as_array).ok_or_else(|| {
                multi_leg_report_error(AppErrorCategory::Conflict, "多腿权益曲线行结构无效")
            })?;
            let exact_row = exact_rows
                .get(index)
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    multi_leg_report_error(
                        AppErrorCategory::Conflict,
                        "多腿 exact 权益曲线行结构无效",
                    )
                })?;
            let ts = raw_row.first().and_then(raw_i128).ok_or_else(|| {
                multi_leg_report_error(AppErrorCategory::Conflict, "多腿权益曲线时间无效")
            })?;
            let equity_raw = raw_row.get(1).and_then(raw_i128).ok_or_else(|| {
                multi_leg_report_error(AppErrorCategory::Conflict, "多腿权益曲线金额无效")
            })?;
            let exact_index = exact_row.first().and_then(Value::as_u64);
            let exact_ts = exact_row.get(1).and_then(raw_i128);
            let exact = exact_row.get(2).and_then(Value::as_str).ok_or_else(|| {
                multi_leg_report_error(
                    AppErrorCategory::Conflict,
                    "多腿 exact 权益金额必须为十进制字符串",
                )
            })?;
            if exact_index != Some(index as u64) || exact_ts != Some(ts) {
                return Err(multi_leg_report_error(
                    AppErrorCategory::Conflict,
                    "多腿 raw/exact 权益采样时间轴不一致",
                ));
            }
            Ok(serde_json::json!({
                "ts": ts.to_string(),
                "equity_raw": equity_raw.to_string(),
                "equity_exact": exact,
            }))
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(MultiLegReportRowsV1 {
        total,
        truncated: total > MAX_MULTI_LEG_EQUITY_REPORT_ROWS as u64,
        rows: report_rows,
    })
}

fn multi_leg_fill_report_rows_v1(
    events: &[Value],
    order_markets: &BTreeMap<u64, String>,
    total: u64,
) -> Result<MultiLegReportRowsV1, AppError> {
    let event_fill_count = u64::try_from(
        events
            .iter()
            .filter(|event| event.pointer("/kind/Filled/fill").is_some())
            .count(),
    )
    .map_err(|_| multi_leg_report_error(AppErrorCategory::Conflict, "多腿成交事件数量超出 u64"))?;
    if event_fill_count != total {
        return Err(multi_leg_report_error(
            AppErrorCategory::Conflict,
            "多腿成交总数与 Filled 事件数量不一致",
        ));
    }
    let mut rows = Vec::with_capacity(
        usize::try_from(total)
            .unwrap_or(MAX_MULTI_LEG_FILL_REPORT_ROWS)
            .min(MAX_MULTI_LEG_FILL_REPORT_ROWS),
    );
    for event in events.iter().rev() {
        let Some(fill_value) = event.pointer("/kind/Filled/fill") else {
            continue;
        };
        let order_id = fill_value
            .get("order_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                multi_leg_report_error(AppErrorCategory::Conflict, "多腿成交缺少有效 order_id")
            })?;
        let instrument = order_markets.get(&order_id).ok_or_else(|| {
            multi_leg_report_error(AppErrorCategory::Conflict, "多腿成交无法关联到订单标的")
        })?;
        let mut fill = fill_value.clone();
        fill["instrument"] = Value::String(instrument.clone());
        rows.push(fill);
        if rows.len() == MAX_MULTI_LEG_FILL_REPORT_ROWS {
            break;
        }
    }
    Ok(MultiLegReportRowsV1 {
        total,
        truncated: total > rows.len() as u64,
        rows,
    })
}

/// Read a replay-verified multi-leg EventLog page using local artifact paths.
pub fn read_multi_leg_event_report_page_v1(
    request: &MultiLegEventReportReadRequestV1,
    context: &RunContext,
) -> Result<MultiLegEventReportPageV1, AppError> {
    read_multi_leg_event_report_page_v1_with_readers(
        request,
        context,
        &FileArtifactReader,
        &FileArtifactReader,
    )
}

/// Read a replay-verified multi-leg EventLog page using separate artifact and data readers.
pub fn read_multi_leg_event_report_page_v1_with_readers(
    request: &MultiLegEventReportReadRequestV1,
    context: &RunContext,
    artifact_reader: &dyn ArtifactReader,
    data_reader: &dyn ArtifactReader,
) -> Result<MultiLegEventReportPageV1, AppError> {
    if request.schema_version != MULTI_LEG_EVENT_REPORT_SCHEMA_VERSION_V1 {
        return Err(multi_leg_report_error(
            AppErrorCategory::InvalidInput,
            "多腿事件报告读取请求 schema_version 不受支持",
        ));
    }
    let page = request.page.unwrap_or(MultiLegEventPageRequestV1 {
        offset: 0,
        limit: MAX_MULTI_LEG_EVENT_REPORT_PAGE_ROWS,
    });
    read_multi_leg_report_read_model_v1_with_page_and_readers(
        &request.outcome,
        context,
        page,
        artifact_reader,
        data_reader,
    )
    .map(|model| model.report.events)
}

/// Read the complete bounded multi-leg research report using local artifact paths.
pub fn read_multi_leg_research_report_v1(
    request: &MultiLegResearchReportReadRequestV1,
    context: &RunContext,
) -> Result<MultiLegResearchReportV1, AppError> {
    read_multi_leg_research_report_v1_with_readers(
        request,
        context,
        &FileArtifactReader,
        &FileArtifactReader,
    )
}

/// Read the complete bounded multi-leg report through separate authorized artifact/data readers.
pub fn read_multi_leg_research_report_v1_with_readers(
    request: &MultiLegResearchReportReadRequestV1,
    context: &RunContext,
    artifact_reader: &dyn ArtifactReader,
    data_reader: &dyn ArtifactReader,
) -> Result<MultiLegResearchReportV1, AppError> {
    if request.schema_version != MULTI_LEG_RESEARCH_REPORT_SCHEMA_VERSION_V1 {
        return Err(multi_leg_report_error(
            AppErrorCategory::InvalidInput,
            "多腿研究报告读取请求 schema_version 不受支持",
        ));
    }
    let page = request.page.unwrap_or(MultiLegEventPageRequestV1 {
        offset: 0,
        limit: MAX_MULTI_LEG_EVENT_REPORT_PAGE_ROWS,
    });
    read_multi_leg_report_read_model_v1_with_page_and_readers(
        &request.outcome,
        context,
        page,
        artifact_reader,
        data_reader,
    )
    .map(|model| model.report)
}

fn validate_multi_leg_event_page_request(page: MultiLegEventPageRequestV1) -> Result<(), AppError> {
    if page.limit == 0 || page.limit > MAX_MULTI_LEG_EVENT_REPORT_PAGE_ROWS {
        return Err(multi_leg_report_error(
            AppErrorCategory::InvalidInput,
            format!(
                "多腿事件报告 page.limit 必须在 1..={MAX_MULTI_LEG_EVENT_REPORT_PAGE_ROWS} 范围内"
            ),
        ));
    }
    Ok(())
}

fn multi_leg_report_error(category: AppErrorCategory, message: impl Into<String>) -> AppError {
    AppError::new(category, message)
}

pub fn multi_leg_initial_cash_raw_v1(
    primary_initial_cash_raw: i128,
    reference_initial_cash_raw: i128,
) -> Result<i128, String> {
    primary_initial_cash_raw
        .checked_add(reference_initial_cash_raw)
        .ok_or_else(|| "多腿初始资金溢出".into())
}

/// Validate the complete persisted equity axis and derive the one canonical return/drawdown pair.
pub fn verify_multi_leg_equity_report_v1(
    equity_curve: &[Value],
    expected_initial_cash_raw: i128,
    declared_artifact_return_bps: Option<i64>,
    outcome_return_bps: i32,
) -> Result<MultiLegEquityReportFactsV1, String> {
    if equity_curve.is_empty() || equity_curve.len() > MAX_MULTI_LEG_REPORT_ROWS {
        return Err("多腿权益曲线为空或超过行数上限".into());
    }
    let mut previous_timestamp = None;
    let mut equity_raw = Vec::with_capacity(equity_curve.len());
    for row in equity_curve {
        let timestamp = row
            .get(0)
            .and_then(raw_i128)
            .filter(|timestamp| *timestamp >= 0)
            .ok_or_else(|| "多腿权益曲线包含无效时间".to_string())?;
        let equity = row
            .get(1)
            .and_then(raw_i128)
            .ok_or_else(|| "多腿权益曲线包含无效金额".to_string())?;
        if previous_timestamp.is_some_and(|previous| timestamp < previous) {
            return Err("多腿权益曲线时间顺序倒退".into());
        }
        previous_timestamp = Some(timestamp);
        equity_raw.push(equity);
    }
    let initial_equity_raw = *equity_raw
        .first()
        .ok_or_else(|| "多腿权益首点无效".to_string())?;
    let final_equity_raw = *equity_raw
        .last()
        .ok_or_else(|| "多腿权益末点无效".to_string())?;
    if initial_equity_raw != expected_initial_cash_raw {
        return Err("多腿权益首点与声明初始资金不一致".into());
    }
    let return_bps =
        crate::report_analysis::equity_total_return_bps_v1(initial_equity_raw, final_equity_raw)?;
    if declared_artifact_return_bps != Some(i64::from(return_bps))
        || outcome_return_bps != return_bps
    {
        return Err("多腿组合收益与权益曲线不一致".into());
    }
    Ok(MultiLegEquityReportFactsV1 {
        points: equity_curve.len() as u64,
        initial_equity_raw,
        final_equity_raw,
        return_bps,
        max_drawdown_bps: crate::report_analysis::equity_max_drawdown_bps_v1(&equity_raw),
    })
}

/// Validate the raw compatibility curve against its exact v5 sidecar and derive exact metrics.
pub fn verify_multi_leg_equity_report_exact_v1(
    equity_curve: &[Value],
    equity_curve_exact: &[Value],
    expected_initial_cash_raw: i128,
    declared_artifact_return_bps: Option<i64>,
    outcome_return_bps: i32,
) -> Result<MultiLegEquityReportFactsV1, String> {
    let initial = ExactDecimal::from_parts(expected_initial_cash_raw, 9)
        .map_err(|error| format!("多腿初始精确权益无效: {error}"))?;
    verify_multi_leg_equity_report_exact_with_principal_v2(
        equity_curve,
        equity_curve_exact,
        expected_initial_cash_raw,
        initial,
        declared_artifact_return_bps,
        outcome_return_bps,
    )
}

/// Validate exact equity against an exact principal that may lie below the legacy raw scale.
pub fn verify_multi_leg_equity_report_exact_with_principal_v2(
    equity_curve: &[Value],
    equity_curve_exact: &[Value],
    expected_initial_cash_raw: i128,
    expected_initial_cash_exact: ExactDecimal,
    declared_artifact_return_bps: Option<i64>,
    outcome_return_bps: i32,
) -> Result<MultiLegEquityReportFactsV1, String> {
    if equity_curve.is_empty()
        || equity_curve.len() > MAX_MULTI_LEG_REPORT_ROWS
        || equity_curve_exact.len() != equity_curve.len()
    {
        return Err("多腿 exact 权益曲线为空、超限或与 raw 曲线长度不一致".into());
    }
    let mut previous_timestamp = None;
    let mut raw_values = Vec::with_capacity(equity_curve.len());
    let mut exact_values = Vec::with_capacity(equity_curve_exact.len());
    for (index, (raw_row, exact_row)) in equity_curve.iter().zip(equity_curve_exact).enumerate() {
        let timestamp = raw_row
            .get(0)
            .and_then(raw_i128)
            .filter(|timestamp| *timestamp >= 0)
            .ok_or_else(|| "多腿权益曲线包含无效时间".to_string())?;
        let equity_raw = raw_row
            .get(1)
            .and_then(raw_i128)
            .ok_or_else(|| "多腿权益曲线包含无效金额".to_string())?;
        if previous_timestamp.is_some_and(|previous| timestamp < previous) {
            return Err("多腿权益曲线时间顺序倒退".into());
        }
        previous_timestamp = Some(timestamp);

        let exact_index = exact_row
            .get(0)
            .and_then(Value::as_u64)
            .ok_or_else(|| "多腿 exact 权益曲线包含无效索引".to_string())?;
        let exact_timestamp = exact_row
            .get(1)
            .and_then(raw_i128)
            .ok_or_else(|| "多腿 exact 权益曲线包含无效时间".to_string())?;
        let exact_text = exact_row
            .get(2)
            .and_then(Value::as_str)
            .ok_or_else(|| "多腿 exact 权益曲线金额必须为十进制字符串".to_string())?;
        let exact = ExactDecimal::parse(exact_text)
            .map_err(|error| format!("多腿 exact 权益金额无效: {error}"))?;
        if exact_index != index as u64
            || exact_timestamp != timestamp
            || exact.to_string() != exact_text
        {
            return Err("多腿 exact 权益曲线索引、时间或十进制规范化不一致".into());
        }
        let raw_projection = exact
            .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
            .and_then(|value| value.to_raw(9).ok())
            .ok_or_else(|| "多腿 exact 权益无法投影到 raw 兼容值".to_string())?;
        if raw_projection != equity_raw {
            return Err("多腿 exact 权益与 raw 兼容曲线不一致".into());
        }
        raw_values.push(equity_raw);
        exact_values.push(exact);
    }

    let initial_equity_raw = *raw_values
        .first()
        .ok_or_else(|| "多腿权益首点无效".to_string())?;
    let final_equity_raw = *raw_values
        .last()
        .ok_or_else(|| "多腿权益末点无效".to_string())?;
    let expected_projection = expected_initial_cash_exact
        .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
        .and_then(|value| value.to_raw(9).ok());
    if expected_projection != Some(expected_initial_cash_raw)
        || initial_equity_raw != expected_initial_cash_raw
        || exact_values.first().copied() != Some(expected_initial_cash_exact)
    {
        return Err("多腿权益首点与声明初始资金不一致".into());
    }
    let initial_exact = exact_values[0];
    let final_exact = *exact_values.last().ok_or("多腿 exact 权益末点无效")?;
    let return_bps =
        crate::report_analysis::exact_equity_total_return_bps_v1(initial_exact, final_exact)?;
    if declared_artifact_return_bps != Some(i64::from(return_bps))
        || outcome_return_bps != return_bps
    {
        return Err("多腿组合精确收益与权益曲线不一致".into());
    }
    Ok(MultiLegEquityReportFactsV1 {
        points: equity_curve.len() as u64,
        initial_equity_raw,
        final_equity_raw,
        return_bps,
        max_drawdown_bps: crate::report_analysis::exact_equity_max_drawdown_bps_v1(&exact_values)?,
    })
}

/// Verify a leg's persisted fill count and outcome digest against replay-derived fills.
pub fn verify_multi_leg_market_fill_report_v1(
    instrument: &str,
    fills: &[Fill],
    outcome_fill_count: u64,
    outcome_result_hash: &str,
    artifact_fill_count: Option<u64>,
    artifact_result_hash: Option<&str>,
) -> Result<MultiLegMarketFillReportFactsV1, String> {
    if instrument.trim().is_empty() {
        return Err("多腿成交标的不能为空".into());
    }
    let fill_count = u64::try_from(fills.len()).map_err(|_| "多腿成交数量超出 u64".to_string())?;
    let mut hasher = Fnv1a::new();
    hasher.write_text(instrument);
    hasher.write_text(
        &serde_json::to_string(fills).map_err(|error| format!("多腿成交序列化失败: {error}"))?,
    );
    let result_hash = format!("{:016x}", hasher.finish());
    if fill_count != outcome_fill_count
        || artifact_fill_count != Some(outcome_fill_count)
        || artifact_result_hash != Some(outcome_result_hash)
        || result_hash != outcome_result_hash
    {
        return Err(format!("{instrument} 成交数或结果指纹与 EventLog 不一致"));
    }
    Ok(MultiLegMarketFillReportFactsV1 {
        instrument: instrument.to_owned(),
        fills: fill_count,
        result_hash,
    })
}

/// Sum pending-reconciliation turnover with checked arithmetic.
pub fn multi_leg_pending_turnover_raw_v1(pending_reconcile: &Value) -> Result<i128, String> {
    let items = pending_reconcile
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "多腿产物缺少 pending_reconcile.items".to_string())?;
    items.iter().try_fold(0_i128, |sum, pending| {
        pending
            .get("turnover_raw")
            .and_then(raw_i128)
            .and_then(|value| sum.checked_add(value))
            .ok_or_else(|| "多腿未配对成交额无效或溢出".to_string())
    })
}

/// Verify and consolidate the multi-leg accounting facts that report surfaces must not recompute.
pub fn verify_multi_leg_research_report_facts_v1(
    input: MultiLegResearchReportInputV1<'_>,
) -> Result<MultiLegResearchReportFactsV1, String> {
    input
        .spec
        .validate()
        .map_err(|error| format!("多腿 spec 验证失败: {error}"))?;
    if !matches!(
        input.artifact.get("schema_version").and_then(Value::as_u64),
        Some(3 | 4 | 5 | 6)
    ) {
        return Err("多腿回测产物 schema_version 不受支持".into());
    }
    let artifact_version = input.artifact.get("schema_version").and_then(Value::as_u64);
    if (input.spec.schema_version == crate::MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION)
        != (artifact_version == Some(6))
    {
        return Err("多腿 exact principal spec 与 v6 产物版本不一致".into());
    }
    if input.spec.run_id != input.outcome.run_id {
        return Err("多腿 outcome 与 spec run_id 不一致".into());
    }
    let instruments = [
        input.outcome.primary_instrument.as_str(),
        input.outcome.reference_instrument.as_str(),
    ];
    if instruments[0].trim().is_empty() || instruments[0] == instruments[1] {
        return Err("多腿报告必须声明两个不同的有效标的".into());
    }
    let instrument_ids = instruments
        .iter()
        .map(|instrument| {
            InstrumentId::parse(instrument)
                .filter(|parsed| parsed.to_string() == *instrument)
                .ok_or_else(|| format!("多腿标的 ID 非规范或无效: {instrument}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if input.artifact.get("run_id").and_then(Value::as_str) != Some(input.outcome.run_id.as_str())
        || input
            .artifact
            .get("primary_instrument")
            .and_then(Value::as_str)
            != Some(instruments[0])
        || input
            .artifact
            .get("reference_instrument")
            .and_then(Value::as_str)
            != Some(instruments[1])
    {
        return Err("多腿结果与持久产物身份不一致".into());
    }
    let instrument_set = instruments
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let events = input
        .artifact
        .get("shared_event_log")
        .and_then(Value::as_array)
        .filter(|events| !events.is_empty() && events.len() <= MAX_MULTI_LEG_REPORT_ROWS)
        .ok_or_else(|| "多腿共享 EventLog 为空或超过行数上限".to_string())?;
    if matches!(
        input.artifact.get("schema_version").and_then(Value::as_u64),
        Some(4 | 5 | 6)
    ) {
        if input
            .artifact
            .get("event_hash")
            .and_then(Value::as_str)
            .is_none_or(|value| {
                value.len() != 16 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err("多腿产物 event_hash 格式无效".into());
        }
        let event_start = events
            .iter()
            .filter_map(|event| event.get("ts").and_then(Value::as_u64))
            .min();
        let event_end = events
            .iter()
            .filter_map(|event| event.get("ts").and_then(Value::as_u64))
            .max();
        if input.artifact.get("clock_start").and_then(Value::as_u64) != event_start
            || input.artifact.get("clock_end").and_then(Value::as_u64) != event_end
        {
            return Err("多腿 RunManifest clock 与共享 EventLog 时间范围不一致".into());
        }
    }
    let expected_digest = input
        .artifact
        .get("shared_result_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| "多腿产物缺少 shared_result_hash".to_string())?;
    let expected_ledger_entries = input
        .artifact
        .get("ledger_entries")
        .cloned()
        .unwrap_or(Value::Null);
    let replay = crate::verify_event_replay_v1(events, expected_digest, &expected_ledger_entries)
        .map_err(|error| error.to_string())?;
    if input.artifact.get("replay_events").and_then(Value::as_u64) != Some(replay.event_count)
        || input
            .artifact
            .get("replay_ledger_entries")
            .and_then(Value::as_u64)
            != Some(replay.ledger.entries().len() as u64)
    {
        return Err("多腿 EventLog digest 或重放账簿与产物不一致".into());
    }
    if matches!(
        input.artifact.get("schema_version").and_then(Value::as_u64),
        Some(4 | 5 | 6)
    ) && input.artifact.get("event_hash").and_then(Value::as_str) != Some(replay.digest.as_str())
    {
        return Err("多腿 RunManifest event_hash 与 EventLog 重放摘要不一致".into());
    }
    let order_markets = multi_leg_order_markets(events, &instrument_set)?;
    let fills_by_market = multi_leg_fills_by_market(events, &order_markets, &instrument_set)?;
    let ledger = &replay.ledger;

    let (initial_cash_exact, initial_cash_raw) = input
        .spec
        .combined_initial_cash()
        .map_err(|error| error.to_string())?;
    if input.artifact.get("schema_version").and_then(Value::as_u64) == Some(6) {
        let (primary_exact, reference_exact) = input
            .spec
            .initial_cash_exact_values()
            .map_err(|error| error.to_string())?;
        let legs = input
            .artifact
            .get("leg_initial_cash_exact")
            .and_then(Value::as_object)
            .ok_or_else(|| "多腿 v6 产物缺少 exact 腿级本金".to_string())?;
        if input
            .artifact
            .get("initial_cash_exact")
            .and_then(Value::as_str)
            != Some(initial_cash_exact.to_string().as_str())
            || legs.get("primary").and_then(Value::as_str)
                != Some(primary_exact.to_string().as_str())
            || legs.get("reference").and_then(Value::as_str)
                != Some(reference_exact.to_string().as_str())
        {
            return Err("多腿 v6 exact 本金与规范 spec 不一致".into());
        }
    }
    let account_id = input
        .spec
        .account_id
        .clone()
        .unwrap_or_else(|| format!("multi-leg-{}", input.spec.run_id));
    if input
        .outcome
        .account_id
        .as_deref()
        .is_some_and(|outcome_account_id| outcome_account_id != account_id)
    {
        return Err("多腿 outcome.account_id 与请求账户不一致".into());
    }
    let declared_account_ledger = input
        .artifact
        .get("account_ledger")
        .ok_or_else(|| "多腿产物缺少共享账户账簿摘要".to_string())?;
    let account_ledger_summary =
        if input.spec.schema_version == crate::MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION {
            super::verify_multi_leg_shared_account_ledger_exact_v2(
                &account_id,
                initial_cash_raw,
                initial_cash_exact,
                &instrument_ids,
                ledger,
                declared_account_ledger,
            )?
        } else {
            super::verify_multi_leg_shared_account_ledger_v1(
                &account_id,
                initial_cash_raw,
                &instrument_ids,
                ledger,
                declared_account_ledger,
            )?
        };

    let effective_specs = input
        .artifact
        .get("effective_market_specs")
        .and_then(Value::as_object)
        .ok_or_else(|| "多腿产物缺少有效市场规格".to_string())?;
    let primary_market_spec = parse_market_spec(effective_specs, "primary")?;
    let reference_market_spec = parse_market_spec(effective_specs, "reference")?;
    primary_market_spec
        .validate()
        .map_err(|error| format!("多腿主腿有效市场规格非法: {error:?}"))?;
    reference_market_spec
        .validate()
        .map_err(|error| format!("多腿参考腿有效市场规格非法: {error:?}"))?;
    let expected_primary_market_spec =
        input.spec.primary_market_spec.clone().unwrap_or_else(|| {
            TradingInstrumentSpec::spot_default(&instrument_ids[0], &input.spec.settlement_currency)
        });
    let expected_reference_market_spec =
        input.spec.reference_market_spec.clone().unwrap_or_else(|| {
            TradingInstrumentSpec::spot_default(&instrument_ids[1], &input.spec.settlement_currency)
        });
    if primary_market_spec.instrument != instrument_ids[0]
        || reference_market_spec.instrument != instrument_ids[1]
        || primary_market_spec.settlement_currency != input.spec.settlement_currency
        || reference_market_spec.settlement_currency != input.spec.settlement_currency
        || primary_market_spec != expected_primary_market_spec
        || reference_market_spec != expected_reference_market_spec
    {
        return Err("多腿有效市场规格与请求标的或冻结规格不一致".into());
    }
    let primary_fills = fills_by_market
        .get(instruments[0])
        .map(Vec::as_slice)
        .unwrap_or_default();
    let reference_fills = fills_by_market
        .get(instruments[1])
        .map(Vec::as_slice)
        .unwrap_or_default();

    let primary_fill_facts = verify_multi_leg_market_fill_report_v1(
        instruments[0],
        primary_fills,
        input.outcome.primary_fills,
        &input.outcome.primary_result_hash,
        input.artifact.get("primary_fills").and_then(Value::as_u64),
        input
            .artifact
            .get("primary_result_hash")
            .and_then(Value::as_str),
    )?;
    let reference_fill_facts = verify_multi_leg_market_fill_report_v1(
        instruments[1],
        reference_fills,
        input.outcome.reference_fills,
        &input.outcome.reference_result_hash,
        input
            .artifact
            .get("reference_fills")
            .and_then(Value::as_u64),
        input
            .artifact
            .get("reference_result_hash")
            .and_then(Value::as_str),
    )?;

    let primary_frame = input
        .frames
        .get(instruments[0])
        .ok_or_else(|| format!("多腿报告缺少主腿 BarFrame: {}", instruments[0]))?;
    let reference_frame = input
        .frames
        .get(instruments[1])
        .ok_or_else(|| format!("多腿报告缺少参考腿 BarFrame: {}", instruments[1]))?;
    if primary_frame.instrument != instrument_ids[0]
        || reference_frame.instrument != instrument_ids[1]
    {
        return Err("多腿报告 BarFrame 标的与请求 outcome 不一致".into());
    }
    let primary_funding_raw = super::multi_leg_funding_cost(ledger, &account_id, instruments[0])?;
    let reference_funding_raw = super::multi_leg_funding_cost(ledger, &account_id, instruments[1])?;
    let primary_return_facts = super::MultiLegLegReturnFacts {
        ledger,
        account_id: &account_id,
        instrument: instruments[0],
        market_spec: &primary_market_spec,
        frame: primary_frame,
        fills: primary_fills,
        funding_cost_raw: primary_funding_raw,
        initial_cash_raw: input.spec.primary_initial_cash_raw,
    };
    let reference_return_facts = super::MultiLegLegReturnFacts {
        ledger,
        account_id: &account_id,
        instrument: instruments[1],
        market_spec: &reference_market_spec,
        frame: reference_frame,
        fills: reference_fills,
        funding_cost_raw: reference_funding_raw,
        initial_cash_raw: input.spec.reference_initial_cash_raw,
    };
    let artifact_version = input.artifact.get("schema_version").and_then(Value::as_u64);
    let exact_metrics = matches!(artifact_version, Some(5 | 6));
    let (primary_return_bps, reference_return_bps) = if exact_metrics {
        let exact_leg_pnl = input
            .artifact
            .get("leg_pnl_exact")
            .and_then(Value::as_object)
            .ok_or_else(|| "多腿 v5 产物缺少 leg_pnl_exact".to_string())?;
        let primary_pnl = super::multi_leg_leg_pnl_exact(primary_return_facts)?;
        let reference_pnl = super::multi_leg_leg_pnl_exact(reference_return_facts)?;
        if exact_leg_pnl.get("primary").and_then(Value::as_str)
            != Some(primary_pnl.to_string().as_str())
            || exact_leg_pnl.get("reference").and_then(Value::as_str)
                != Some(reference_pnl.to_string().as_str())
        {
            return Err("多腿精确腿级 PnL 与重放账簿不一致".into());
        }
        if artifact_version == Some(6) {
            let (primary_initial_exact, reference_initial_exact) = input
                .spec
                .initial_cash_exact_values()
                .map_err(|error| error.to_string())?;
            (
                super::multi_leg_leg_return_bps_exact_v2(primary_pnl, primary_initial_exact)?,
                super::multi_leg_leg_return_bps_exact_v2(reference_pnl, reference_initial_exact)?,
            )
        } else {
            (
                super::multi_leg_leg_return_bps_exact(
                    primary_pnl,
                    input.spec.primary_initial_cash_raw,
                )?,
                super::multi_leg_leg_return_bps_exact(
                    reference_pnl,
                    input.spec.reference_initial_cash_raw,
                )?,
            )
        }
    } else {
        (
            super::multi_leg_leg_return_bps(primary_return_facts)?,
            super::multi_leg_leg_return_bps(reference_return_facts)?,
        )
    };
    if input
        .artifact
        .get("primary_return_bps")
        .and_then(Value::as_i64)
        != Some(i64::from(primary_return_bps))
        || input
            .artifact
            .get("reference_return_bps")
            .and_then(Value::as_i64)
            != Some(i64::from(reference_return_bps))
    {
        return Err("多腿逐腿收益与重放账簿、成交及期末标记不一致".into());
    }

    let attribution = input
        .artifact
        .get("attribution_totals")
        .ok_or_else(|| "多腿产物缺少归因总计".to_string())?;
    let read_attribution_raw = |field: &str, label: &str| {
        attribution
            .get(field)
            .and_then(raw_i128)
            .ok_or_else(|| format!("多腿归因{label}无效"))
    };
    let pending_reconcile = input
        .artifact
        .get("pending_reconcile")
        .ok_or_else(|| "多腿产物缺少 pending_reconcile".to_string())?;
    let costs = super::verify_multi_leg_cost_facts_v1(super::MultiLegCostVerificationFacts {
        primary_fills,
        reference_fills,
        primary_market_spec: &primary_market_spec,
        reference_market_spec: &reference_market_spec,
        primary_funding_raw,
        reference_funding_raw,
        declared_group_fees_raw: read_attribution_raw("fees_raw", "组级费用总计")?,
        declared_residual_fees_raw: read_attribution_raw("residual_fees_raw", "残余费用总计")?,
        declared_funding_raw: read_attribution_raw("funding_raw", "资金费总计")?,
        declared_group_turnover_raw: read_attribution_raw("turnover_raw", "组级成交额总计")?,
        declared_pending_turnover_raw: multi_leg_pending_turnover_raw_v1(pending_reconcile)?,
    })?;

    let equity_curve = input
        .artifact
        .get("equity_curve")
        .and_then(Value::as_array)
        .ok_or_else(|| "多腿产物缺少共享权益曲线".to_string())?;
    let declared_return = input
        .artifact
        .get("combined_return_bps")
        .and_then(Value::as_i64);
    let equity = if exact_metrics {
        let exact_curve = input
            .artifact
            .get("equity_curve_exact")
            .and_then(Value::as_array)
            .ok_or_else(|| "多腿 v5 产物缺少 equity_curve_exact".to_string())?;
        if artifact_version == Some(6) {
            verify_multi_leg_equity_report_exact_with_principal_v2(
                equity_curve,
                exact_curve,
                initial_cash_raw,
                initial_cash_exact,
                declared_return,
                input.outcome.combined_return_bps,
            )?
        } else {
            verify_multi_leg_equity_report_exact_v1(
                equity_curve,
                exact_curve,
                initial_cash_raw,
                declared_return,
                input.outcome.combined_return_bps,
            )?
        }
    } else {
        verify_multi_leg_equity_report_v1(
            equity_curve,
            initial_cash_raw,
            declared_return,
            input.outcome.combined_return_bps,
        )?
    };
    let total_fills = primary_fill_facts
        .fills
        .checked_add(reference_fill_facts.fills)
        .ok_or_else(|| "多腿成交数量溢出".to_string())?;
    Ok(MultiLegResearchReportFactsV1 {
        schema_version: MULTI_LEG_RESEARCH_REPORT_SCHEMA_VERSION_V1,
        account_id,
        initial_cash_raw,
        initial_cash_exact: (input.spec.schema_version
            == crate::MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION)
            .then(|| initial_cash_exact.to_string()),
        account_ledger_summary,
        event_count: replay.event_count,
        event_hash: replay.digest,
        order_markets,
        primary: MultiLegLegReportFactsV1 {
            instrument: primary_fill_facts.instrument,
            fills: primary_fill_facts.fills,
            result_hash: primary_fill_facts.result_hash,
            return_bps: primary_return_bps,
            funding_cost_raw: primary_funding_raw,
        },
        reference: MultiLegLegReportFactsV1 {
            instrument: reference_fill_facts.instrument,
            fills: reference_fill_facts.fills,
            result_hash: reference_fill_facts.result_hash,
            return_bps: reference_return_bps,
            funding_cost_raw: reference_funding_raw,
        },
        costs: MultiLegReportCostFactsV1 {
            total_fees_raw: costs.total_fees_raw,
            total_funding_raw: costs.total_funding_raw,
            total_turnover_raw: costs.total_turnover_raw,
            cost_bps: costs.cost_bps,
        },
        equity,
        total_fills,
    })
}

fn multi_leg_order_markets(
    events: &[Value],
    declared_instruments: &std::collections::BTreeSet<&str>,
) -> Result<BTreeMap<u64, String>, String> {
    let mut order_markets = BTreeMap::new();
    for event in events {
        let (Some(order_id), Some(instrument)) = (
            multi_leg_event_order_id(event),
            multi_leg_event_direct_instrument(event),
        ) else {
            continue;
        };
        if !declared_instruments.contains(instrument.as_str()) {
            return Err(format!("多腿 EventLog 含有未声明标的订单: {instrument}"));
        }
        if order_markets
            .insert(order_id, instrument.clone())
            .is_some_and(|previous| previous != instrument)
        {
            return Err(format!("多腿订单 {order_id} 关联到多个标的"));
        }
    }
    Ok(order_markets)
}

fn multi_leg_fills_by_market(
    events: &[Value],
    order_markets: &BTreeMap<u64, String>,
    declared_instruments: &std::collections::BTreeSet<&str>,
) -> Result<BTreeMap<String, Vec<Fill>>, String> {
    let mut fills_by_market = BTreeMap::<String, Vec<Fill>>::new();
    for event in events {
        let Some(fill_value) = event.pointer("/kind/Filled/fill") else {
            continue;
        };
        let order_id = fill_value
            .get("order_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| "多腿成交缺少有效 order_id".to_string())?;
        let order_instrument = order_markets
            .get(&order_id)
            .ok_or_else(|| "多腿成交无法关联到具体标的".to_string())?;
        if let Some(fill_instrument) = fill_value.get("instrument").and_then(canonical_instrument) {
            if &fill_instrument != order_instrument {
                return Err("多腿成交标的与所属订单不一致".into());
            }
        }
        if !declared_instruments.contains(order_instrument.as_str()) {
            return Err(format!(
                "多腿 EventLog 含有未声明标的成交: {order_instrument}"
            ));
        }
        let fill: Fill = serde_json::from_value(fill_value.clone())
            .map_err(|error| format!("多腿成交无效: {error}"))?;
        fills_by_market
            .entry(order_instrument.clone())
            .or_default()
            .push(fill);
    }
    Ok(fills_by_market)
}

fn multi_leg_event_order_id(event: &Value) -> Option<u64> {
    let kind = event.get("kind")?.as_object()?;
    kind.get("OrderRiskDecision")
        .and_then(|event| event.pointer("/decision/client_order_id"))
        .or_else(|| event.pointer("/kind/OrderSubmitted/order/client_id"))
        .or_else(|| event.pointer("/kind/Filled/fill/order_id"))
        .or_else(|| event.pointer("/kind/LedgerApplied/entry/order_id"))
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
                    .and_then(|payload| payload.get("client_order_id"))
            })
        })
        .and_then(Value::as_u64)
}

fn multi_leg_event_direct_instrument(event: &Value) -> Option<String> {
    let kind = event.get("kind")?.as_object()?;
    let intent_instruments = kind
        .get("StrategyDecision")
        .and_then(|event| event.pointer("/decision/intents"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|intent| intent.get("instrument").and_then(canonical_instrument))
        .collect::<std::collections::BTreeSet<_>>();
    if intent_instruments.len() == 1 {
        return intent_instruments.into_iter().next();
    }
    event
        .pointer("/kind/OrderRiskDecision/decision/instrument")
        .or_else(|| event.pointer("/kind/OrderSubmitted/order/instrument"))
        .or_else(|| event.pointer("/kind/Filled/fill/instrument"))
        .or_else(|| event.pointer("/kind/LedgerApplied/entry/instrument"))
        .or_else(|| kind.values().find_map(|payload| payload.get("instrument")))
        .and_then(canonical_instrument)
}

fn canonical_instrument(value: &Value) -> Option<String> {
    serde_json::from_value::<InstrumentId>(value.clone())
        .ok()
        .map(|instrument| instrument.to_string())
        .or_else(|| {
            value
                .as_str()
                .and_then(InstrumentId::parse)
                .map(|instrument| instrument.to_string())
        })
}

fn parse_market_spec(
    effective_specs: &serde_json::Map<String, Value>,
    leg: &str,
) -> Result<TradingInstrumentSpec, String> {
    serde_json::from_value(
        effective_specs
            .get(leg)
            .cloned()
            .ok_or_else(|| format!("多腿产物缺少{leg}腿有效市场规格"))?,
    )
    .map_err(|error| format!("多腿{leg}腿有效市场规格无效: {error}"))
}

fn raw_i128(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<i128>().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{Money, Price, Quantity, SCALE};
    use serde_json::json;

    fn fill_hash(instrument: &str, fills: &[Fill]) -> String {
        let mut hasher = Fnv1a::new();
        hasher.write_text(instrument);
        hasher.write_text(&serde_json::to_string(fills).unwrap());
        format!("{:016x}", hasher.finish())
    }

    #[test]
    fn initial_cash_sum_uses_checked_arithmetic() {
        assert_eq!(multi_leg_initial_cash_raw_v1(100, 25).unwrap(), 125);
        assert!(multi_leg_initial_cash_raw_v1(i128::MAX, 1)
            .unwrap_err()
            .contains("溢出"));
    }

    #[test]
    fn multi_leg_event_page_request_enforces_the_shared_limit() {
        assert!(
            validate_multi_leg_event_page_request(MultiLegEventPageRequestV1 {
                offset: 0,
                limit: 1,
            })
            .is_ok()
        );
        assert!(
            validate_multi_leg_event_page_request(MultiLegEventPageRequestV1 {
                offset: 0,
                limit: MAX_MULTI_LEG_EVENT_REPORT_PAGE_ROWS,
            })
            .is_ok()
        );
        assert!(
            validate_multi_leg_event_page_request(MultiLegEventPageRequestV1 {
                offset: 0,
                limit: 0,
            })
            .is_err()
        );
        assert!(
            validate_multi_leg_event_page_request(MultiLegEventPageRequestV1 {
                offset: 0,
                limit: MAX_MULTI_LEG_EVENT_REPORT_PAGE_ROWS + 1,
            })
            .is_err()
        );
    }

    #[test]
    fn multi_leg_equity_report_rows_are_bounded_and_keep_endpoints() {
        let curve = (0..2_001)
            .map(|index| json!([index, (100_000 + index).to_string()]))
            .collect::<Vec<_>>();
        let report = multi_leg_equity_report_rows_v1(&curve).unwrap();
        assert_eq!(report.total, 2_001);
        assert!(report.truncated);
        assert!(report.rows.len() <= MAX_MULTI_LEG_EQUITY_REPORT_ROWS);
        assert_eq!(report.rows.first().unwrap()["ts"], "0");
        assert_eq!(report.rows.last().unwrap()["ts"], "2000");
    }

    #[test]
    fn multi_leg_fill_report_rows_are_bounded_and_latest_first() {
        let events = (0..205)
            .map(|order_id| {
                json!({
                    "kind": {"Filled": {"fill": {"order_id": order_id, "quantity_raw": "1"}}}
                })
            })
            .collect::<Vec<_>>();
        let order_markets = (0..205)
            .map(|order_id| (order_id, "BTC-USDT".to_owned()))
            .collect::<BTreeMap<_, _>>();
        let report = multi_leg_fill_report_rows_v1(&events, &order_markets, 205).unwrap();
        assert_eq!(report.total, 205);
        assert!(report.truncated);
        assert_eq!(report.rows.len(), MAX_MULTI_LEG_FILL_REPORT_ROWS);
        assert_eq!(report.rows.first().unwrap()["order_id"], 204);
        assert_eq!(report.rows.last().unwrap()["order_id"], 5);
        assert_eq!(report.rows.first().unwrap()["instrument"], "BTC-USDT");
        assert!(multi_leg_fill_report_rows_v1(&events, &order_markets, 204).is_err());
    }

    #[test]
    fn equity_report_facts_validate_identity_order_return_and_drawdown() {
        let curve = vec![
            json!([1, (100 * SCALE).to_string()]),
            json!([2, (120 * SCALE).to_string()]),
            json!([3, (90 * SCALE).to_string()]),
            json!([4, (110 * SCALE).to_string()]),
        ];
        let facts =
            verify_multi_leg_equity_report_v1(&curve, 100 * SCALE, Some(1_000), 1_000).unwrap();
        assert_eq!(facts.points, 4);
        assert_eq!(facts.initial_equity_raw, 100 * SCALE);
        assert_eq!(facts.final_equity_raw, 110 * SCALE);
        assert_eq!(facts.return_bps, 1_000);
        assert_eq!(facts.max_drawdown_bps, 2_500);

        let mut reversed = curve.clone();
        reversed[2] = json!([0, (90 * SCALE).to_string()]);
        assert!(
            verify_multi_leg_equity_report_v1(&reversed, 100 * SCALE, Some(1_000), 1_000)
                .unwrap_err()
                .contains("时间顺序")
        );
        assert!(
            verify_multi_leg_equity_report_v1(&curve, 100 * SCALE, Some(999), 1_000)
                .unwrap_err()
                .contains("收益")
        );
        assert!(
            verify_multi_leg_equity_report_v1(&curve, 99 * SCALE, Some(1_000), 1_000)
                .unwrap_err()
                .contains("初始资金")
        );
        assert!(verify_multi_leg_equity_report_v1(&[], 100 * SCALE, Some(0), 0).is_err());
    }

    #[test]
    fn exact_equity_report_metrics_preserve_subnano_return_and_drawdown() {
        let raw_curve = vec![json!([1, "1"]), json!([2, "1"]), json!([3, "1"])];
        let exact_curve = vec![
            json!([0, 1, "0.000000001"]),
            json!([1, 2, "0.0000000012"]),
            json!([2, 3, "0.0000000010002"]),
        ];
        let facts =
            verify_multi_leg_equity_report_exact_v1(&raw_curve, &exact_curve, 1, Some(2), 2)
                .unwrap();
        assert_eq!(facts.return_bps, 2);
        assert_eq!(facts.max_drawdown_bps, 1_665);

        let mut tampered = exact_curve.clone();
        tampered[2][2] = Value::String("0.000000002".into());
        assert!(
            verify_multi_leg_equity_report_exact_v1(&raw_curve, &tampered, 1, Some(2), 2,)
                .unwrap_err()
                .contains("raw 兼容曲线")
        );
    }

    #[test]
    fn market_fill_report_facts_bind_count_and_both_persisted_hashes() {
        let fills = [Fill {
            qty: Quantity::from_raw(2 * SCALE),
            price: Price::from_raw(100 * SCALE),
            fee: Money::from_raw(SCALE),
            ..Fill::default()
        }];
        let digest = fill_hash("BTCUSDT.BINANCE", &fills);
        let facts = verify_multi_leg_market_fill_report_v1(
            "BTCUSDT.BINANCE",
            &fills,
            1,
            &digest,
            Some(1),
            Some(&digest),
        )
        .unwrap();
        assert_eq!(facts.fills, 1);
        assert_eq!(facts.result_hash, digest);
        assert!(verify_multi_leg_market_fill_report_v1(
            "BTCUSDT.BINANCE",
            &fills,
            2,
            &digest,
            Some(2),
            Some(&digest),
        )
        .is_err());
        assert!(verify_multi_leg_market_fill_report_v1(
            "BTCUSDT.BINANCE",
            &fills,
            1,
            "0000000000000000",
            Some(1),
            Some("0000000000000000"),
        )
        .is_err());
    }

    #[test]
    fn pending_reconciliation_turnover_is_checked_and_rejects_malformed_rows() {
        let rows = json!({"items": [
            {"turnover_raw": "100"},
            {"turnover_raw": 25}
        ]});
        assert_eq!(multi_leg_pending_turnover_raw_v1(&rows).unwrap(), 125);
        assert!(multi_leg_pending_turnover_raw_v1(&json!({"items": [
            {"turnover_raw": i128::MAX.to_string()}, {"turnover_raw": "1"}
        ]}))
        .unwrap_err()
        .contains("溢出"));
        assert!(
            multi_leg_pending_turnover_raw_v1(&json!({"items": [{"turnover_raw": "x"}]})).is_err()
        );
    }
}
