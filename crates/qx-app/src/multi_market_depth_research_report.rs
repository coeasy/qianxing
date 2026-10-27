//! Complete, bounded application report for shared-account multi-market L1/L2 runs.

use crate::artifact_store::{read_text_bounded, ArtifactReader, FileArtifactReader};
use crate::report_read::{
    read_multi_market_depth_event_report_read_model_v1_with_reader,
    MultiMarketDepthEventReportReadModelV1, MultiMarketDepthEventReportReadRequestV1,
    MultiMarketEventReportPageRequestV1, ResearchReportRowsV1,
    MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1,
};
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiMarketDepthBacktestOutcome,
    MultiMarketDepthBacktestSpec, RunContext, StrategyArtifactIdentity, VerificationResult,
};
use qx_xingban::OrderBookExecutionDescriptor;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const MULTI_MARKET_DEPTH_RESEARCH_REPORT_SCHEMA_VERSION_V1: u32 = 1;
pub const MAX_MULTI_MARKET_DEPTH_REPORT_EQUITY_ROWS: usize = 1_000;
pub const MAX_MULTI_MARKET_DEPTH_REPORT_FILL_ROWS: usize = 200;
pub const MAX_MULTI_MARKET_DEPTH_REPORT_SNAPSHOT_ROWS: usize = 1_000;
pub const MAX_MULTI_MARKET_DEPTH_REPORT_LEVELS_PER_SIDE: usize = 20;
pub const MAX_MULTI_MARKET_DEPTH_REPORT_MARKETS: usize = crate::MAX_MULTI_MARKET_DEPTH_MARKETS;
const MAX_REPORT_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiMarketDepthResearchReportReadRequestV1 {
    pub schema_version: u32,
    pub outcome: MultiMarketDepthBacktestOutcome,
    #[serde(default)]
    pub page: Option<MultiMarketEventReportPageRequestV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiMarketDepthResearchMarketV1 {
    pub instrument: String,
    pub source: String,
    pub content_hash: String,
    pub data_fingerprint: String,
    pub fill_count: u64,
    pub quality_status: String,
    pub dataset_quality: Option<Value>,
    pub data_tier: String,
    pub source_end_ts: u64,
    pub depth: crate::artifact_store::VerifiedDepthFramePreview,
    pub strategy_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiMarketDepthResearchEventPageV1 {
    pub schema_version: u32,
    pub run_id: String,
    pub total: u64,
    pub offset: u64,
    pub truncated: bool,
    pub rows: Vec<Value>,
    pub fill_count: u64,
    pub fill_sequence_sha256: String,
    pub replayed_ledger_entries: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiMarketDepthResearchReportV1 {
    pub schema_version: u32,
    pub run_id: String,
    pub account_id: String,
    pub instrument: String,
    pub spec_fingerprint: String,
    pub custom_strategy_identities: Vec<StrategyArtifactIdentity>,
    pub verification: VerificationResult,
    pub summary: Value,
    pub equity: ResearchReportRowsV1,
    pub fills: ResearchReportRowsV1,
    pub events: MultiMarketDepthResearchEventPageV1,
    pub markets: Vec<MultiMarketDepthResearchMarketV1>,
}

pub fn read_multi_market_depth_research_report_v1(
    request: &MultiMarketDepthResearchReportReadRequestV1,
    context: &RunContext,
) -> Result<MultiMarketDepthResearchReportV1, AppError> {
    read_multi_market_depth_research_report_v1_with_readers(
        request,
        context,
        &FileArtifactReader,
        &FileArtifactReader,
    )
}

pub fn read_multi_market_depth_research_report_v1_with_readers(
    request: &MultiMarketDepthResearchReportReadRequestV1,
    context: &RunContext,
    artifact_reader: &dyn ArtifactReader,
    data_reader: &dyn ArtifactReader,
) -> Result<MultiMarketDepthResearchReportV1, AppError> {
    context.require(CallerCapability::Research, "读取多市场深度完整研究报告")?;
    if request.schema_version != MULTI_MARKET_DEPTH_RESEARCH_REPORT_SCHEMA_VERSION_V1 {
        return Err(invalid(
            "多市场深度完整报告读取请求 schema_version 不受支持",
        ));
    }
    let event_request = MultiMarketDepthEventReportReadRequestV1 {
        schema_version: MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1,
        outcome: request.outcome.clone(),
        page: request.page,
    };
    let model = read_multi_market_depth_event_report_read_model_v1_with_reader(
        &event_request,
        context,
        artifact_reader,
    )?;
    build_report(model, &request.outcome, artifact_reader, data_reader)
}

fn build_report(
    model: MultiMarketDepthEventReportReadModelV1,
    outcome: &MultiMarketDepthBacktestOutcome,
    artifact_reader: &dyn ArtifactReader,
    data_reader: &dyn ArtifactReader,
) -> Result<MultiMarketDepthResearchReportV1, AppError> {
    let artifact_version = model
        .artifact
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| conflict("多市场深度产物缺少 schema_version"))?;
    if !matches!(artifact_version, 1 | 2 | 3 | 4) {
        return Err(conflict("多市场深度产物 schema_version 不受支持"));
    }
    if model.spec.markets.len() > MAX_MULTI_MARKET_DEPTH_REPORT_MARKETS {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            "多市场深度报告超过标的数量上限",
        ));
    }
    let market_data = model
        .artifact
        .get("market_data")
        .and_then(Value::as_array)
        .ok_or_else(|| conflict("多市场深度产物缺少 market_data"))?;
    let per_market_fills = model
        .artifact
        .get("per_market_fills")
        .and_then(Value::as_object)
        .ok_or_else(|| conflict("多市场深度产物缺少逐市场成交统计"))?;
    if market_data.len() != model.spec.markets.len()
        || per_market_fills.len() != model.spec.markets.len()
    {
        return Err(conflict("多市场深度输入/成交统计数量与 spec 不一致"));
    }
    let quality_records = quality_records_by_market(&model.artifact, market_data)?;
    verify_quality_record_set(&model.spec, &quality_records)?;
    let quality_records = quality_records
        .as_object()
        .ok_or_else(|| conflict("多市场深度质量记录必须是对象"))?;
    let market_data_by_instrument = index_market_data(market_data)?;

    let mut markets = Vec::with_capacity(model.spec.markets.len());
    let mut input_identities = Vec::with_capacity(model.spec.markets.len());
    let mut total_snapshots = 0_u64;
    let mut total_fills = 0_u64;
    let mut execution_markets = Vec::with_capacity(model.spec.markets.len());
    for configured in &model.spec.markets {
        let instrument = configured.instrument.as_str();
        let identity = market_data_by_instrument
            .get(instrument)
            .copied()
            .ok_or_else(|| conflict(format!("market_data 缺少 {instrument}")))?;
        let frame_reference = identity
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| conflict(format!("{instrument} 缺少深度输入路径")))?;
        if !data_reader.equivalent_references(frame_reference, &configured.depth_path)? {
            return Err(conflict(format!("{instrument} 深度输入路径与 spec 不一致")));
        }
        let frame_payload = read_text_bounded(
            data_reader,
            frame_reference,
            MAX_REPORT_ARTIFACT_BYTES,
            "多市场深度输入",
        )?;
        let frame = qx_xingban::DepthFrame::from_json(&frame_payload)
            .map_err(|error| conflict(format!("{instrument} DepthFrame 无效: {error}")))?;
        let quality_record = quality_records.get(instrument);
        let quality_payload = match configured.dataset_manifest_path.as_deref() {
            Some(expected_reference) => {
                let recorded_reference = quality_record
                    .and_then(|record| record.get("manifest_path"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| conflict(format!("{instrument} 质量清单记录缺少路径")))?;
                if !data_reader.equivalent_references(recorded_reference, expected_reference)? {
                    return Err(conflict(format!("{instrument} 质量清单路径与 spec 不一致")));
                }
                Some(read_text_bounded(
                    data_reader,
                    recorded_reference,
                    MAX_REPORT_ARTIFACT_BYTES,
                    "多市场深度质量清单",
                )?)
            }
            None => None,
        };
        let verified = crate::verify_multi_market_depth_input_v1(
            configured,
            identity,
            &frame_payload,
            &frame,
            quality_record,
            quality_payload.as_deref(),
        )?;
        let dataset_quality =
            depth_dataset_quality_panel(quality_record, verified.quality_fingerprint.as_deref())?;
        let content_hash = identity
            .get("content_hash")
            .and_then(Value::as_str)
            .ok_or_else(|| conflict(format!("{instrument} 深度产物缺少 content_hash")))?
            .to_owned();
        total_snapshots = total_snapshots
            .checked_add(verified.snapshot_count)
            .filter(|total| *total <= crate::artifact_store::MAX_ARTIFACT_ROWS)
            .ok_or_else(|| {
                AppError::new(AppErrorCategory::Timeout, "多市场深度快照总数超过读取上限")
            })?;
        input_identities.push(crate::MultiMarketDepthInputIdentityV1 {
            instrument: verified.instrument.clone(),
            data_tier: verified.data_tier.clone(),
            source: verified.source.clone(),
            input_hash: verified.input_hash,
            start_ts_ms: verified.start_ts_ms,
            end_ts_ms: verified.end_ts_ms,
            quality_fingerprint: verified.quality_fingerprint.clone(),
        });
        let fill_count = per_market_fills
            .get(instrument)
            .and_then(Value::as_u64)
            .ok_or_else(|| conflict(format!("{instrument} 逐市场成交数无效")))?;
        total_fills = total_fills
            .checked_add(fill_count)
            .ok_or_else(|| conflict("多市场深度成交数溢出"))?;
        let normalized_market_spec_json = serde_json::to_string(&configured.market_spec)
            .map_err(|error| invariant(format!("{instrument} 市场规格序列化失败: {error}")))?;
        let funding_events_json = serde_json::to_vec(&configured.funding_events)
            .map_err(|error| invariant(format!("{instrument} 资金费事件编码失败: {error}")))?;
        execution_markets.push(serde_json::json!({
            "instrument": instrument,
            "data_tier": verified.data_tier,
            "priority": configured.priority,
            "market_spec": configured.market_spec,
            "market_spec_json": normalized_market_spec_json,
            "fee_bps": configured.fee_bps,
            "latency_snapshots": configured.latency_snapshots,
            "queue_position_bps": configured.queue_position_bps,
            "queue_position_model": configured.queue_position_model,
            "market_impact_bps": configured.market_impact_bps,
            "funding_bps": configured.funding_bps,
            "funding_event_count": configured.funding_events.len(),
            "funding_events_sha256": qx_strategy::sha256_hex(&funding_events_json),
            "order_policy": configured.order_policy,
        }));
        markets.push(MultiMarketDepthResearchMarketV1 {
            instrument: instrument.to_owned(),
            source: verified.source,
            content_hash,
            data_fingerprint: verified.fingerprint,
            fill_count,
            quality_status: verified.quality_status,
            dataset_quality,
            data_tier: verified.data_tier,
            source_end_ts: verified.end_ts_ms,
            depth: depth_preview(&frame),
            strategy_ids: vec![configured.strategy.strategy_id.clone()],
        });
    }
    if total_fills != outcome.successful_fills {
        return Err(conflict("多市场逐市场成交数与 outcome 成交数不一致"));
    }
    let input_identity =
        crate::multi_market_depth_input_set_identity_v1(&model.spec, &input_identities)?;
    if input_identity.data_fingerprint != outcome.data_fingerprint {
        return Err(conflict("多市场深度数据指纹与 outcome 摘要不一致"));
    }

    let mut custom_strategy_identities: Vec<StrategyArtifactIdentity> = serde_json::from_value(
        model
            .artifact
            .get("strategy_identities")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    )
    .map_err(|error| conflict(format!("多市场深度策略身份无效: {error}")))?;
    custom_strategy_identities.sort_by(|left, right| left.strategy_id.cmp(&right.strategy_id));
    let spec_fingerprint =
        crate::multi_market_depth_spec_fingerprint_v1(&model.spec, &custom_strategy_identities)?;
    if spec_fingerprint != outcome.spec_fingerprint {
        return Err(conflict("多市场深度规格指纹与 outcome 摘要不一致"));
    }
    let mut identity_spec = model.spec.clone();
    identity_spec.output_dir.clear();
    for market in &mut identity_spec.markets {
        market.depth_path.clear();
        market.dataset_manifest_path = None;
    }
    let config_hash = qx_strategy::sha256_hex(
        &serde_json::to_vec(&identity_spec)
            .map_err(|error| invariant(format!("多市场深度配置编码失败: {error}")))?,
    );
    if model.manifest.run_id != outcome.run_id
        || model.manifest.config_hash != config_hash
        || model.manifest.data_fingerprint != input_identity.data_fingerprint
        || model.manifest.input_components != input_identity.input_components
        || model.manifest.result_hash != outcome.shared_result_hash
        || model.manifest.input_event_hash != input_identity.data_fingerprint
        || model.manifest.output_event_hash != outcome.shared_result_hash
        || model.manifest.strategy_version != format!("multi-market-depth:{spec_fingerprint}")
        || model.manifest.instrument_spec_version != "multi-market-depth-spec-v1"
        || model.manifest.model_fingerprint
            != qx_strategy::sha256_hex(
                b"OrderBookPortfolioBacktestEngine/OrderBookExecutionModel/v1",
            )
        || model.manifest.global_seed != 0
        || !model.manifest.determinism_mode
    {
        return Err(conflict("多市场深度 RunManifest 与输入/结果身份不一致"));
    }
    if !outcome.execution_models.is_empty() {
        let declared = model
            .spec
            .markets
            .iter()
            .map(|market| {
                Ok((
                    market.instrument.clone(),
                    market.execution_model()?.audit_descriptor(),
                ))
            })
            .collect::<Result<BTreeMap<String, OrderBookExecutionDescriptor>, AppError>>()?;
        if outcome.execution_models != declared {
            return Err(conflict("多市场深度生效撮合参数与 spec 不一致"));
        }
    }
    if model
        .artifact
        .get("replay_verified")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(conflict("多市场深度事件重放与账簿副产物不一致"));
    }

    let equity = read_required_json_sidecar(
        &model.artifact,
        "equity_path",
        artifact_reader,
        "多市场深度权益 sidecar",
    )?;
    if model.artifact.get("equity") != Some(&equity) {
        return Err(conflict("多市场深度权益 sidecar 与产物不一致"));
    }
    let equity_exact = if artifact_version >= 3 {
        let exact = read_required_json_sidecar(
            &model.artifact,
            "equity_exact_path",
            artifact_reader,
            "多市场深度精确权益 sidecar",
        )?;
        if model.artifact.get("equity_exact") != Some(&exact) {
            return Err(conflict("多市场深度精确权益 sidecar 与产物不一致"));
        }
        Some(exact)
    } else {
        None
    };
    if model.spec.schema_version == crate::MULTI_MARKET_DEPTH_BACKTEST_EXACT_SCHEMA_VERSION
        && equity_exact.is_none()
    {
        return Err(conflict("多市场深度 v2 exact 本金要求精确权益 sidecar"));
    }
    let (verified_equity, equity_rows, equity_exact_rows) =
        verify_equity(&equity, equity_exact.as_ref(), &model.spec, outcome)?;
    if artifact_version >= 4 {
        verify_exact_equity_metrics(&model.artifact, &verified_equity)?;
    }
    let fills = read_required_json_sidecar(
        &model.artifact,
        "fills_path",
        artifact_reader,
        "多市场深度成交 sidecar",
    )?;
    if model.page.fill_count != outcome.successful_fills {
        return Err(conflict("多市场深度 Filled 事件数与 outcome 成交数不一致"));
    }
    if artifact_version >= 2 {
        crate::verify_multi_market_event_fill_sidecar_v1(
            &fills,
            outcome.successful_fills,
            &model.page.fill_sequence_sha256,
        )?;
    } else {
        let legacy_events = model
            .artifact
            .get("event_log")
            .and_then(Value::as_array)
            .ok_or_else(|| conflict("多市场深度 v1 产物缺少 event_log"))?;
        crate::verify_multi_market_legacy_event_fill_sidecar_v1(
            &fills,
            legacy_events,
            outcome.successful_fills,
        )?;
    }
    let fill_rows = fills
        .as_array()
        .ok_or_else(|| conflict("多市场深度成交 sidecar 必须是数组"))?;
    let exact_trade_metrics = if artifact_version >= 4 {
        Some(verify_exact_trade_metrics(
            &model.artifact,
            fill_rows,
            &model.spec,
        )?)
    } else {
        None
    };
    let fill_preview = sample_fills(fill_rows);
    let page = MultiMarketDepthResearchEventPageV1 {
        schema_version: model.page.schema_version,
        run_id: model.page.run_id,
        total: model.page.total,
        offset: model.page.offset,
        truncated: model.page.truncated,
        rows: model.page.rows,
        fill_count: model.page.fill_count,
        fill_sequence_sha256: model.page.fill_sequence_sha256,
        replayed_ledger_entries: model.page.replayed_ledger_entries,
    };
    let mut metrics_exact = serde_json::json!({
        "equity": {
            "initial": verified_equity.initial_equity_exact.to_string(),
            "final": verified_equity.final_equity_exact.to_string(),
            "return": verified_equity.return_amount_exact.to_string(),
            "return_bps": verified_equity.return_bps.to_string(),
            "max_drawdown": verified_equity.max_drawdown_exact.to_string(),
            "max_drawdown_bps": verified_equity.max_drawdown_bps.to_string(),
            "input_precision": if artifact_version >= 4 {
                "shared-account multi-market DepthFrame v2 spec-bound Spot uses exact marks and exact fill facts; legacy frames retain the 1e-9 projection"
            } else {
                "legacy 1e-9 DepthFrame projection"
            }
        }
    });
    if let Some(financial) = exact_trade_metrics {
        metrics_exact["turnover"] = financial["turnover"].clone();
        metrics_exact["fees_by_currency"] = financial["fees_by_currency"].clone();
        metrics_exact["legacy_raw_projection"] = financial["legacy_raw_projection"].clone();
        metrics_exact["input_precision"] = financial["input_precision"].clone();
    }
    let instrument = format!(
        "portfolio[{}]",
        model
            .spec
            .markets
            .iter()
            .map(|market| market.instrument.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    let summary = serde_json::json!({
        "run_id": outcome.run_id,
        "account_id": outcome.account_id,
        "settlement_currency": model.spec.settlement_currency,
        "markets_total": outcome.markets_total,
        "execution_assumptions": {
            "settlement_currency": model.spec.settlement_currency,
            "markets": execution_markets,
            "effective_execution_models": outcome.execution_models,
        },
        "successful_fills": outcome.successful_fills,
        "fills": outcome.successful_fills,
        "pending_orders": outcome.pending_orders,
        "equity_points": verified_equity.total,
        "event_count": page.total,
        "account": {
            "account_id": outcome.account_id,
            "initial_cash_raw": verified_equity.initial_equity_raw.to_string(),
            "initial_cash_exact": verified_equity.initial_equity_exact.to_string()
        },
        "metrics": {
            "return_bps": verified_equity.return_bps,
            "max_drawdown_bps": verified_equity.max_drawdown_bps,
            "final_equity_raw": verified_equity.final_equity_raw.to_string()
        },
        "metrics_exact": metrics_exact,
        "result_hash": outcome.shared_result_hash,
        "data_fingerprint": input_identity.data_fingerprint,
        "spec_fingerprint": spec_fingerprint,
        "markets": markets.iter().map(|market| serde_json::json!({
            "instrument": market.instrument,
            "data_tier": market.data_tier,
            "data_fingerprint": market.data_fingerprint,
            "fills": market.fill_count,
            "quality_status": market.quality_status,
            "snapshot_count": market.depth.total
        })).collect::<Vec<_>>()
    });
    Ok(MultiMarketDepthResearchReportV1 {
        schema_version: MULTI_MARKET_DEPTH_RESEARCH_REPORT_SCHEMA_VERSION_V1,
        run_id: outcome.run_id.clone(),
        account_id: outcome.account_id.clone(),
        instrument,
        spec_fingerprint,
        custom_strategy_identities,
        verification: VerificationResult {
            run_id: outcome.run_id.clone(),
            result_hash: outcome.shared_result_hash.clone(),
            data_fingerprint: input_identity.data_fingerprint,
            verified: true,
            checks: vec![
                "多市场深度 outcome、spec 与 RunManifest 身份一致".into(),
                "各标的 DepthFrame 档位、来源、质量清单与数据指纹已复核".into(),
                "共享 EventLog 已完整重放且 Filled 与成交 sidecar 一致".into(),
                "共享权益曲线、收益和最大回撤已复算".into(),
            ],
            mismatches: Vec::new(),
        },
        summary,
        equity: sample_equity(&equity_rows, &equity_exact_rows),
        fills: fill_preview,
        events: page,
        markets,
    })
}

struct VerifiedEquity {
    total: u64,
    initial_equity_raw: i128,
    final_equity_raw: i128,
    initial_equity_exact: qx_core::ExactDecimal,
    final_equity_exact: qx_core::ExactDecimal,
    return_amount_exact: qx_core::ExactDecimal,
    max_drawdown_exact: qx_core::ExactDecimal,
    return_bps: i32,
    max_drawdown_bps: u32,
}

fn verify_equity(
    value: &Value,
    exact_value: Option<&Value>,
    spec: &MultiMarketDepthBacktestSpec,
    outcome: &MultiMarketDepthBacktestOutcome,
) -> Result<
    (
        VerifiedEquity,
        Vec<(u64, i128)>,
        Vec<(u64, qx_core::ExactDecimal)>,
    ),
    AppError,
> {
    let rows = value
        .as_array()
        .filter(|rows| {
            !rows.is_empty() && rows.len() as u64 <= crate::artifact_store::MAX_ARTIFACT_ROWS
        })
        .ok_or_else(|| conflict("多市场深度权益曲线为空或超过读取上限"))?;
    if rows.len() as u64 != outcome.equity_points {
        return Err(conflict("多市场深度权益点数与结果摘要不一致"));
    }
    let parsed = rows
        .iter()
        .map(|row| {
            let pair = row
                .as_array()
                .filter(|pair| pair.len() == 2)
                .ok_or_else(|| conflict("多市场深度权益行必须包含时间和权益"))?;
            let ts = pair[0]
                .as_u64()
                .or_else(|| pair[0].as_str().and_then(|text| text.parse().ok()))
                .filter(|ts| *ts > 0)
                .ok_or_else(|| conflict("多市场深度权益时间无效"))?;
            let equity = raw_i128(&pair[1]).ok_or_else(|| conflict("多市场深度权益金额无效"))?;
            Ok((ts, equity))
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    if parsed.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(conflict("多市场深度权益时间必须严格递增"));
    }
    let initial = parsed.first().map(|row| row.1).unwrap_or_default();
    let final_equity = parsed.last().map(|row| row.1).unwrap_or_default();
    if initial != spec.initial_cash_raw || final_equity != outcome.final_equity_raw {
        return Err(conflict("多市场深度权益曲线与初始资金/期末权益不一致"));
    }
    let exact_initial = spec
        .initial_cash_exact_value()
        .map_err(|error| conflict(format!("多市场深度精确初始资金无效: {error}")))?;
    let exact_rows = match exact_value {
        Some(value) => {
            let rows = value
                .as_array()
                .filter(|rows| rows.len() == parsed.len())
                .ok_or_else(|| conflict("多市场深度精确权益点数与 raw 曲线不一致"))?;
            rows.iter()
                .enumerate()
                .map(|(index, row)| {
                    let equity_index = row
                        .get("equity_index")
                        .and_then(Value::as_u64)
                        .and_then(|value| usize::try_from(value).ok())
                        .ok_or_else(|| conflict("多市场深度精确权益 index 无效"))?;
                    let ts = row
                        .get("ts")
                        .and_then(Value::as_u64)
                        .or_else(|| {
                            row.get("ts")
                                .and_then(Value::as_str)
                                .and_then(|value| value.parse().ok())
                        })
                        .filter(|value| *value > 0)
                        .ok_or_else(|| conflict("多市场深度精确权益时间无效"))?;
                    let encoded = row
                        .get("equity_exact")
                        .and_then(Value::as_str)
                        .ok_or_else(|| conflict("多市场深度精确权益金额缺失"))?;
                    let exact = qx_core::ExactDecimal::parse(encoded)
                        .map_err(|_| conflict("多市场深度精确权益金额格式无效"))?;
                    if exact.to_string() != encoded {
                        return Err(conflict("多市场深度精确权益金额必须采用规范格式"));
                    }
                    if equity_index != index || ts != parsed[index].0 {
                        return Err(conflict("多市场深度精确权益 index/time 与 raw 曲线不一致"));
                    }
                    if spec.schema_version
                        == crate::MULTI_MARKET_DEPTH_BACKTEST_EXACT_SCHEMA_VERSION
                        && exact_cash_raw_projection(exact) != Some(parsed[index].1)
                    {
                        return Err(conflict(
                            "多市场深度 v2 精确权益 raw 投影与兼容权益曲线不一致",
                        ));
                    }
                    Ok((ts, exact))
                })
                .collect::<Result<Vec<_>, AppError>>()?
        }
        None => parsed
            .iter()
            .map(|(ts, raw)| {
                qx_core::ExactDecimal::from_parts(*raw, 9)
                    .map(|value| (*ts, value))
                    .map_err(|error| invariant(format!("多市场深度 raw 权益投影无效: {error}")))
            })
            .collect::<Result<Vec<_>, AppError>>()?,
    };
    let exact_curve = exact_rows
        .iter()
        .map(|(_, value)| *value)
        .collect::<Vec<_>>();
    let final_equity_exact = exact_curve.last().copied().unwrap_or(exact_initial);
    if exact_curve.first().copied() != Some(exact_initial) {
        return Err(conflict("多市场深度精确权益曲线初值与初始资金不一致"));
    }
    let return_bps =
        crate::report_analysis::exact_equity_total_return_bps_v1(exact_initial, final_equity_exact)
            .map_err(conflict)?;
    let max_drawdown_bps =
        crate::report_analysis::exact_equity_max_drawdown_bps_v1(&exact_curve).map_err(conflict)?;
    if return_bps != outcome.return_bps || max_drawdown_bps != outcome.max_drawdown_bps {
        return Err(conflict("多市场深度收益或最大回撤与权益曲线不一致"));
    }
    let return_amount_exact = final_equity_exact
        .checked_sub(exact_initial)
        .ok_or_else(|| conflict("多市场深度精确收益金额溢出"))?;
    let mut exact_peak = exact_initial;
    let mut max_drawdown_exact = qx_core::ExactDecimal::ZERO;
    for equity in &exact_curve {
        if exact_peak
            .checked_cmp(*equity)
            .is_some_and(|ordering| ordering.is_lt())
        {
            exact_peak = *equity;
        }
        let drawdown = exact_peak
            .checked_sub(*equity)
            .ok_or_else(|| conflict("多市场深度精确回撤金额溢出"))?;
        if drawdown
            .checked_cmp(max_drawdown_exact)
            .is_some_and(|ordering| ordering.is_gt())
        {
            max_drawdown_exact = drawdown;
        }
    }
    Ok((
        VerifiedEquity {
            total: parsed.len() as u64,
            initial_equity_raw: initial,
            final_equity_raw: final_equity,
            initial_equity_exact: exact_initial,
            final_equity_exact,
            return_amount_exact,
            max_drawdown_exact,
            return_bps,
            max_drawdown_bps,
        },
        parsed,
        exact_rows,
    ))
}

fn verify_exact_equity_metrics(
    artifact: &Value,
    verified: &VerifiedEquity,
) -> Result<(), AppError> {
    let equity = artifact
        .get("metrics_exact")
        .and_then(Value::as_object)
        .ok_or_else(|| conflict("多市场深度 v4 产物缺少精确权益指标"))?;
    let expected = [
        ("initial", verified.initial_equity_exact.to_string()),
        ("final", verified.final_equity_exact.to_string()),
        ("return", verified.return_amount_exact.to_string()),
        ("return_bps", verified.return_bps.to_string()),
        ("max_drawdown", verified.max_drawdown_exact.to_string()),
        ("max_drawdown_bps", verified.max_drawdown_bps.to_string()),
    ];
    for (field, expected) in expected {
        let actual = equity
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| conflict(format!("多市场深度精确权益指标缺少 {field}")))?;
        if actual != expected {
            return Err(conflict(format!(
                "多市场深度精确权益指标 {field} 与权益曲线重算结果不一致"
            )));
        }
    }
    Ok(())
}

fn exact_cash_raw_projection(amount: qx_core::ExactDecimal) -> Option<i128> {
    amount
        .checked_mul_at_scale(
            qx_core::ExactDecimal::ONE,
            9,
            qx_core::DecimalRounding::TowardZero,
        )
        .and_then(|projected| projected.to_raw(9).ok())
}

fn verify_exact_trade_metrics(
    artifact: &Value,
    fill_rows: &[Value],
    spec: &MultiMarketDepthBacktestSpec,
) -> Result<Value, AppError> {
    let metrics = artifact
        .get("metrics_exact")
        .and_then(Value::as_object)
        .ok_or_else(|| conflict("多市场深度 v4 产物缺少精确交易指标"))?;
    let mut fees_by_currency = BTreeMap::<String, qx_core::ExactDecimal>::new();
    let mut turnover = qx_core::ExactDecimal::ZERO;
    let mut fees_raw = 0_i128;
    let mut turnover_raw = 0_i128;
    let market_specs = spec
        .markets
        .iter()
        .map(|market| {
            let market_spec = market.market_spec.clone();
            Ok((market.instrument.clone(), market_spec))
        })
        .collect::<Result<BTreeMap<_, _>, AppError>>()?;
    let ledger_entries: Vec<qx_core::LedgerEntry> = serde_json::from_value(
        artifact
            .get("ledger_entries")
            .cloned()
            .ok_or_else(|| conflict("多市场深度 v4 产物缺少账簿 entries"))?,
    )
    .map_err(|error| conflict(format!("多市场深度账簿 entries 无效: {error}")))?;
    let mut instrument_by_order = BTreeMap::<u64, String>::new();
    for entry in &ledger_entries {
        if entry.kind == qx_core::LedgerEntryKind::TradePosition {
            if let (Some(order_id), Some(instrument)) = (entry.order_id, entry.instrument.as_ref())
            {
                instrument_by_order.insert(order_id, instrument.to_string());
            }
        }
    }

    for row in fill_rows {
        let fill: qx_core::Fill = serde_json::from_value(row.clone())
            .map_err(|error| conflict(format!("多市场深度成交格式无效: {error}")))?;
        fill.validate_exact_amounts()
            .map_err(|error| conflict(format!("多市场深度成交金额无效: {error}")))?;
        let instrument_value = instrument_by_order
            .get(&fill.order_id)
            .ok_or_else(|| conflict("多市场深度成交找不到对应账簿订单标的"))?;
        let market_spec = market_specs
            .get(instrument_value)
            .ok_or_else(|| conflict("多市场深度成交引用了未配置标的"))?;
        let exact_turnover = market_spec
            .notional_exact(fill.quantity_decimal(), fill.price_decimal())
            .map_err(|error| conflict(format!("多市场深度成交额重算失败: {error}")))?;
        turnover = turnover
            .checked_add(exact_turnover)
            .ok_or_else(|| conflict("多市场深度精确成交额累计溢出"))?;
        turnover_raw = turnover_raw
            .checked_add(exact_raw_projection(exact_turnover)?)
            .ok_or_else(|| conflict("多市场深度成交额兼容投影累计溢出"))?;

        if !fill.exact_fees.is_empty() {
            for fee in &fill.exact_fees {
                let currency = fee
                    .currency
                    .as_deref()
                    .unwrap_or(&spec.settlement_currency)
                    .trim()
                    .to_ascii_uppercase();
                if currency.is_empty() {
                    return Err(conflict("多市场深度精确手续费缺少币种"));
                }
                let total = fees_by_currency
                    .entry(currency.clone())
                    .or_insert(qx_core::ExactDecimal::ZERO);
                *total = total
                    .checked_add(fee.amount)
                    .ok_or_else(|| conflict("多市场深度精确手续费累计溢出"))?;
                if currency == spec.settlement_currency {
                    fees_raw = fees_raw
                        .checked_add(exact_raw_projection(fee.amount)?)
                        .ok_or_else(|| conflict("多市场深度手续费兼容投影累计溢出"))?;
                }
            }
        } else {
            for fee in fill
                .fee_charges()
                .map_err(|error| conflict(format!("多市场深度手续费无效: {error}")))?
            {
                let currency = fee
                    .currency
                    .as_deref()
                    .unwrap_or(&spec.settlement_currency)
                    .trim()
                    .to_ascii_uppercase();
                let amount = qx_core::ExactDecimal::from_parts(fee.amount.raw(), 9)
                    .map_err(|error| conflict(format!("多市场深度手续费金额无效: {error}")))?;
                let total = fees_by_currency
                    .entry(currency)
                    .or_insert(qx_core::ExactDecimal::ZERO);
                *total = total
                    .checked_add(amount)
                    .ok_or_else(|| conflict("多市场深度手续费累计溢出"))?;
            }
            fees_raw = fees_raw
                .checked_add(fill.fee.raw())
                .ok_or_else(|| conflict("多市场深度手续费兼容投影累计溢出"))?;
        }
    }

    let reported_turnover = metrics
        .get("turnover")
        .ok_or_else(|| conflict("多市场深度 v4 产物缺少成交额"))?;
    let turnover_amount = reported_turnover
        .get("amount")
        .and_then(Value::as_str)
        .ok_or_else(|| conflict("多市场深度精确成交额格式无效"))?;
    let turnover_currency = reported_turnover
        .get("currency")
        .and_then(Value::as_str)
        .ok_or_else(|| conflict("多市场深度精确成交额缺少币种"))?;
    let reported_turnover_exact = qx_core::ExactDecimal::parse(turnover_amount)
        .map_err(|_| conflict("多市场深度精确成交额格式无效"))?;
    if reported_turnover_exact != turnover || turnover_currency != spec.settlement_currency {
        return Err(conflict("多市场深度精确成交额与逐笔成交/结算币种不一致"));
    }

    let reported_fees = metrics
        .get("fees_by_currency")
        .and_then(Value::as_object)
        .ok_or_else(|| conflict("多市场深度 v4 产物缺少按币种手续费"))?;
    let normalized_reported_fees = reported_fees
        .iter()
        .map(|(currency, value)| {
            let encoded = value
                .as_str()
                .ok_or_else(|| conflict("多市场深度手续费金额必须是精确十进制字符串"))?;
            let amount = qx_core::ExactDecimal::parse(encoded)
                .map_err(|_| conflict("多市场深度手续费金额格式无效"))?;
            if amount.to_string() != encoded {
                return Err(conflict("多市场深度手续费金额必须采用规范格式"));
            }
            Ok((currency.to_ascii_uppercase(), amount))
        })
        .collect::<Result<BTreeMap<_, _>, AppError>>()?;
    if normalized_reported_fees != fees_by_currency {
        return Err(conflict("多市场深度按币种手续费与成交明细不一致"));
    }

    let raw_projection = metrics
        .get("legacy_raw_projection")
        .ok_or_else(|| conflict("多市场深度 v4 产物缺少兼容指标投影"))?;
    let reported_fees_raw = raw_projection
        .get("fees_raw")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<i128>().ok())
        .ok_or_else(|| conflict("多市场深度 fees_raw 投影无效"))?;
    let reported_turnover_raw = raw_projection
        .get("turnover_raw")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<i128>().ok())
        .ok_or_else(|| conflict("多市场深度 turnover_raw 投影无效"))?;
    if reported_fees_raw != fees_raw || reported_turnover_raw != turnover_raw {
        return Err(conflict("多市场深度兼容指标投影与逐笔成交不一致"));
    }
    let input_precision = metrics
        .get("input_precision")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| conflict("多市场深度 v4 产物缺少精度口径说明"))?;

    let mut ledger_fees = BTreeMap::<String, qx_core::ExactDecimal>::new();
    for entry in &ledger_entries {
        if entry.kind != qx_core::LedgerEntryKind::Fee {
            continue;
        }
        let amount = entry
            .exact_amounts
            .map(|values| values.amount)
            .or_else(|| qx_core::ExactDecimal::from_parts(entry.amount.raw(), 9).ok())
            .ok_or_else(|| conflict("多市场深度账簿手续费金额无效"))?;
        let positive_fee = amount
            .checked_neg()
            .ok_or_else(|| conflict("多市场深度账簿手续费取反溢出"))?;
        let total = ledger_fees
            .entry(entry.currency.to_ascii_uppercase())
            .or_insert(qx_core::ExactDecimal::ZERO);
        *total = total
            .checked_add(positive_fee)
            .ok_or_else(|| conflict("多市场深度账簿手续费累计溢出"))?;
    }
    if ledger_fees != fees_by_currency {
        return Err(conflict("多市场深度账簿手续费与成交明细不一致"));
    }

    Ok(serde_json::json!({
        "turnover": { "amount": turnover.to_string(), "currency": turnover_currency },
        "fees_by_currency": fees_by_currency.iter().map(|(currency, amount)| (currency.clone(), amount.to_string())).collect::<BTreeMap<_, _>>(),
        "legacy_raw_projection": { "fees_raw": fees_raw.to_string(), "turnover_raw": turnover_raw.to_string() },
        "input_precision": input_precision,
    }))
}

