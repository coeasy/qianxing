//! Complete, bounded application report for shared-account multi-market Bar runs.

use crate::artifact_store::{read_text_bounded, ArtifactReader, FileArtifactReader};
use crate::report_read::{
    read_multi_market_event_report_read_model_v1_with_reader, MultiMarketEventReportPageRequestV1,
    MultiMarketEventReportReadModelV1, MultiMarketEventReportReadRequestV1, ResearchReportRowsV1,
    MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1,
};
use crate::{
    AppError, AppErrorCategory, BacktestMarginConfig, CallerCapability, MultiMarketBacktestOutcome,
    MultiMarketBacktestSpec, RunContext, VerificationResult,
};
use qx_core::{InstrumentId, OrderPolicy, TradingInstrumentSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const MULTI_MARKET_RESEARCH_REPORT_SCHEMA_VERSION_V1: u32 = 1;
pub const MAX_MULTI_MARKET_REPORT_EQUITY_ROWS: usize = 1_000;
pub const MAX_MULTI_MARKET_REPORT_FILL_ROWS: usize = 200;
pub const MAX_MULTI_MARKET_REPORT_BAR_ROWS: usize = 1_000;
pub const MAX_MULTI_MARKET_REPORT_SCHEDULE_PREVIEW_ROWS: usize = 100;
const MAX_REPORT_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;

/// Request for one verified shared-account multi-market Bar report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiMarketResearchReportReadRequestV1 {
    pub schema_version: u32,
    pub outcome: MultiMarketBacktestOutcome,
    #[serde(default)]
    pub page: Option<MultiMarketEventReportPageRequestV1>,
}

/// Market-level report facts. Numeric financial values are decimal strings to preserve precision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiMarketResearchMarketV1 {
    pub instrument: String,
    pub strategy_ids: Vec<String>,
    pub data_fingerprint: String,
    pub fill_count: u64,
    pub quality_status: String,
    pub source_end_ts: u64,
    pub bars: ResearchReportRowsV1,
    pub pnl_contribution: Option<ResearchReportRowsV1>,
}

/// A complete bounded event page; market and causal Bar context are attached by the app reader.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiMarketResearchEventPageV1 {
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

/// Shared bounded report returned to CLI, Python, HTTP and other SDK surfaces.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiMarketResearchReportV1 {
    pub schema_version: u32,
    pub run_id: String,
    pub account_id: String,
    pub instrument: String,
    pub spec_fingerprint: String,
    pub verification: VerificationResult,
    pub summary: Value,
    pub equity: ResearchReportRowsV1,
    pub fills: ResearchReportRowsV1,
    pub events: MultiMarketResearchEventPageV1,
    pub markets: Vec<MultiMarketResearchMarketV1>,
}

pub fn read_multi_market_research_report_v1(
    request: &MultiMarketResearchReportReadRequestV1,
    context: &RunContext,
) -> Result<MultiMarketResearchReportV1, AppError> {
    read_multi_market_research_report_v1_with_readers(
        request,
        context,
        &FileArtifactReader,
        &FileArtifactReader,
    )
}

/// Read, replay and verify the complete report through caller-provided artifact/data boundaries.
pub fn read_multi_market_research_report_v1_with_readers(
    request: &MultiMarketResearchReportReadRequestV1,
    context: &RunContext,
    artifact_reader: &dyn ArtifactReader,
    data_reader: &dyn ArtifactReader,
) -> Result<MultiMarketResearchReportV1, AppError> {
    context.require(CallerCapability::Research, "读取多市场完整研究报告")?;
    if request.schema_version != MULTI_MARKET_RESEARCH_REPORT_SCHEMA_VERSION_V1 {
        return Err(invalid("多市场完整报告读取请求 schema_version 不受支持"));
    }
    let event_request = MultiMarketEventReportReadRequestV1 {
        schema_version: MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1,
        outcome: request.outcome.clone(),
        page: request.page,
    };
    let model = read_multi_market_event_report_read_model_v1_with_reader(
        &event_request,
        context,
        artifact_reader,
    )?;
    build_report(model, artifact_reader, data_reader)
}