fn exact_raw_projection(value: qx_core::ExactDecimal) -> Result<i128, AppError> {
    value
        .checked_mul_at_scale(
            qx_core::ExactDecimal::ONE,
            9,
            qx_core::DecimalRounding::TowardZero,
        )
        .and_then(|projected| projected.to_raw(9).ok())
        .ok_or_else(|| conflict("多市场深度精确指标 raw 投影溢出"))
}

fn depth_preview(
    frame: &qx_xingban::DepthFrame,
) -> crate::artifact_store::VerifiedDepthFramePreview {
    let total = frame.snapshots.len() as u64;
    let indexes = evenly_spaced_indexes(
        frame.snapshots.len(),
        MAX_MULTI_MARKET_DEPTH_REPORT_SNAPSHOT_ROWS,
    );
    let rows = indexes
        .into_iter()
        .map(|index| {
            let snapshot = &frame.snapshots[index];
            let levels = |source: &[qx_xingban::BookLevel]| {
                source
                    .iter()
                    .take(MAX_MULTI_MARKET_DEPTH_REPORT_LEVELS_PER_SIDE)
                    .map(|level| crate::artifact_store::VerifiedDepthLevel {
                        price_raw: level.price.raw().to_string(),
                        qty_raw: level.qty.raw().to_string(),
                    })
                    .collect::<Vec<_>>()
            };
            crate::artifact_store::VerifiedDepthFramePoint {
                ts: snapshot.ts,
                sequence: snapshot.sequence,
                bids: levels(&snapshot.bids),
                asks: levels(&snapshot.asks),
                levels_truncated: snapshot.bids.len()
                    > MAX_MULTI_MARKET_DEPTH_REPORT_LEVELS_PER_SIDE
                    || snapshot.asks.len() > MAX_MULTI_MARKET_DEPTH_REPORT_LEVELS_PER_SIDE,
            }
        })
        .collect::<Vec<_>>();
    crate::artifact_store::VerifiedDepthFramePreview {
        total,
        truncated: rows.len() < total as usize || rows.iter().any(|row| row.levels_truncated),
        rows,
    }
}

fn sample_equity(
    rows: &[(u64, i128)],
    exact_rows: &[(u64, qx_core::ExactDecimal)],
) -> ResearchReportRowsV1 {
    let indexes = evenly_spaced_indexes(rows.len(), MAX_MULTI_MARKET_DEPTH_REPORT_EQUITY_ROWS);
    ResearchReportRowsV1 {
        total: rows.len() as u64,
        truncated: rows.len() > MAX_MULTI_MARKET_DEPTH_REPORT_EQUITY_ROWS,
        rows: indexes
            .into_iter()
            .map(|index| {
                serde_json::json!({
                    "ts": rows[index].0.to_string(),
                    "equity_raw": rows[index].1.to_string(),
                    "equity_exact": exact_rows[index].1.to_string()
                })
            })
            .collect(),
    }
}

fn sample_fills(rows: &[Value]) -> ResearchReportRowsV1 {
    let sampled = rows
        .iter()
        .rev()
        .take(MAX_MULTI_MARKET_DEPTH_REPORT_FILL_ROWS)
        .cloned()
        .collect::<Vec<_>>();
    ResearchReportRowsV1 {
        total: rows.len() as u64,
        truncated: rows.len() > MAX_MULTI_MARKET_DEPTH_REPORT_FILL_ROWS,
        rows: sampled,
    }
}