fn build_report(
    model: MultiMarketEventReportReadModelV1,
    artifact_reader: &dyn ArtifactReader,
    data_reader: &dyn ArtifactReader,
) -> Result<MultiMarketResearchReportV1, AppError> {
    let outcome = read_embedded_outcome(&model.artifact)?;
    let artifact_version = model
        .artifact
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| conflict("多市场产物缺少 schema_version"))?;
    let market_data = model
        .artifact
        .get("market_data")
        .and_then(Value::as_array)
        .ok_or_else(|| conflict("多市场产物缺少 market_data"))?;
    if market_data.len() != model.spec.markets.len() {
        return Err(conflict("多市场输入数量与 spec 不一致"));
    }
    let quality_records = model
        .artifact
        .get("dataset_quality")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    crate::validate_multi_market_quality_manifest_set_v1(&model.spec, &quality_records)?;
    let quality_objects = quality_records
        .as_object()
        .ok_or_else(|| conflict("多市场数据质量记录必须是对象"))?;
    let per_market_fills = model
        .artifact
        .get("per_market_fills")
        .and_then(Value::as_object)
        .ok_or_else(|| conflict("多市场产物缺少逐市场成交统计"))?;

    let strategy_markets = strategy_market_index(&model.spec)?;
    let mut markets = Vec::with_capacity(model.spec.markets.len());
    let mut verified_inputs = Vec::with_capacity(model.spec.markets.len());
    let mut market_specs = BTreeMap::<InstrumentId, TradingInstrumentSpec>::new();
    let mut final_marks = BTreeMap::<InstrumentId, qx_core::Price>::new();
    let mut execution_market_assumptions = Vec::with_capacity(model.spec.markets.len());
    let mut event_rows = model.page.rows.clone();
    let event_instruments = event_rows
        .iter()
        .map(|row| {
            event_market_instrument(row, &strategy_markets, &model.page.order_markets).filter(
                |instrument| {
                    model
                        .spec
                        .markets
                        .iter()
                        .any(|market| market.instrument == *instrument)
                },
            )
        })
        .collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    let mut aggregate_fills = 0_u64;

    for market_data in market_data {
        let instrument = market_data
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or_else(|| conflict("多市场 market_data 缺少 instrument"))?;
        if !seen.insert(instrument.to_owned()) {
            return Err(conflict("多市场输入存在重复标的"));
        }
        let configured = model
            .spec
            .markets
            .iter()
            .find(|market| market.instrument == instrument)
            .ok_or_else(|| conflict(format!("spec 缺少市场 {instrument}")))?;
        let frame_reference = market_data
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| conflict(format!("{instrument} 缺少 BarFrame 路径")))?;
        let frame_text = read_text_bounded(
            data_reader,
            frame_reference,
            MAX_REPORT_ARTIFACT_BYTES,
            "多市场 BarFrame",
        )?;
        let frame = qx_datastruct::BarFrame::from_json(&frame_text)
            .map_err(|error| conflict(format!("{instrument} BarFrame 无效: {error:?}")))?;
        if frame.ts.len() as u64 > crate::artifact_store::MAX_ARTIFACT_ROWS {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                format!("{instrument} BarFrame 超过行数上限"),
            ));
        }

        let quality_record = quality_objects.get(instrument);
        let quality_text = if let Some(record) = quality_record {
            let reference = record
                .get("manifest_path")
                .and_then(Value::as_str)
                .ok_or_else(|| conflict(format!("{instrument} 质量清单缺少路径")))?;
            Some(read_text_bounded(
                data_reader,
                reference,
                MAX_REPORT_ARTIFACT_BYTES,
                "多市场数据质量清单",
            )?)
        } else {
            None
        };
        let verified = crate::verify_multi_market_bar_input_v1(
            configured,
            market_data,
            &frame_text,
            &frame,
            quality_record,
            quality_text.as_deref(),
        )?;
        verified_inputs.push(crate::MultiMarketInputIdentityV1 {
            instrument: verified.instrument.clone(),
            bar_fingerprint: verified.fingerprint.clone(),
            quality_fingerprint: verified.quality_fingerprint.clone(),
        });
        let effective_spec = configured
            .market_spec
            .as_ref()
            .map(|snapshot| crate::market_spec_from_value(&frame.instrument, snapshot))
            .transpose()?
            .unwrap_or_else(|| {
                TradingInstrumentSpec::spot_default(
                    &frame.instrument,
                    &model.spec.settlement_currency,
                )
            });
        let default_order_policy = OrderPolicy {
            margin_mode: effective_spec.margin_mode_default(),
            ..OrderPolicy::default()
        };
        let effective_order_policy = configured.order_policy.unwrap_or(default_order_policy);
        let derived_margin_model = configured
            .market_spec
            .as_ref()
            .filter(|snapshot| {
                snapshot
                    .get("leverage_tiers")
                    .and_then(Value::as_array)
                    .is_some_and(|tiers| !tiers.is_empty())
            })
            .map(crate::backtest_margin_model_from_market);
        let selected_margin_model = configured
            .margin_model
            .as_ref()
            .or(derived_margin_model.as_ref());
        let effective_margin_model = if effective_spec.product.supports_leverage() {
            selected_margin_model
                .cloned()
                .unwrap_or(BacktestMarginConfig::Leverage {
                    leverage: effective_order_policy.leverage,
                    maintenance_bp: i64::from(effective_spec.maintenance_margin_bps),
                })
        } else {
            BacktestMarginConfig::NoMargin
        };
        let normalized_market_spec_json =
            serde_json::to_string(&effective_spec).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("{instrument} 标准化市场规格序列化失败: {error}"),
                )
            })?;
        let margin_model_summary = summarize_margin_model(&effective_margin_model)?;
        execution_market_assumptions.push(serde_json::json!({
            "instrument": instrument,
            "priority": configured.priority,
            "market_spec": serde_json::to_value(&effective_spec).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("{instrument} 标准化市场规格 JSON 转换失败: {error}"),
                )
            })?,
            "market_spec_json": normalized_market_spec_json,
            "fee_schedule": configured.fee_schedule,
            "order_policy": effective_order_policy,
            "margin_model": margin_model_summary,
            "funding_bps": configured.funding_bps,
            "funding_events": summarize_funding_events(&configured.funding_events)?,
        }));
        market_specs.insert(frame.instrument.clone(), effective_spec);
        final_marks.insert(
            frame.instrument.clone(),
            qx_core::Price::from_raw(verified.final_close_raw),
        );

        for (row, event_instrument) in event_rows.iter_mut().zip(&event_instruments) {
            if event_instrument.as_deref() != Some(instrument) {
                continue;
            }
            let Some((event_ts, priority)) = event_timestamp_priority(row) else {
                row["market_context"] = serde_json::json!({"instrument": instrument});
                row["bar_context"] = Value::Null;
                row["bar_context_status"] = Value::String("invalid_event_timeline".into());
                continue;
            };
            let context = crate::multi_market_bar_context(&frame, event_ts, priority);
            row["market_context"] = serde_json::json!({
                "instrument": instrument,
                "data_fingerprint": verified.fingerprint
            });
            row["bar_context_status"] = if context.is_some() {
                Value::String("verified".into())
            } else {
                Value::String("before_first_visible_bar".into())
            };
            row["bar_context"] = context.unwrap_or(Value::Null);
        }

        let fill_count = per_market_fills
            .get(instrument)
            .and_then(Value::as_u64)
            .ok_or_else(|| conflict(format!("{instrument} 逐市场成交数无效")))?;
        aggregate_fills = aggregate_fills
            .checked_add(fill_count)
            .ok_or_else(|| conflict("逐市场成交数总和溢出"))?;
        markets.push(MultiMarketResearchMarketV1 {
            instrument: instrument.to_owned(),
            strategy_ids: configured
                .strategies
                .iter()
                .map(|member| member.strategy.strategy_id.clone())
                .collect(),
            data_fingerprint: verified.fingerprint,
            fill_count,
            quality_status: verified.quality_status,
            source_end_ts: verified.source_end_ts,
            bars: bar_preview(&frame),
            pnl_contribution: None,
        });
    }
    if seen.len() != model.spec.markets.len()
        || per_market_fills.len() != model.spec.markets.len()
        || aggregate_fills != outcome.successful_fills
    {
        return Err(conflict("逐市场输入/成交统计与 outcome 摘要不一致"));
    }

    let input_identity = crate::multi_market_input_set_identity_v1(&model.spec, &verified_inputs)?;
    if input_identity.data_fingerprint != outcome.data_fingerprint {
        return Err(conflict("多市场输入数据指纹与 outcome 不一致"));
    }
    let strategy_identities: Vec<crate::StrategyArtifactIdentity> = serde_json::from_value(
        model
            .artifact
            .get("strategy_identities")
            .cloned()
            .ok_or_else(|| conflict("多市场产物缺少策略身份列表"))?,
    )
    .map_err(|error| conflict(format!("多市场策略身份列表无效: {error}")))?;
    let spec_fingerprint =
        crate::multi_market_spec_fingerprint_v1(&model.spec, &strategy_identities)?;
    if spec_fingerprint != outcome.spec_fingerprint {
        return Err(conflict("多市场规格指纹与 outcome 不一致"));
    }

    let mut input_components = input_identity.input_components;
    let contribution = read_optional_json_sidecar(
        &model.artifact,
        "instrument_equity_contributions_path",
        artifact_reader,
        "多市场逐标的权益贡献",
    )?;
    let has_contribution = contribution.is_some();
    if let Some((payload, body)) = contribution.as_ref() {
        input_components.insert(
            "derived:instrument-equity-contributions".into(),
            qx_data::sha256_hex_bytes(body.as_bytes()),
        );
        if payload.is_null() {
            return Err(conflict("逐标的权益贡献 JSON 不能为空"));
        }
    }
    let exact_equity = read_optional_json_sidecar(
        &model.artifact,
        "equity_exact_path",
        artifact_reader,
        "多市场 exact 权益",
    )?;
    if let Some((_, body)) = exact_equity.as_ref() {
        input_components.insert(
            "derived:equity-exact".into(),
            qx_data::sha256_hex_bytes(body.as_bytes()),
        );
    }
    if exact_equity.is_some() && !has_contribution {
        return Err(conflict("exact 权益 sidecar 缺少逐标的贡献 sidecar"));
    }
    if model.spec.initial_cash_exact.is_some() && exact_equity.is_none() {
        return Err(conflict(
            "MultiMarketBacktestSpec v2 缺少 exact 权益 sidecar",
        ));
    }
    if model.manifest.input_components != input_components
        || model.manifest.data_fingerprint != input_identity.data_fingerprint
        || model.manifest.result_hash != outcome.shared_result_hash
    {
        return Err(conflict("多市场 RunManifest 与输入组件/结果身份不一致"));
    }

    let equity = read_required_json_sidecar(
        &model.artifact,
        "equity_path",
        artifact_reader,
        "多市场权益 sidecar",
    )?;
    let fills = read_required_json_sidecar(
        &model.artifact,
        "fills_path",
        artifact_reader,
        "多市场成交 sidecar",
    )?;
    if model.artifact.get("equity") != Some(&equity) {
        return Err(conflict("多市场权益 sidecar 与运行产物不一致"));
    }
    let exact_equity_payload = exact_equity.as_ref().map(|(payload, _)| payload);
    let exact_equity_rows = exact_equity_payload
        .map(|payload| {
            payload
                .as_array()
                .cloned()
                .ok_or_else(|| conflict("多市场 exact 权益 sidecar 必须是数组"))
        })
        .transpose()?;
    let sidecars = if artifact_version == 2 {
        if let Some(exact_equity) = exact_equity_payload {
            crate::verify_multi_market_bar_sidecars_from_event_report_with_exact_v1(
                &equity,
                Some(exact_equity),
                &fills,
                outcome.successful_fills,
                &model.page.fill_sequence_sha256,
                &model.spec,
                outcome.equity_points,
                outcome.final_equity_raw,
                outcome.return_bps,
                outcome.max_drawdown_bps,
            )?
        } else {
            crate::verify_multi_market_bar_sidecars_from_event_report_v1(
                &equity,
                &fills,
                outcome.successful_fills,
                &model.page.fill_sequence_sha256,
                &model.spec,
                outcome.equity_points,
                outcome.final_equity_raw,
                outcome.return_bps,
                outcome.max_drawdown_bps,
            )?
        }
    } else {
        let events = model
            .artifact
            .get("event_log")
            .and_then(Value::as_array)
            .ok_or_else(|| conflict("多市场 v1 产物缺少 event_log"))?;
        crate::verify_multi_market_bar_sidecars_with_exact_v1(
            &equity,
            exact_equity_payload,
            &fills,
            events,
            &model.spec,
            outcome.equity_points,
            outcome.successful_fills,
            outcome.final_equity_raw,
            outcome.return_bps,
            outcome.max_drawdown_bps,
        )?
    };
    let equity_rows = equity
        .as_array()
        .ok_or_else(|| conflict("多市场权益曲线必须是数组"))?;
    let fill_rows = fills
        .as_array()
        .ok_or_else(|| conflict("多市场成交 sidecar 必须是数组"))?;
    let equity_report_rows = sample_equity(equity_rows, exact_equity_rows.as_deref())?;
    let fill_report_rows = sample_fills(fill_rows, sidecars.fill_rows)?;

    if let Some((contribution, _)) = contribution {
        let verified = crate::verify_multi_market_pnl_contributions_with_exact_v1(
            &contribution,
            equity_rows,
            exact_equity_rows.as_deref(),
            &model.spec,
            &model.ledger,
            &market_specs,
            &final_marks,
        )?;
        for market in &mut markets {
            let curve = verified
                .curves
                .get(&market.instrument)
                .ok_or_else(|| conflict("市场缺少已核验的权益贡献曲线"))?;
            market.pnl_contribution = Some(sample_contribution_curve(
                curve,
                verified.total_equity_points,
            )?);
        }
    }

    for (row, instrument) in event_rows.iter_mut().zip(event_instruments) {
        if instrument.is_none() {
            row["market_context"] = Value::Null;
            row["bar_context"] = Value::Null;
            row["bar_context_status"] = Value::String("input_unavailable".into());
        }
    }

    let instrument = format!(
        "portfolio[{}]",
        markets
            .iter()
            .map(|market| market.instrument.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    let summary = serde_json::json!({
        "run_id": outcome.run_id,
        "account_id": outcome.account_id,
        "markets_total": outcome.markets_total,
        "strategies_total": outcome.strategies_total,
        "execution_assumptions": {
            "settlement_currency": model.spec.settlement_currency,
            "markets": execution_market_assumptions,
        },
        "successful_fills": sidecars.fill_rows,
        "fills": sidecars.fill_rows,
        "event_count": model.page.total,
        "account": {"account_id": outcome.account_id, "initial_cash_raw": sidecars.first_equity_raw.to_string()},
        "metrics": {
            "return_bps": sidecars.return_bps,
            "max_drawdown_bps": sidecars.max_drawdown_bps,
            "final_equity_raw": sidecars.final_equity_raw.to_string()
        },
        "result_hash": outcome.shared_result_hash,
        "data_fingerprint": input_identity.data_fingerprint,
        "spec_fingerprint": spec_fingerprint,
        "markets": markets.iter().map(|market| serde_json::json!({
            "instrument": market.instrument,
            "data_fingerprint": market.data_fingerprint,
            "fills": market.fill_count,
            "quality_status": market.quality_status,
            "bar_count": market.bars.total
        })).collect::<Vec<_>>(),
    });
    let mut checks = vec![
        "outcome 与多市场回测产物身份一致".to_owned(),
        "各标的 BarFrame、质量清单与聚合数据指纹已复算".to_owned(),
        "CanonicalRunSpec、RunManifest 输入组件和结果身份已核对".to_owned(),
        "EventLog 已完整重放且账簿/成交副产物相等".to_owned(),
        "共享权益曲线、收益和最大回撤已复算".to_owned(),
    ];
    if has_contribution {
        checks.push("逐标的权益贡献已与共享权益和重放账簿期末估值核对".to_owned());
        if exact_equity_rows.is_some() {
            checks.push("逐标的子纳米 exact 贡献和共享 exact 权益已与重放账簿核对".to_owned());
        }
    } else {
        checks.push("历史产物未包含逐标的权益贡献副产物".to_owned());
    }
    let verification = VerificationResult {
        run_id: outcome.run_id.clone(),
        result_hash: outcome.shared_result_hash.clone(),
        data_fingerprint: input_identity.data_fingerprint,
        verified: true,
        checks,
        mismatches: Vec::new(),
    };
    let events = MultiMarketResearchEventPageV1 {
        schema_version: model.page.schema_version,
        run_id: model.page.run_id,
        total: model.page.total,
        offset: model.page.offset,
        truncated: model.page.truncated,
        rows: event_rows,
        fill_count: model.page.fill_count,
        fill_sequence_sha256: model.page.fill_sequence_sha256,
        replayed_ledger_entries: model.page.replayed_ledger_entries,
    };
    Ok(MultiMarketResearchReportV1 {
        schema_version: MULTI_MARKET_RESEARCH_REPORT_SCHEMA_VERSION_V1,
        run_id: outcome.run_id,
        account_id: outcome.account_id,
        instrument,
        spec_fingerprint,
        verification,
        summary,
        equity: equity_report_rows,
        fills: fill_report_rows,
        events,
        markets,
    })
}

fn read_embedded_outcome(artifact: &Value) -> Result<MultiMarketBacktestOutcome, AppError> {
    serde_json::from_value(
        artifact
            .get("outcome")
            .cloned()
            .ok_or_else(|| conflict("多市场产物缺少 outcome"))?,
    )
    .map_err(|error| conflict(format!("多市场产物 outcome 无效: {error}")))
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
        .ok_or_else(|| conflict(format!("多市场产物缺少 {path_key}")))?;
    let text = read_text_bounded(reader, reference, MAX_REPORT_ARTIFACT_BYTES, label)?;
    serde_json::from_str(&text).map_err(|error| conflict(format!("{label} JSON 无效: {error}")))
}

fn read_optional_json_sidecar(
    artifact: &Value,
    path_key: &str,
    reader: &dyn ArtifactReader,
    label: &str,
) -> Result<Option<(Value, String)>, AppError> {
    let Some(reference) = artifact.get(path_key).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let reference = reference
        .as_str()
        .ok_or_else(|| conflict(format!("{path_key} 必须是路径字符串")))?;
    let text = read_text_bounded(reader, reference, MAX_REPORT_ARTIFACT_BYTES, label)?;
    let value = serde_json::from_str(&text)
        .map_err(|error| conflict(format!("{label} JSON 无效: {error}")))?;
    Ok(Some((value, text)))
}

fn bar_preview(frame: &qx_datastruct::BarFrame) -> ResearchReportRowsV1 {
    let total = frame.ts.len() as u64;
    let stride = total
        .div_ceil(MAX_MULTI_MARKET_REPORT_BAR_ROWS.saturating_sub(1).max(1) as u64)
        .max(1) as usize;
    let indexes = (0..frame.ts.len())
        .step_by(stride)
        .chain((!frame.ts.is_empty()).then_some(frame.ts.len() - 1))
        .collect::<BTreeSet<_>>();
    let rows = indexes
        .into_iter()
        .map(|index| {
            serde_json::json!({
                "ts": frame.ts[index],
                "open_raw": frame.open_raw[index].to_string(),
                "high_raw": frame.high_raw[index].to_string(),
                "low_raw": frame.low_raw[index].to_string(),
                "close_raw": frame.close_raw[index].to_string(),
                "volume_raw": frame.volume_raw[index].to_string(),
            })
        })
        .collect();
    ResearchReportRowsV1 {
        total,
        truncated: total > MAX_MULTI_MARKET_REPORT_BAR_ROWS as u64,
        rows,
    }
}

fn summarize_margin_model(model: &BacktestMarginConfig) -> Result<Value, AppError> {
    let encoded = serde_json::to_vec(model).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("保证金模型摘要序列化失败: {error}"),
        )
    })?;
    let digest = qx_data::sha256_hex_bytes(&encoded);
    let summary = match model {
        BacktestMarginConfig::NoMargin => serde_json::json!({
            "kind": "no_margin",
            "tiers_total": 0,
            "tiers_truncated": false,
            "tiers_sha256": digest,
            "tiers": []
        }),
        BacktestMarginConfig::Leverage {
            leverage,
            maintenance_bp,
        } => serde_json::json!({
            "kind": "leverage",
            "leverage": leverage,
            "maintenance_bp": maintenance_bp,
            "tiers_total": 0,
            "tiers_truncated": false,
            "tiers_sha256": digest,
            "tiers": []
        }),
        BacktestMarginConfig::Tiers { tiers } => serde_json::json!({
            "kind": "tiers",
            "tiers_total": tiers.len(),
            "tiers_truncated": tiers.len() > MAX_MULTI_MARKET_REPORT_SCHEDULE_PREVIEW_ROWS,
            "tiers_sha256": digest,
            "tiers": tiers.iter().take(MAX_MULTI_MARKET_REPORT_SCHEDULE_PREVIEW_ROWS).map(|tier| serde_json::json!({
                "max_notional_raw": tier.max_notional.to_string(),
                "initial_bp": tier.initial_bp,
                "maintenance_bp": tier.maintenance_bp,
                "max_leverage": tier.max_leverage,
            })).collect::<Vec<_>>()
        }),
    };
    Ok(summary)
}