fn evenly_spaced_indexes(length: usize, max_rows: usize) -> Vec<usize> {
    let count = length.min(max_rows);
    if count == 0 {
        return Vec::new();
    }
    if count == length || count == 1 {
        return (0..count).collect();
    }
    (0..count)
        .map(|index| {
            (u128::from(index as u64) * u128::from((length - 1) as u64)
                / u128::from((count - 1) as u64)) as usize
        })
        .collect()
}

fn verify_quality_record_set(
    spec: &MultiMarketDepthBacktestSpec,
    quality_records: &Value,
) -> Result<(), AppError> {
    let records = quality_records
        .as_object()
        .ok_or_else(|| conflict("多市场深度数据质量记录必须是对象"))?;
    let expected = spec
        .markets
        .iter()
        .filter(|market| market.dataset_manifest_path.is_some())
        .map(|market| market.instrument.clone())
        .collect::<BTreeSet<_>>();
    let actual = records.keys().cloned().collect::<BTreeSet<_>>();
    if expected != actual {
        return Err(conflict("多市场深度质量记录集合与 spec 声明不一致"));
    }
    Ok(())
}

fn quality_records_by_market(artifact: &Value, market_data: &[Value]) -> Result<Value, AppError> {
    if let Some(records) = artifact.get("dataset_quality") {
        if records.is_object() {
            return Ok(records.clone());
        }
        return Err(conflict("多市场深度数据质量记录必须是对象"));
    }
    let mut records = serde_json::Map::new();
    for row in market_data {
        let instrument = row
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or_else(|| conflict("多市场深度 market_data 缺少 instrument"))?;
        if let Some(quality) = row.get("dataset_quality").filter(|value| !value.is_null()) {
            if records
                .insert(instrument.to_owned(), quality.clone())
                .is_some()
            {
                return Err(conflict("多市场深度质量记录包含重复标的"));
            }
        }
    }
    Ok(Value::Object(records))
}