fn summarize_funding_events(events: &[qx_core::FundingRateEvent]) -> Result<Value, AppError> {
    let encoded = serde_json::to_vec(events).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("资金费率输入摘要序列化失败: {error}"),
        )
    })?;
    Ok(serde_json::json!({
        "total": events.len(),
        "truncated": events.len() > MAX_MULTI_MARKET_REPORT_SCHEDULE_PREVIEW_ROWS,
        "sha256": qx_data::sha256_hex_bytes(&encoded),
        "rows": events.iter().take(MAX_MULTI_MARKET_REPORT_SCHEDULE_PREVIEW_ROWS).map(|event| serde_json::json!({
            "instrument": event.instrument,
            "settlement_ts": event.settlement_ts,
            "rate_bps": event.rate_bps,
        })).collect::<Vec<_>>(),
    }))
}

fn sample_equity(
    rows: &[Value],
    exact_rows: Option<&[Value]>,
) -> Result<ResearchReportRowsV1, AppError> {
    if rows.is_empty() || rows.len() as u64 > crate::artifact_store::MAX_ARTIFACT_ROWS {
        return Err(conflict("多市场权益曲线为空或超过行数上限"));
    }
    if exact_rows.is_some_and(|exact| exact.len() != rows.len()) {
        return Err(conflict("多市场 exact 权益曲线行数与 raw 曲线不一致"));
    }
    let total = rows.len() as u64;
    let stride = total
        .div_ceil(MAX_MULTI_MARKET_REPORT_EQUITY_ROWS.saturating_sub(1).max(1) as u64)
        .max(1) as usize;
    let indexes = (0..rows.len())
        .step_by(stride)
        .chain(std::iter::once(rows.len() - 1))
        .collect::<BTreeSet<_>>();
    let rows = indexes
        .into_iter()
        .map(|index| {
            let row = &rows[index];
            let pair = row
                .as_array()
                .filter(|pair| pair.len() >= 2)
                .ok_or_else(|| conflict("多市场权益行必须是 [ts,equity_raw]"))?;
            let ts = raw_i128(&pair[0]).ok_or_else(|| conflict("权益时间戳无效"))?;
            let equity = raw_i128(&pair[1]).ok_or_else(|| conflict("权益金额无效"))?;
            let mut result = serde_json::json!({
                "ts": ts.to_string(),
                "equity_raw": equity.to_string()
            });
            if let Some(exact_rows) = exact_rows {
                let exact_value = exact_rows
                    .get(index)
                    .and_then(|row| row.get("equity_exact"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| conflict("多市场 exact 权益金额缺失"))?;
                result["equity_exact"] = Value::String(exact_value.to_owned());
            }
            Ok(result)
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(ResearchReportRowsV1 {
        total,
        truncated: total > MAX_MULTI_MARKET_REPORT_EQUITY_ROWS as u64,
        rows,
    })
}

fn sample_contribution_curve(
    curve: &[crate::VerifiedPnlContributionSample],
    expected_total: u64,
) -> Result<ResearchReportRowsV1, AppError> {
    if curve.is_empty() || curve.len() as u64 != expected_total {
        return Err(conflict("逐标的权益贡献点数与已核验组合权益点数不一致"));
    }
    let total = curve.len() as u64;
    let sample_count = curve.len().min(MAX_MULTI_MARKET_REPORT_EQUITY_ROWS);
    let indexes = if sample_count == curve.len() {
        (0..curve.len()).collect::<BTreeSet<_>>()
    } else {
        (0..sample_count)
            .map(|sample_index| {
                (u128::from(sample_index as u64) * u128::from((curve.len() - 1) as u64)
                    / u128::from((sample_count - 1) as u64)) as usize
            })
            .collect::<BTreeSet<_>>()
    };
    let rows = indexes
        .into_iter()
        .map(|index| {
            let sample = &curve[index];
            let mut row = serde_json::json!({
                "equity_index": sample.equity_index,
                "ts": sample.ts,
                "pnl_contribution_raw": sample.pnl_contribution_raw.to_string()
            });
            if let Some(exact) = sample.pnl_contribution_exact {
                row["pnl_contribution_exact"] = Value::String(exact.to_string());
            }
            row
        })
        .collect::<Vec<_>>();
    Ok(ResearchReportRowsV1 {
        total,
        truncated: total > MAX_MULTI_MARKET_REPORT_EQUITY_ROWS as u64,
        rows,
    })
}

fn sample_fills(rows: &[Value], expected_total: u64) -> Result<ResearchReportRowsV1, AppError> {
    if rows.len() as u64 != expected_total {
        return Err(conflict("成交 sidecar 行数与已复核成交总数不一致"));
    }
    let sampled = rows
        .iter()
        .rev()
        .take(MAX_MULTI_MARKET_REPORT_FILL_ROWS)
        .cloned()
        .collect::<Vec<_>>();
    Ok(ResearchReportRowsV1 {
        total: expected_total,
        truncated: expected_total > MAX_MULTI_MARKET_REPORT_FILL_ROWS as u64,
        rows: sampled,
    })
}

fn strategy_market_index(
    spec: &MultiMarketBacktestSpec,
) -> Result<BTreeMap<String, String>, AppError> {
    let mut result = BTreeMap::new();
    for market in &spec.markets {
        for strategy in &market.strategies {
            if result
                .insert(
                    strategy.strategy.strategy_id.clone(),
                    market.instrument.clone(),
                )
                .is_some()
            {
                return Err(conflict("多市场策略标识重复，不能唯一关联标的"));
            }
        }
    }
    Ok(result)
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
        let instruments = intents
            .iter()
            .filter_map(|intent| intent.get("instrument").and_then(canonical_instrument))
            .collect::<BTreeSet<_>>();
        if instruments.len() > 1 {
            return None;
        }
        if let Some(instrument) = instruments.into_iter().next() {
            return Some(instrument);
        }
    }
    let direct = kind
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
    if let Some(instrument) = direct.and_then(canonical_instrument) {
        return Some(instrument);
    }
    let order_id = kind
        .get("OrderRiskDecision")
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
        .and_then(Value::as_u64);
    if let Some(instrument) = order_id.and_then(|order_id| order_markets.get(&order_id)) {
        return Some(instrument.clone());
    }
    let strategy_id = kind
        .get("OrderRiskDecision")
        .and_then(|event| event.get("decision"))
        .and_then(|decision| decision.get("strategy_id"))
        .or_else(|| {
            kind.get("StrategyDecision")
                .and_then(|event| event.get("decision"))
                .and_then(|decision| decision.get("strategy_id"))
        })
        .or_else(|| event.pointer("/metadata/context/strategy_id"))
        .and_then(Value::as_str)?;
    strategy_markets.get(strategy_id).cloned()
}

fn canonical_instrument(value: &Value) -> Option<String> {
    serde_json::from_value::<InstrumentId>(value.clone())
        .ok()
        .map(|instrument| instrument.to_string())
        .or_else(|| value.as_str().map(str::to_owned))
}

fn event_timestamp_priority(event: &Value) -> Option<(u64, u8)> {
    let ts = event.get("ts")?.as_u64()?;
    let priority = event
        .get("prio")?
        .as_u64()
        .and_then(|value| u8::try_from(value).ok())?;
    Some((ts, priority))
}

fn raw_i128(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<i128>().ok()))
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn conflict(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::Conflict, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_schedule_previews_are_bounded_and_keep_full_input_digests() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").expect("instrument");
        let funding_events = (1..=150_u64)
            .map(|index| qx_core::FundingRateEvent {
                instrument: instrument.clone(),
                settlement_ts: index * qx_core::FUNDING_INTERVAL_MS,
                rate_bps: -25,
                funding_interval_ms: None,
            })
            .collect::<Vec<_>>();
        let funding = summarize_funding_events(&funding_events).expect("funding summary");
        assert_eq!(funding["total"], 150);
        assert_eq!(funding["truncated"], true);
        assert_eq!(funding["rows"].as_array().unwrap().len(), 100);
        assert_eq!(funding["sha256"].as_str().unwrap().len(), 64);

        let tiers = (1..=150_i128)
            .map(|index| crate::BacktestMarginTier {
                max_notional: index * 1_000,
                initial_bp: 100,
                maintenance_bp: 50,
                max_leverage: Some(20),
            })
            .collect::<Vec<_>>();
        let margin =
            summarize_margin_model(&BacktestMarginConfig::Tiers { tiers }).expect("margin summary");
        assert_eq!(margin["tiers_total"], 150);
        assert_eq!(margin["tiers_truncated"], true);
        assert_eq!(margin["tiers"].as_array().unwrap().len(), 100);
        assert_eq!(margin["tiers"][0]["max_notional_raw"], "1000");
        assert_eq!(margin["tiers_sha256"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn per_market_contribution_preview_is_bounded_and_preserves_endpoints() {
        let curve = (0..2_001_u64)
            .map(|index| crate::VerifiedPnlContributionSample {
                equity_index: index,
                ts: index * 1_000,
                pnl_contribution_raw: i128::from(index),
                pnl_contribution_exact: None,
            })
            .collect::<Vec<_>>();
        let preview = sample_contribution_curve(&curve, 2_001).expect("contribution preview");
        let rows = &preview.rows;
        assert_eq!(preview.total, 2_001);
        assert!(preview.truncated);
        assert_eq!(rows.len(), MAX_MULTI_MARKET_REPORT_EQUITY_ROWS);
        assert_eq!(rows.first().unwrap()["equity_index"], 0);
        assert_eq!(rows.last().unwrap()["equity_index"], 2_000);
        assert!(sample_contribution_curve(&curve, 2_000).is_err());
    }
}