fn depth_dataset_quality_panel(
    record: Option<&Value>,
    quality_fingerprint: Option<&str>,
) -> Result<Option<Value>, AppError> {
    let Some(record) = record else {
        return Ok(None);
    };
    let quality_report = record
        .get("quality_report")
        .cloned()
        .filter(Value::is_object)
        .ok_or_else(|| conflict("多市场深度质量记录缺少有效 quality_report"))?;
    let report_is_clean = ["duplicate_rows", "out_of_order_rows", "missing_intervals"]
        .iter()
        .all(|field| quality_report.get(field).and_then(Value::as_u64) == Some(0));
    Ok(Some(serde_json::json!({
        "manifest_fingerprint": quality_fingerprint,
        "manifest_sha256": record.get("manifest_sha256").cloned().unwrap_or(Value::Null),
        "quality_report": quality_report,
        "status": if report_is_clean { "clean" } else { "issues" },
    })))
}

fn index_market_data(rows: &[Value]) -> Result<BTreeMap<&str, &Value>, AppError> {
    let mut indexed = BTreeMap::new();
    for row in rows {
        let instrument = row
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or_else(|| conflict("多市场深度 market_data 缺少 instrument"))?;
        if indexed.insert(instrument, row).is_some() {
            return Err(conflict("多市场深度 market_data 包含重复标的"));
        }
    }
    Ok(indexed)
}

fn read_required_json_sidecar(
    artifact: &Value,
    path_key: &str,
    reader: &dyn ArtifactReader,
    label: &str,
) -> Result<Value, AppError> {
    let reference = artifact
        .get(path_key)
        .and_then(Value::as_str)
        .ok_or_else(|| conflict(format!("多市场深度产物缺少 {path_key}")))?;
    let text = read_text_bounded(reader, reference, MAX_REPORT_ARTIFACT_BYTES, label)?;
    serde_json::from_str(&text).map_err(|error| conflict(format!("{label} JSON 无效: {error}")))
}

fn raw_i128(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
        .or_else(|| value.as_str().and_then(|text| text.parse::<i128>().ok()))
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn conflict(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::Conflict, message)
}

fn invariant(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}
