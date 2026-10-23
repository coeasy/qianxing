//! Read-only, verified drilldown data for the interactive research report.

use super::{respond, AppPaths};
use qx_app::{
    AppError, AppErrorCategory, BacktestOutcome, CallerCapability, MultiLegBacktestOutcome,
    MultiLegBacktestSpec, MultiMarketBacktestOutcome, MultiMarketBacktestSpec,
    MultiMarketDepthBacktestOutcome, MultiMarketDepthBacktestSpec, RunContext, VerificationResult,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const MAX_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ARTIFACT_ROWS: u64 = 5_000_000;
const MAX_EQUITY_POINTS: u64 = 1_000;
const MAX_FILL_ROWS: usize = 200;
const MAX_EVENT_ROWS: usize = 300;
const MAX_BAR_POINTS: u64 = 1_000;
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 1024 * 1024;
const MAX_MULTI_MARKET_DIAGNOSTICS: usize = 512;
const MAX_MULTI_ACCOUNT_DIAGNOSTICS: usize = 32;
const MAX_MULTI_MARKETS: usize = 32;
const MAX_MULTI_ACCOUNTS: usize = 32;

#[derive(Serialize)]
struct CsvView {
    total: u64,
    truncated: bool,
    rows: Vec<Value>,
}

#[derive(Serialize)]
struct EventView {
    total: u64,
    offset: u64,
    truncated: bool,
    rows: Vec<Value>,
}

#[derive(Clone, Copy)]
struct EventPage {
    offset: u64,
    limit: usize,
}

#[derive(Serialize)]
struct ResearchArtifactView {
    schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_kind: Option<&'static str>,
    run_id: String,
    instrument: String,
    verification: VerificationResult,
    summary: Value,
    equity: CsvView,
    fills: CsvView,
    events: EventView,
    bars: BarView,
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<DepthView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event_trace_available: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event_instrument: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    strategy_diagnostics: Option<BoundStrategyDiagnostics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    market_strategy_diagnostics: Option<Vec<BoundStrategyDiagnostics>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    markets: Option<Vec<MarketResearchView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    accounts: Option<Vec<AccountResearchView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_event_offsets: Option<BTreeMap<String, u64>>,
}

#[derive(Serialize)]
struct MarketResearchView {
    instrument: String,
    data_fingerprint: String,
    fill_count: u64,
    quality_status: &'static str,
    bars: BarView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pnl_contribution: Option<PnlContributionView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<DepthView>,
    #[serde(skip)]
    source_end_ts: Option<u64>,
}

#[derive(Serialize)]
struct PnlContributionView {
    currency: String,
    total: u64,
    truncated: bool,
    rows: Vec<Value>,
}

#[derive(Serialize)]
struct AccountResearchView {
    account_id: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instrument: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    return_bps: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_drawdown_bps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fills: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    equity_points: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<Value>,
}

#[derive(Serialize)]
struct BarView {
    source_status: &'static str,
    total: u64,
    truncated: bool,
    rows: Vec<qx_app::artifact_store::VerifiedBarFramePoint>,
}

#[derive(Serialize)]
struct DepthView {
    source_status: &'static str,
    total: u64,
    truncated: bool,
    rows: Vec<qx_app::artifact_store::VerifiedDepthFramePoint>,
}

#[derive(Serialize)]
struct BoundStrategyDiagnostics {
    identity_bound: bool,
    computation: &'static str,
    report: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FillKey {
    ts: u64,
    order_id: u64,
    qty_raw: i128,
    price_raw: i128,
    fee_raw: i128,
    strategy_id: String,
}

pub(super) fn route(body: &str, paths: &AppPaths) -> crate::ApiResponse {
    respond(build(body, paths).and_then(|document| {
        serde_json::to_string(&document).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("研究报告明细序列化失败: {error}"),
            )
        })
    }))
}

fn build(body: &str, paths: &AppPaths) -> Result<ResearchArtifactView, AppError> {
    let request: Value = serde_json::from_str(body).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("研究报告请求 JSON 无效: {error}"),
        )
    })?;
    let (
        outcome_value,
        strategy_diagnostics,
        market_strategy_diagnostics,
        event_page,
        account_event_offsets,
        event_instrument,
    ) = parse_request(&request)?;
    let account_strategy_diagnostics = request
        .get("account_strategy_diagnostics")
        .filter(|value| !value.is_null());
    let is_multi_account = outcome_value.get("batch_id").is_some()
        && outcome_value
            .get("runs")
            .and_then(Value::as_array)
            .is_some();
    if account_event_offsets.is_some() && !is_multi_account {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "account_event_offsets 仅适用于多账户研究报告",
        ));
    }
    if event_instrument.is_some() && !is_multi_account {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "event_instrument 仅适用于多账户研究报告",
        ));
    }
    if account_strategy_diagnostics.is_some() && !is_multi_account {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "account_strategy_diagnostics 仅适用于多账户研究报告",
        ));
    }
    if outcome_value.get("markets_total").is_some() && outcome_value.get("pending_orders").is_some()
    {
        if strategy_diagnostics.is_some() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "多市场深度报告须通过 market_strategy_diagnostics 按标的绑定策略诊断",
            ));
        }
        return build_multi_market_depth(
            outcome_value,
            paths,
            event_page,
            market_strategy_diagnostics,
        );
    }
    if outcome_value.get("tier").is_some() {
        if strategy_diagnostics.is_some() || market_strategy_diagnostics.is_some() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "深度研究报告不支持 Bar 策略诊断附件",
            ));
        }
        return build_depth(outcome_value, paths, event_page);
    }
    if outcome_value.get("batch_id").is_some() && outcome_value.get("runs").is_some() {
        if strategy_diagnostics.is_some() || market_strategy_diagnostics.is_some() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "多账户报告须通过 account_strategy_diagnostics 按账户身份绑定策略诊断",
            ));
        }
        return build_multi_account(
            outcome_value,
            paths,
            event_page,
            account_event_offsets.as_ref(),
            event_instrument.as_deref(),
            account_strategy_diagnostics,
        );
    }
    if outcome_value.get("attribution_artifact").is_some() {
        return build_multi_leg(
            outcome_value,
            paths,
            event_page,
            strategy_diagnostics,
            market_strategy_diagnostics,
        );
    }
    if outcome_value.get("markets_total").is_some() {
        if strategy_diagnostics.is_some() && market_strategy_diagnostics.is_some() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "strategy_diagnostics 与 market_strategy_diagnostics 不能同时提供",
            ));
        }
        return build_multi_market(
            outcome_value,
            paths,
            event_page,
            strategy_diagnostics,
            market_strategy_diagnostics,
        );
    }
    if market_strategy_diagnostics.is_some() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "market_strategy_diagnostics 仅适用于多市场 Bar 研究报告",
        ));
    }
    let encoded_outcome = serde_json::to_string(outcome_value).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("回测结果请求编码失败: {error}"),
        )
    })?;
    let mut outcome = BacktestOutcome::from_json(&encoded_outcome)?;
    let event_trace = outcome.artifacts.event_trace.clone().ok_or_else(|| {
        AppError::new(
            AppErrorCategory::DataUnavailable,
            "该回测没有可交互读取的事件轨迹",
        )
    })?;
    let root = std::fs::canonicalize(&paths.artifact_root)
        .map_err(|error| AppError::from_io("解析 API 研究产物根目录", &error))?;
    outcome.artifacts.run_manifest = rooted_file(&outcome.artifacts.run_manifest, &root)?;
    outcome.artifacts.summary = rooted_file(&outcome.artifacts.summary, &root)?;
    outcome.artifacts.equity = rooted_file(&outcome.artifacts.equity, &root)?;
    outcome.artifacts.fills = rooted_file(&outcome.artifacts.fills, &root)?;
    outcome.artifacts.event_trace = Some(rooted_file(&event_trace, &root)?);
    let event_trace = outcome.artifacts.event_trace.as_deref().ok_or_else(|| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            "规范化事件轨迹路径后字段丢失",
        )
    })?;

    for path in [
        &outcome.artifacts.run_manifest,
        &outcome.artifacts.summary,
        &outcome.artifacts.equity,
        &outcome.artifacts.fills,
    ] {
        check_artifact_size(Path::new(path))?;
    }
    check_artifact_size(Path::new(event_trace))?;

    let mut verification = qx_app::verify_run(
        &outcome,
        &RunContext::new(CallerCapability::Research, outcome.run_id.clone()),
    )?;
    let summary = read_json(Path::new(&outcome.artifacts.summary))?;
    if !verification.verified {
        return Ok(ResearchArtifactView {
            schema_version: 1,
            run_kind: None,
            run_id: outcome.run_id,
            instrument: outcome.instrument,
            verification,
            summary,
            equity: empty_view(),
            fills: empty_view(),
            events: empty_event_view(),
            bars: empty_bars("run_not_verified"),
            depth: None,
            event_trace_available: None,
            event_instrument: None,
            strategy_diagnostics: None,
            market_strategy_diagnostics: None,
            markets: None,
            accounts: None,
            account_event_offsets: None,
        });
    }
    verify_declared_run_artifacts(&summary, &root)?;

    let strategy_diagnostics = strategy_diagnostics
        .map(|report| bind_strategy_diagnostics(report, &summary))
        .transpose()?;
    let equity = equity_view(Path::new(&outcome.artifacts.equity))?;
    let manifest = read_json(Path::new(&outcome.artifacts.run_manifest))?;
    let (mut events, event_fills) =
        event_view(Path::new(event_trace), &summary, &manifest, event_page)?;
    let fills = validated_fill_view(Path::new(&outcome.artifacts.fills), event_fills)?;
    verification
        .checks
        .push("Bar EventLog 已完整重放，摘要/RunManifest digest 与成交明细一致".into());
    let bars = raw_bars_view(&summary, &paths.data_root, &root)?;
    attach_bar_event_context(&mut events, &summary, &paths.data_root)?;
    Ok(ResearchArtifactView {
        schema_version: 1,
        run_kind: None,
        run_id: outcome.run_id,
        instrument: outcome.instrument,
        verification,
        summary,
        equity,
        fills,
        events,
        bars,
        depth: None,
        event_trace_available: None,
        event_instrument: None,
        strategy_diagnostics,
        market_strategy_diagnostics: None,
        markets: None,
        accounts: None,
        account_event_offsets: None,
    })
}

/// Verify a persisted two-leg run and project it into the interactive report surface. The
/// attribution artifact is treated as untrusted: source BarFrames, their optional quality
/// manifests, the shared event log, replay ledger, per-leg fills and equity metrics are checked
/// again before any report rows are returned.
fn build_multi_leg(
    outcome_value: &Value,
    paths: &AppPaths,
    requested_page: Option<EventPage>,
    strategy_diagnostics: Option<&Value>,
    market_strategy_diagnostics: Option<&Value>,
) -> Result<ResearchArtifactView, AppError> {
    let requested: MultiLegBacktestOutcome = serde_json::from_value(outcome_value.clone())
        .map_err(|error| invalid_report(format!("多腿回测结果结构无效: {error}")))?;
    let artifact_root = std::fs::canonicalize(&paths.artifact_root)
        .map_err(|error| AppError::from_io("解析 API 研究产物根目录", &error))?;
    let data_root = std::fs::canonicalize(&paths.data_root)
        .map_err(|error| AppError::from_io("解析 API 数据根目录", &error))?;
    let artifact_path = rooted_file(&requested.attribution_artifact, &artifact_root)?;
    check_artifact_size(Path::new(&artifact_path))?;
    let artifact = read_json(Path::new(&artifact_path))?;
    if artifact.get("schema_version").and_then(Value::as_u64) != Some(3) {
        return Err(invalid_report("多腿回测产物 schema_version 不受支持"));
    }
    let spec: MultiLegBacktestSpec = serde_json::from_value(
        artifact
            .get("spec")
            .cloned()
            .ok_or_else(|| invalid_report("多腿回测产物缺少原始 spec"))?,
    )
    .map_err(|error| invalid_report(format!("多腿回测产物 spec 无效: {error}")))?;
    spec.validate()?;
    if spec.run_id != requested.run_id
        || artifact.get("run_id").and_then(Value::as_str) != Some(requested.run_id.as_str())
        || artifact.get("primary_instrument").and_then(Value::as_str)
            != Some(requested.primary_instrument.as_str())
        || artifact.get("reference_instrument").and_then(Value::as_str)
            != Some(requested.reference_instrument.as_str())
    {
        return Err(invalid_report("多腿结果与持久产物身份不一致"));
    }

    let market_data = artifact
        .get("market_data")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_report("多腿产物缺少 market_data"))?;
    if market_data.len() != 2 {
        return Err(invalid_report("多腿 market_data 必须恰含两条腿"));
    }
    let mut frames = BTreeMap::<String, qx_datastruct::BarFrame>::new();
    let mut fingerprints = BTreeMap::<String, String>::new();
    let mut quality_fingerprints = BTreeMap::<String, String>::new();
    let mut markets = Vec::with_capacity(2);
    let quality_objects = artifact
        .get("dataset_quality")
        .filter(|value| !value.is_null());
    for market in market_data {
        let instrument = market
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_report("多腿 market_data 缺少 instrument"))?;
        if frames.contains_key(instrument)
            || ![
                requested.primary_instrument.as_str(),
                requested.reference_instrument.as_str(),
            ]
            .contains(&instrument)
        {
            return Err(invalid_report("多腿输入标的重复或与结果身份不一致"));
        }
        let source = market
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_report(format!("{instrument} 缺少 BarFrame 路径")))?;
        let frame_path = rooted_file(source, &data_root)?;
        check_artifact_size(Path::new(&frame_path))?;
        let bytes = std::fs::read(&frame_path)
            .map_err(|error| AppError::from_io("读取多腿 BarFrame", &error))?;
        let frame =
            qx_datastruct::BarFrame::from_json(std::str::from_utf8(&bytes).map_err(|error| {
                invalid_report(format!("{instrument} BarFrame 不是 UTF-8: {error}"))
            })?)
            .map_err(|error| invalid_report(format!("{instrument} BarFrame 无效: {error:?}")))?;
        if frame.ts.len() as u64 > MAX_ARTIFACT_ROWS {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                format!("{instrument} BarFrame 超过 {MAX_ARTIFACT_ROWS} 行上限"),
            ));
        }
        let fingerprint = qx_data::fingerprint_bar_frame(&frame)
            .map_err(|error| invalid_report(format!("{instrument} BarFrame 指纹失败: {error}")))?;
        if frame.instrument.to_string() != instrument
            || market.get("content_hash").and_then(Value::as_str) != Some(fingerprint.as_str())
            || market.get("row_count").and_then(Value::as_u64) != Some(frame.ts.len() as u64)
        {
            return Err(invalid_report(format!(
                "{instrument} BarFrame 与运行输入身份不一致"
            )));
        }
        let expected_manifest = if instrument == requested.primary_instrument {
            spec.primary_dataset_manifest_path.as_deref()
        } else {
            spec.reference_dataset_manifest_path.as_deref()
        };
        let actual_manifest = market.get("dataset_manifest_path").and_then(Value::as_str);
        if actual_manifest != expected_manifest {
            return Err(invalid_report(format!(
                "{instrument} 数据质量清单路径与 spec 不一致"
            )));
        }
        let quality_status = if let Some(expected_manifest) = expected_manifest {
            let record = quality_objects
                .and_then(|value| {
                    value.get(if instrument == requested.primary_instrument {
                        "primary"
                    } else {
                        "reference"
                    })
                })
                .ok_or_else(|| invalid_report(format!("{instrument} 缺少数据质量核验结果")))?;
            let quality_path = rooted_file(expected_manifest, &data_root)?;
            let quality_bytes = std::fs::read(&quality_path)
                .map_err(|error| AppError::from_io("读取多腿数据质量清单", &error))?;
            let manifest = qx_data::DatasetManifestV2::from_json(
                std::str::from_utf8(&quality_bytes).map_err(|error| {
                    invalid_report(format!("{instrument} 数据质量清单不是 UTF-8: {error}"))
                })?,
            )
            .map_err(|error| invalid_report(format!("{instrument} 数据质量清单无效: {error}")))?;
            let interval = manifest
                .quality_report
                .interval_ms
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    invalid_report(format!("{instrument} 数据质量清单缺少正 interval_ms"))
                })?;
            let missing = frame
                .ts
                .windows(2)
                .try_fold(0_u64, |total, pair| {
                    total.checked_add((pair[1] - pair[0]).div_ceil(interval).saturating_sub(1))
                })
                .ok_or_else(|| invalid_report("多腿 BarFrame 缺口计数溢出"))?;
            let manifest_fingerprint = manifest.fingerprint().map_err(|error| {
                invalid_report(format!("{instrument} 数据质量清单指纹失败: {error}"))
            })?;
            let manifest_sha256 = qx_data::sha256_hex_bytes(&quality_bytes);
            let frame_sha256 = qx_data::sha256_hex_bytes(&bytes);
            if manifest.instrument != instrument
                || manifest.tier != qx_data::DatasetTier::Bar
                || manifest.provider_version != qx_data::BARFRAME_DATASET_VERSION
                || manifest.dataset.fingerprint != fingerprint
                || manifest.content_hash != frame_sha256
                || manifest.quality_report.row_count != frame.ts.len() as u64
                || manifest.dataset.start_timestamp != frame.ts.first().copied().unwrap_or_default()
                || manifest.dataset.end_timestamp != frame.ts.last().copied().unwrap_or_default()
                || manifest.quality_report.missing_intervals != missing
                || manifest.quality_report.duplicate_rows != 0
                || manifest.quality_report.out_of_order_rows != 0
                || record.get("manifest_path").and_then(Value::as_str) != Some(expected_manifest)
                || record.get("manifest_sha256").and_then(Value::as_str)
                    != Some(manifest_sha256.as_str())
                || record.get("manifest_fingerprint").and_then(Value::as_str)
                    != Some(manifest_fingerprint.as_str())
                || record.get("quality_report")
                    != Some(
                        &serde_json::to_value(&manifest.quality_report)
                            .map_err(|error| invalid_report(error.to_string()))?,
                    )
            {
                return Err(invalid_report(format!(
                    "{instrument} 数据质量清单与 BarFrame 不一致"
                )));
            }
            quality_fingerprints.insert(instrument.to_owned(), manifest_fingerprint);
            "verified"
        } else {
            if quality_objects.is_some() {
                return Err(invalid_report(format!(
                    "{instrument} 未声明数据质量清单但产物含核验结果"
                )));
            }
            "not_provided"
        };
        fingerprints.insert(instrument.to_owned(), fingerprint.clone());
        frames.insert(instrument.to_owned(), frame.clone());
        markets.push(MarketResearchView {
            instrument: instrument.to_owned(),
            data_fingerprint: fingerprint,
            fill_count: 0,
            quality_status,
            bars: barframe_view(&frame),
            pnl_contribution: None,
            data_tier: None,
            depth: None,
            source_end_ts: frame.ts.last().copied(),
        });
    }
    if frames.len() != 2
        || !frames.contains_key(&requested.primary_instrument)
        || !frames.contains_key(&requested.reference_instrument)
    {
        return Err(invalid_report("多腿回测缺少主腿或参考腿输入"));
    }
    let data_fingerprint = if quality_fingerprints.is_empty() {
        format!(
            "multi-bar:{}:{}",
            fingerprints[&requested.primary_instrument],
            fingerprints[&requested.reference_instrument]
        )
    } else {
        format!(
            "multi-bar:{}:{}:quality:{}:{}",
            fingerprints[&requested.primary_instrument],
            fingerprints[&requested.reference_instrument],
            quality_fingerprints[&requested.primary_instrument],
            quality_fingerprints[&requested.reference_instrument]
        )
    };
    if artifact.get("data_fingerprint").and_then(Value::as_str) != Some(data_fingerprint.as_str()) {
        return Err(invalid_report("多腿数据指纹与已复核输入不一致"));
    }

    let events = artifact
        .get("shared_event_log")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_report("多腿产物缺少共享 EventLog"))?;
    if events.is_empty() || events.len() as u64 > MAX_ARTIFACT_ROWS {
        return Err(invalid_report("多腿共享 EventLog 为空或超过行数上限"));
    }
    let typed_events: Vec<qx_core::Event> = serde_json::from_value(Value::Array(events.clone()))
        .map_err(|error| invalid_report(format!("多腿 EventLog 无效: {error}")))?;
    let (event_log, ledger) = qx_core::ReplayVerifier::replay_owned(typed_events)
        .map_err(|error| invalid_report(format!("多腿 EventLog 重放失败: {error:?}")))?;
    let shared_result_hash = format!("{:016x}", event_log.digest());
    if artifact.get("shared_result_hash").and_then(Value::as_str)
        != Some(shared_result_hash.as_str())
        || artifact.get("replay_events").and_then(Value::as_u64) != Some(events.len() as u64)
        || artifact
            .get("replay_ledger_entries")
            .and_then(Value::as_u64)
            != Some(ledger.entries().len() as u64)
        || serde_json::to_value(ledger.entries())
            .map_err(|error| invalid_report(error.to_string()))?
            != artifact
                .get("ledger_entries")
                .cloned()
                .unwrap_or(Value::Null)
    {
        return Err(invalid_report(
            "多腿 EventLog digest 或重放账簿与产物不一致",
        ));
    }
    let account_id = spec
        .account_id
        .clone()
        .unwrap_or_else(|| format!("multi-leg-{}", spec.run_id));
    let initial_cash_raw = spec
        .primary_initial_cash_raw
        .checked_add(spec.reference_initial_cash_raw)
        .ok_or_else(|| invalid_report("多腿初始资金溢出"))?;
    let expected_positions = [
        requested.primary_instrument.as_str(),
        requested.reference_instrument.as_str(),
    ]
    .into_iter()
    .map(|instrument| {
        let instrument_id = qx_core::InstrumentId::parse(instrument)
            .ok_or_else(|| invalid_report(format!("多腿标的 ID 无效: {instrument}")))?;
        let position = ledger.position_for(&account_id, &instrument_id);
        Ok(serde_json::json!({
            "instrument": instrument,
            "quantity_raw": position.quantity.raw().to_string(),
            "average_entry_raw": position.average_entry.raw().to_string(),
            "realized_pnl_raw": position.realized_pnl.raw().to_string()
        }))
    })
    .collect::<Result<Vec<_>, AppError>>()?;
    let realized_pnl = ledger
        .realized_pnl_for(&account_id)
        .ok_or_else(|| invalid_report("多腿重放账户已实现盈亏溢出"))?;
    let expected_ledger = serde_json::json!({
        "account_id": account_id,
        "allocation_policy": "shared_buying_power_deterministic_market_priority",
        "cross_leg_buying_power": true,
        "initial_cash_raw": initial_cash_raw.to_string(),
        "initial_cash": format_raw(initial_cash_raw),
        "cash_balances_raw": ledger.cash_balances_for(&account_id).iter().map(|(currency, value)| (currency.clone(), value.to_string())).collect::<BTreeMap<_, _>>(),
        "positions": expected_positions,
        "realized_pnl_raw": realized_pnl.to_string(),
        "ledger_entry_count": ledger.entries().len(),
        "replay_verified": true
    });
    if artifact.get("account_ledger") != Some(&expected_ledger) {
        return Err(invalid_report("多腿共享账户账簿摘要与 EventLog 重放不一致"));
    }

    let order_markets = events
        .iter()
        .filter_map(|event| {
            let instrument = event_market_instrument(event, &BTreeMap::new(), &BTreeMap::new())?;
            Some((event_order_id(event)?, instrument))
        })
        .collect::<BTreeMap<_, _>>();
    let mut fills_by_market = BTreeMap::<String, Vec<qx_core::Fill>>::new();
    let mut event_market_contexts = BTreeMap::<u64, Vec<Value>>::new();
    for row in events {
        let Some(kind) = row.get("kind").and_then(Value::as_object) else {
            continue;
        };
        if let Some(fill_value) = kind.get("Filled").and_then(|value| value.get("fill")) {
            let order_id = fill_value
                .get("order_id")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid_report("多腿成交缺少有效 order_id"))?;
            let instrument = order_markets
                .get(&order_id)
                .ok_or_else(|| invalid_report("多腿成交无法关联到具体标的"))?;
            let fill: qx_core::Fill = serde_json::from_value(fill_value.clone())
                .map_err(|error| invalid_report(format!("多腿成交无效: {error}")))?;
            fills_by_market
                .entry(instrument.clone())
                .or_default()
                .push(fill);
        }
        let instruments = multi_leg_event_instruments(row, &order_markets, &frames);
        let Some(seq) = row.get("seq").and_then(Value::as_u64) else {
            continue;
        };
        let Some(ts) = row.get("ts").and_then(Value::as_u64) else {
            continue;
        };
        let Some(priority) = row
            .get("prio")
            .and_then(Value::as_u64)
            .and_then(|value| u8::try_from(value).ok())
        else {
            continue;
        };
        let mut contexts = Vec::new();
        for instrument in instruments {
            if let Some(frame) = frames.get(&instrument) {
                let (bar_context, status) = match multi_market_bar_context(frame, ts, priority) {
                    Some(context) => (context, "verified"),
                    None => (Value::Null, "before_first_visible_bar"),
                };
                contexts.push(serde_json::json!({
                    "instrument": instrument,
                    "data_fingerprint": fingerprints.get(&instrument),
                    "bar_context": bar_context,
                    "bar_context_status": status
                }));
            }
        }
        if !contexts.is_empty() {
            event_market_contexts.insert(seq, contexts);
        }
    }
    let mut per_market_fills = BTreeMap::<String, u64>::new();
    for instrument in [
        &requested.primary_instrument,
        &requested.reference_instrument,
    ] {
        let leg_fills = fills_by_market.get(instrument).cloned().unwrap_or_default();
        let mut hasher = qx_core::Fnv1a::new();
        hasher.write_text(instrument);
        hasher.write_text(
            &serde_json::to_string(&leg_fills)
                .map_err(|error| invalid_report(error.to_string()))?,
        );
        let result_hash = format!("{:016x}", hasher.finish());
        let (count, outcome_hash) = if instrument == &requested.primary_instrument {
            (
                requested.primary_fills,
                requested.primary_result_hash.as_str(),
            )
        } else {
            (
                requested.reference_fills,
                requested.reference_result_hash.as_str(),
            )
        };
        if leg_fills.len() as u64 != count
            || artifact
                .get(if instrument == &requested.primary_instrument {
                    "primary_fills"
                } else {
                    "reference_fills"
                })
                .and_then(Value::as_u64)
                != Some(count)
            || artifact
                .get(if instrument == &requested.primary_instrument {
                    "primary_result_hash"
                } else {
                    "reference_result_hash"
                })
                .and_then(Value::as_str)
                != Some(outcome_hash)
            || result_hash != outcome_hash
        {
            return Err(invalid_report(format!(
                "{instrument} 成交数或结果指纹与 EventLog 不一致"
            )));
        }
        per_market_fills.insert(instrument.clone(), count);
    }
    for market in &mut markets {
        market.fill_count = *per_market_fills.get(&market.instrument).unwrap_or(&0);
    }

    let effective_specs = artifact
        .get("effective_market_specs")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_report("多腿产物缺少有效市场规格"))?;
    let primary_spec: qx_core::TradingInstrumentSpec = serde_json::from_value(
        effective_specs
            .get("primary")
            .cloned()
            .ok_or_else(|| invalid_report("多腿产物缺少主腿有效市场规格"))?,
    )
    .map_err(|error| invalid_report(format!("主腿有效市场规格无效: {error}")))?;
    let reference_spec: qx_core::TradingInstrumentSpec = serde_json::from_value(
        effective_specs
            .get("reference")
            .cloned()
            .ok_or_else(|| invalid_report("多腿产物缺少参考腿有效市场规格"))?,
    )
    .map_err(|error| invalid_report(format!("参考腿有效市场规格无效: {error}")))?;
    let primary_fills = fills_by_market
        .get(&requested.primary_instrument)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let reference_fills = fills_by_market
        .get(&requested.reference_instrument)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let primary_funding =
        multi_leg_funding_cost(&ledger, &account_id, &requested.primary_instrument)?;
    let reference_funding =
        multi_leg_funding_cost(&ledger, &account_id, &requested.reference_instrument)?;
    let primary_return = multi_leg_leg_return_bps(
        &ledger,
        &account_id,
        &requested.primary_instrument,
        &primary_spec,
        &frames[&requested.primary_instrument],
        primary_fills,
        primary_funding,
        spec.primary_initial_cash_raw,
    )?;
    let reference_return = multi_leg_leg_return_bps(
        &ledger,
        &account_id,
        &requested.reference_instrument,
        &reference_spec,
        &frames[&requested.reference_instrument],
        reference_fills,
        reference_funding,
        spec.reference_initial_cash_raw,
    )?;
    if artifact.get("primary_return_bps").and_then(Value::as_i64) != Some(i64::from(primary_return))
        || artifact.get("reference_return_bps").and_then(Value::as_i64)
            != Some(i64::from(reference_return))
    {
        return Err(invalid_report(
            "多腿逐腿收益与重放账簿、成交及期末标记不一致",
        ));
    }
    let total_fees = primary_fills
        .iter()
        .chain(reference_fills)
        .try_fold(0_i128, |sum, fill| sum.checked_add(fill.fee.raw()))
        .ok_or_else(|| invalid_report("多腿成交费用总额溢出"))?;
    let total_funding = primary_funding
        .checked_add(reference_funding)
        .ok_or_else(|| invalid_report("多腿资金费总额溢出"))?;
    let total_turnover = primary_fills
        .iter()
        .map(|fill| primary_spec.notional(fill.qty.raw(), fill.price.raw()))
        .chain(
            reference_fills
                .iter()
                .map(|fill| reference_spec.notional(fill.qty.raw(), fill.price.raw())),
        )
        .try_fold(0_i128, |sum, amount| {
            amount.ok().and_then(|amount| sum.checked_add(amount))
        })
        .ok_or_else(|| invalid_report("多腿成交额重算失败或溢出"))?;
    let attribution = artifact
        .get("attribution_totals")
        .ok_or_else(|| invalid_report("多腿产物缺少归因总计"))?;
    let recorded_group_fees = attribution
        .get("fees_raw")
        .and_then(raw_i128)
        .ok_or_else(|| invalid_report("多腿组级费用总计无效"))?;
    let recorded_residual_fees = attribution
        .get("residual_fees_raw")
        .and_then(raw_i128)
        .ok_or_else(|| invalid_report("多腿残余费用总计无效"))?;
    let recorded_group_turnover = attribution
        .get("turnover_raw")
        .and_then(raw_i128)
        .ok_or_else(|| invalid_report("多腿组级成交额总计无效"))?;
    let pending_turnover = artifact
        .get("pending_reconcile")
        .and_then(|value| value.get("items"))
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_report("多腿产物缺少 pending_reconcile"))?
        .iter()
        .try_fold(0_i128, |sum, pending| {
            pending
                .get("turnover_raw")
                .and_then(raw_i128)
                .and_then(|value| sum.checked_add(value))
        })
        .ok_or_else(|| invalid_report("多腿未配对成交额无效或溢出"))?;
    if recorded_group_fees.checked_add(recorded_residual_fees) != Some(total_fees)
        || attribution.get("funding_raw").and_then(raw_i128) != Some(total_funding)
        || recorded_group_turnover.checked_add(pending_turnover) != Some(total_turnover)
    {
        return Err(invalid_report(
            "多腿费用、资金费或成交额归因与重放事实不一致",
        ));
    }
    let net_cost = total_fees
        .checked_add(total_funding)
        .ok_or_else(|| invalid_report("多腿净成本溢出"))?;
    let cost_bps = if total_turnover > 0 {
        Some(
            net_cost
                .checked_mul(10_000)
                .and_then(|value| value.checked_div(total_turnover))
                .and_then(|value| i64::try_from(value).ok())
                .ok_or_else(|| invalid_report("多腿单位净成本无法复算"))?,
        )
    } else {
        None
    };

    let equity = artifact
        .get("equity_curve")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_report("多腿产物缺少共享权益曲线"))?;
    if equity.is_empty() || equity.len() as u64 > MAX_ARTIFACT_ROWS {
        return Err(invalid_report("多腿权益曲线为空或超过行数上限"));
    }
    let mut previous_equity_ts = None;
    for row in equity {
        let timestamp = row
            .get(0)
            .and_then(raw_i128)
            .filter(|timestamp| *timestamp >= 0)
            .ok_or_else(|| invalid_report("多腿权益曲线包含无效时间"))?;
        row.get(1)
            .and_then(raw_i128)
            .ok_or_else(|| invalid_report("多腿权益曲线包含无效金额"))?;
        if previous_equity_ts.is_some_and(|previous| timestamp < previous) {
            return Err(invalid_report("多腿权益曲线时间顺序倒退"));
        }
        previous_equity_ts = Some(timestamp);
    }
    let first_equity = equity
        .first()
        .and_then(|row| row.get(1))
        .and_then(raw_i128)
        .ok_or_else(|| invalid_report("多腿权益首点无效"))?;
    let final_equity = equity
        .last()
        .and_then(|row| row.get(1))
        .and_then(raw_i128)
        .ok_or_else(|| invalid_report("多腿权益末点无效"))?;
    if first_equity != initial_cash_raw {
        return Err(invalid_report("多腿权益首点与声明初始资金不一致"));
    }
    let computed_return = final_equity
        .checked_sub(first_equity)
        .and_then(|value| value.checked_mul(10_000))
        .and_then(|value| value.checked_div(first_equity))
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid_report("多腿组合收益基点无法复算"))?;
    if artifact.get("combined_return_bps").and_then(Value::as_i64)
        != Some(i64::from(computed_return))
        || requested.combined_return_bps != computed_return
    {
        return Err(invalid_report("多腿组合收益与权益曲线不一致"));
    }
    let computed_drawdown = equity_drawdown_bps(equity)?;

    let page = requested_page.unwrap_or(EventPage {
        offset: 0,
        limit: MAX_EVENT_ROWS,
    });
    if page.offset > events.len() as u64 {
        return Err(invalid_report("事件分页 offset 超出多腿事件流范围"));
    }
    let event_end = (page.offset as usize)
        .saturating_add(page.limit)
        .min(events.len());
    let mut event_rows = events[page.offset as usize..event_end].to_vec();
    for row in &mut event_rows {
        attach_research_fact_projection(row);
        let seq = row.get("seq").and_then(Value::as_u64);
        let contexts = seq.and_then(|seq| event_market_contexts.get(&seq));
        match contexts {
            Some(contexts) if contexts.len() == 1 => {
                row["instrument"] = contexts[0]["instrument"].clone();
                row["market_context"] = serde_json::json!({
                    "instrument": contexts[0]["instrument"],
                    "data_fingerprint": contexts[0]["data_fingerprint"]
                });
                row["bar_context"] = contexts[0]["bar_context"].clone();
                row["bar_context_status"] = contexts[0]["bar_context_status"].clone();
            }
            Some(contexts) => {
                row["market_contexts"] = Value::Array(contexts.clone());
                row["market_context"] = Value::Null;
                row["bar_context"] = Value::Null;
                row["bar_context_status"] = Value::String("multiple_legs".into());
            }
            None => {
                row["market_context"] = Value::Null;
                row["bar_context"] = Value::Null;
                row["bar_context_status"] = Value::String("input_unavailable".into());
            }
        }
    }
    let fills_total = requested
        .primary_fills
        .saturating_add(requested.reference_fills);
    let mut fill_rows = events
        .iter()
        .filter_map(|event| {
            let mut fill = event.pointer("/kind/Filled/fill")?.clone();
            let order_id = fill.get("order_id").and_then(Value::as_u64)?;
            let instrument = order_markets.get(&order_id)?;
            fill["instrument"] = Value::String(instrument.clone());
            Some(fill)
        })
        .collect::<Vec<_>>();
    if fill_rows.len() as u64 != fills_total {
        return Err(invalid_report("多腿成交总数与 Filled 事件数量不一致"));
    }
    fill_rows.reverse();
    fill_rows.truncate(MAX_FILL_ROWS);
    let (equity_truncated, equity_rows) = sampled_equity_rows(equity);
    let instrument = format!(
        "multi-leg[{},{}]",
        requested.primary_instrument, requested.reference_instrument
    );
    let strategy_id = artifact
        .get("strategy_id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_report("多腿产物缺少策略身份"))?;
    let bound_strategy_diagnostics = strategy_diagnostics
        .map(|report| bind_multi_leg_diagnostic(report, strategy_id, &markets))
        .transpose()?;
    let bound_market_strategy_diagnostics = market_strategy_diagnostics
        .map(|reports| bind_multi_leg_diagnostics(reports, strategy_id, &markets))
        .transpose()?;
    let funding_events_bytes = serde_json::to_vec(&spec.funding_events)
        .map_err(|error| invalid_report(format!("多腿 funding_events 编码失败: {error}")))?;
    let funding_events_sha256 = qx_data::sha256_hex_bytes(&funding_events_bytes);
    let summary = serde_json::json!({
        "run_id": requested.run_id,
        "account_id": account_id,
        "markets_total": 2,
        "execution_assumptions": {
            "settlement_currency": spec.settlement_currency,
            "fill_model": spec.fill_model,
            "primary_maker_fee_bps": spec.primary_maker_fee_bps.unwrap_or(spec.maker_fee_bps),
            "primary_taker_fee_bps": spec.primary_taker_fee_bps.unwrap_or(spec.taker_fee_bps),
            "reference_maker_fee_bps": spec.reference_maker_fee_bps.unwrap_or(spec.maker_fee_bps),
            "reference_taker_fee_bps": spec.reference_taker_fee_bps.unwrap_or(spec.taker_fee_bps),
            "funding_bps": spec.funding_bps,
            "funding_events_count": spec.funding_events.len(),
            "funding_events_sha256": funding_events_sha256,
            "market_specs": effective_specs,
        },
        "successful_fills": fills_total,
        "primary_fills": requested.primary_fills,
        "reference_fills": requested.reference_fills,
        "event_count": events.len(),
        "account": expected_ledger,
        "metrics": {
            "return_bps": computed_return,
            "max_drawdown_bps": computed_drawdown,
            "final_equity_raw": final_equity.to_string(),
            "primary_return_bps": primary_return,
            "reference_return_bps": reference_return,
            "fees_raw": total_fees.to_string(),
            "funding_raw": total_funding.to_string(),
            "turnover_raw": total_turnover.to_string(),
            "cost_bps": cost_bps
        },
        "fills": fills_total,
        "result_hash": shared_result_hash,
        "data_fingerprint": data_fingerprint,
        "markets": markets.iter().map(|market| serde_json::json!({
            "instrument": market.instrument,
            "data_fingerprint": market.data_fingerprint,
            "fills": market.fill_count,
            "quality_status": market.quality_status,
            "bar_count": market.bars.total
        })).collect::<Vec<_>>()
    });
    let report_run_id = requested.run_id.clone();
    Ok(ResearchArtifactView {
        schema_version: 10,
        run_kind: Some("multi_leg"),
        run_id: report_run_id.clone(),
        instrument,
        verification: VerificationResult {
            run_id: report_run_id,
            result_hash: shared_result_hash.clone(),
            data_fingerprint: data_fingerprint.clone(),
            verified: true,
            checks: vec![
                "多腿 outcome、spec 与归因产物身份一致".into(),
                "两条 BarFrame 和可选数据质量清单已复算".into(),
                "共享 EventLog 已完整重放，digest 与账簿明细一致".into(),
                "逐腿成交数与成交指纹已从事件流复算".into(),
                "共享账户账簿及组合权益、收益和回撤已复核".into(),
            ],
            mismatches: Vec::new(),
        },
        summary,
        equity: CsvView {
            total: equity.len() as u64,
            truncated: equity_truncated,
            rows: equity_rows,
        },
        fills: CsvView {
            total: fills_total,
            truncated: fills_total > MAX_FILL_ROWS as u64,
            rows: fill_rows,
        },
        events: EventView {
            total: events.len() as u64,
            offset: page.offset,
            truncated: event_end < events.len(),
            rows: event_rows,
        },
        bars: empty_bars("unsupported_input_kind"),
        depth: None,
        event_trace_available: Some(true),
        event_instrument: None,
        strategy_diagnostics: bound_strategy_diagnostics,
        market_strategy_diagnostics: bound_market_strategy_diagnostics,
        markets: Some(markets),
        accounts: None,
        account_event_offsets: None,
    })
}

fn multi_leg_event_instruments(
    event: &Value,
    order_markets: &BTreeMap<u64, String>,
    frames: &BTreeMap<String, qx_datastruct::BarFrame>,
) -> Vec<String> {
    let intents = event
        .pointer("/kind/StrategyDecision/decision/intents")
        .and_then(Value::as_array);
    let mut instruments = intents
        .into_iter()
        .flatten()
        .filter_map(|intent| intent.get("instrument").and_then(canonical_instrument))
        .filter(|instrument| frames.contains_key(instrument))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if instruments.is_empty() {
        if let Some(instrument) = event_market_instrument(event, &BTreeMap::new(), order_markets)
            .filter(|instrument| frames.contains_key(instrument))
        {
            instruments.push(instrument);
        }
    }
    instruments
}

fn bind_multi_leg_diagnostic(
    report: &Value,
    strategy_id: &str,
    markets: &[MarketResearchView],
) -> Result<BoundStrategyDiagnostics, AppError> {
    let instrument = report
        .get("instrument")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_report("多腿策略诊断缺少 instrument"))?;
    let market = markets
        .iter()
        .find(|market| market.instrument == instrument)
        .ok_or_else(|| invalid_report(format!("多腿策略诊断标的不属于本次运行: {instrument}")))?;
    if report.get("schema_version").and_then(Value::as_u64) != Some(1)
        || report.get("kind").and_then(Value::as_str) != Some("qianxing-strategy-bias-diagnostics")
        || report
            .get("input_bars")
            .and_then(Value::as_u64)
            .is_none_or(|count| count == 0)
        || report.get("as_of").and_then(Value::as_u64).is_none()
        || report.get("lookahead").and_then(Value::as_object).is_none()
        || report.get("startup").and_then(Value::as_object).is_none()
        || report.get("strategy_id").and_then(Value::as_str) != Some(strategy_id)
        || report.get("data_fingerprint").and_then(Value::as_str)
            != Some(market.data_fingerprint.as_str())
    {
        return Err(invalid_report("多腿策略诊断与策略身份或腿级输入指纹不匹配"));
    }
    Ok(BoundStrategyDiagnostics {
        identity_bound: true,
        computation: "client_computed_not_reexecuted_by_api",
        report: report.clone(),
    })
}

fn bind_multi_leg_diagnostics(
    reports: &Value,
    strategy_id: &str,
    markets: &[MarketResearchView],
) -> Result<Vec<BoundStrategyDiagnostics>, AppError> {
    let reports = reports
        .as_array()
        .ok_or_else(|| invalid_report("多腿 market_strategy_diagnostics 必须是数组"))?;
    if reports.is_empty() || reports.len() > markets.len() {
        return Err(invalid_report("多腿策略诊断数量必须为 1..=2"));
    }
    let mut seen = std::collections::BTreeSet::new();
    reports
        .iter()
        .map(|report| {
            let instrument = report
                .get("instrument")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_report("多腿策略诊断缺少 instrument"))?;
            if !seen.insert(instrument.to_owned()) {
                return Err(invalid_report("多腿策略诊断重复绑定同一标的"));
            }
            bind_multi_leg_diagnostic(report, strategy_id, markets)
        })
        .collect()
}

fn build_depth(
    outcome_value: &Value,
    paths: &AppPaths,
    requested_page: Option<EventPage>,
) -> Result<ResearchArtifactView, AppError> {
    let encoded = serde_json::to_string(outcome_value).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("深度回测结果请求编码失败: {error}"),
        )
    })?;
    let mut outcome = qx_app::DepthBacktestOutcome::from_json(&encoded)?;
    let root = std::fs::canonicalize(&paths.artifact_root)
        .map_err(|error| AppError::from_io("解析 API 研究产物根目录", &error))?;
    outcome.artifacts.run_manifest = rooted_file(&outcome.artifacts.run_manifest, &root)?;
    outcome.artifacts.summary = rooted_file(&outcome.artifacts.summary, &root)?;
    outcome.artifacts.equity = rooted_file(&outcome.artifacts.equity, &root)?;
    outcome.artifacts.fills = rooted_file(&outcome.artifacts.fills, &root)?;
    if let Some(event_trace) = outcome.artifacts.event_trace.as_ref() {
        outcome.artifacts.event_trace = Some(rooted_file(event_trace, &root)?);
    }
    for path in [
        &outcome.artifacts.run_manifest,
        &outcome.artifacts.summary,
        &outcome.artifacts.equity,
        &outcome.artifacts.fills,
    ] {
        check_artifact_size(Path::new(path))?;
    }
    if let Some(event_trace) = outcome.artifacts.event_trace.as_deref() {
        check_artifact_size(Path::new(event_trace))?;
    }
    let mut verification = qx_app::verify_depth_run(
        &outcome,
        &RunContext::new(CallerCapability::Research, outcome.run_id.clone()),
    )?;
    let summary = read_json(Path::new(&outcome.artifacts.summary))?;
    if !verification.verified {
        return Ok(ResearchArtifactView {
            schema_version: 2,
            run_kind: Some("depth"),
            run_id: outcome.run_id,
            instrument: outcome.instrument,
            verification,
            summary,
            equity: empty_view(),
            fills: empty_view(),
            events: empty_event_view(),
            bars: empty_bars("unsupported_input_kind"),
            depth: Some(empty_depth("run_not_verified")),
            event_trace_available: Some(false),
            event_instrument: None,
            strategy_diagnostics: None,
            market_strategy_diagnostics: None,
            markets: None,
            accounts: None,
            account_event_offsets: None,
        });
    }
    verify_declared_run_artifacts(&summary, &root)?;
    let equity = equity_view(Path::new(&outcome.artifacts.equity))?;
    let manifest = read_json(Path::new(&outcome.artifacts.run_manifest))?;
    let (events, fills) = if let Some(event_trace) = outcome.artifacts.event_trace.as_deref() {
        let (mut events, event_fills) =
            event_view(Path::new(event_trace), &summary, &manifest, requested_page)?;
        for row in &mut events.rows {
            if let Some(object) = row.as_object_mut() {
                object.insert("bar_context".into(), Value::Null);
                object.insert(
                    "bar_context_status".into(),
                    Value::String("not_applicable_depth".into()),
                );
            }
        }
        let fills = validated_fill_view(Path::new(&outcome.artifacts.fills), event_fills)?;
        verification
            .checks
            .push("深度 EventLog 已完整重放，摘要/RunManifest digest 与成交明细一致".into());
        (events, fills)
    } else {
        (
            empty_event_view(),
            depth_fill_view(Path::new(&outcome.artifacts.fills))?,
        )
    };
    let depth = raw_depth_view(&summary, &paths.data_root, &root)?;
    Ok(ResearchArtifactView {
        schema_version: 2,
        run_kind: Some("depth"),
        run_id: outcome.run_id,
        instrument: outcome.instrument,
        verification,
        summary,
        equity,
        fills,
        events,
        bars: empty_bars("unsupported_input_kind"),
        depth: Some(depth),
        event_trace_available: Some(outcome.artifacts.event_trace.is_some()),
        event_instrument: None,
        strategy_diagnostics: None,
        market_strategy_diagnostics: None,
        markets: None,
        accounts: None,
        account_event_offsets: None,
    })
}

/// Verify and project the shared-account multi-market artifact into the same bounded report
/// surface as single-market Bar/depth runs. The producer's `replay_verified` marker is ignored:
/// inputs, sidecars, manifest, ledger and fills are independently checked here.
fn build_multi_market(
    outcome_value: &Value,
    paths: &AppPaths,
    requested_page: Option<EventPage>,
    strategy_diagnostics: Option<&Value>,
    market_strategy_diagnostics: Option<&Value>,
) -> Result<ResearchArtifactView, AppError> {
    let requested: MultiMarketBacktestOutcome = serde_json::from_value(outcome_value.clone())
        .map_err(|error| invalid_report(format!("多市场回测结果结构无效: {error}")))?;
    let artifact_root = std::fs::canonicalize(&paths.artifact_root)
        .map_err(|error| AppError::from_io("解析 API 研究产物根目录", &error))?;
    let data_root = std::fs::canonicalize(&paths.data_root)
        .map_err(|error| AppError::from_io("解析 API 数据根目录", &error))?;
    let artifact_path = rooted_file(&requested.artifact_path, &artifact_root)?;
    check_artifact_size(Path::new(&artifact_path))?;
    let artifact = read_json(Path::new(&artifact_path))?;
    if artifact.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(invalid_report("多市场回测产物 schema_version 不受支持"));
    }
    let embedded: MultiMarketBacktestOutcome = serde_json::from_value(
        artifact
            .get("outcome")
            .cloned()
            .ok_or_else(|| invalid_report("多市场回测产物缺少 outcome"))?,
    )
    .map_err(|error| invalid_report(format!("多市场回测产物 outcome 无效: {error}")))?;
    if embedded != requested {
        return Err(invalid_report("请求 outcome 与多市场回测产物身份不一致"));
    }
    let spec: MultiMarketBacktestSpec = serde_json::from_value(
        artifact
            .get("spec")
            .cloned()
            .ok_or_else(|| invalid_report("多市场回测产物缺少 spec"))?,
    )
    .map_err(|error| invalid_report(format!("多市场回测产物 spec 无效: {error}")))?;
    spec.validate()?;
    if spec.run_id != requested.run_id
        || spec.account_id != requested.account_id
        || spec.markets.len() != requested.markets_total
        || spec.markets.len() > MAX_MULTI_MARKETS
    {
        return Err(invalid_report("多市场 spec 与结果摘要不一致"));
    }

    let market_data = artifact
        .get("market_data")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_report("多市场回测产物缺少 market_data"))?;
    if market_data.len() != spec.markets.len() {
        return Err(invalid_report("多市场输入数量与 spec 不一致"));
    }
    let quality_objects = artifact
        .get("dataset_quality")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let per_market_fills = artifact
        .get("per_market_fills")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_report("多市场回测产物缺少逐市场成交统计"))?;
    let events = artifact
        .get("event_log")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_report("多市场产物缺少事件流"))?;
    if events.len() as u64 > MAX_ARTIFACT_ROWS {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            "多市场事件流超过行数上限",
        ));
    }
    let page = requested_page.unwrap_or(EventPage {
        offset: 0,
        limit: MAX_EVENT_ROWS,
    });
    if page.offset > events.len() as u64 {
        return Err(invalid_report("事件分页 offset 超出多市场事件流范围"));
    }
    let event_end = (page.offset as usize)
        .saturating_add(page.limit)
        .min(events.len());
    let strategy_markets = spec
        .markets
        .iter()
        .flat_map(|market| {
            market.strategies.iter().map(move |member| {
                (
                    member.strategy.strategy_id.clone(),
                    market.instrument.clone(),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    let order_markets = events
        .iter()
        .filter_map(|event| {
            let instrument = event_market_instrument(event, &strategy_markets, &BTreeMap::new())?;
            Some((event_order_id(event)?, instrument))
        })
        .collect::<BTreeMap<_, _>>();
    let mut event_targets = BTreeMap::<String, Vec<(u64, u64, u8)>>::new();
    for event in &events[page.offset as usize..event_end] {
        let Some(instrument) = event_market_instrument(event, &strategy_markets, &order_markets)
        else {
            continue;
        };
        if !spec
            .markets
            .iter()
            .any(|market| market.instrument == instrument)
        {
            continue;
        }
        let (Some(seq), Some(ts), Some(priority)) = (
            event.get("seq").and_then(Value::as_u64),
            event.get("ts").and_then(Value::as_u64),
            event
                .get("prio")
                .and_then(Value::as_u64)
                .and_then(|value| u8::try_from(value).ok()),
        ) else {
            continue;
        };
        event_targets
            .entry(instrument)
            .or_default()
            .push((seq, ts, priority));
    }
    let mut market_event_contexts = BTreeMap::<u64, Value>::new();
    let mut data_hasher = qx_core::Fnv1a::new();
    let mut input_components = BTreeMap::new();
    let mut market_specs = BTreeMap::<qx_core::InstrumentId, qx_core::TradingInstrumentSpec>::new();
    let mut final_market_marks = BTreeMap::<qx_core::InstrumentId, qx_core::Price>::new();
    let mut markets = Vec::with_capacity(spec.markets.len());
    let mut seen_instruments = std::collections::BTreeSet::new();
    let mut aggregate_market_fills = 0_u64;
    for market in market_data {
        let instrument = market
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_report("market_data 缺少 instrument"))?;
        if !seen_instruments.insert(instrument.to_owned()) {
            return Err(invalid_report("多市场输入存在重复标的"));
        }
        let configured = spec
            .markets
            .iter()
            .find(|candidate| candidate.instrument == instrument)
            .ok_or_else(|| invalid_report(format!("spec 缺少市场 {instrument}")))?;
        let source = market
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_report(format!("{instrument} 缺少 BarFrame 路径")))?;
        let frame_path = rooted_file(source, &data_root)?;
        check_artifact_size(Path::new(&frame_path))?;
        let frame_json = std::fs::read_to_string(&frame_path)
            .map_err(|error| AppError::from_io("读取多市场 BarFrame", &error))?;
        let frame = qx_datastruct::BarFrame::from_json(&frame_json)
            .map_err(|error| invalid_report(format!("{instrument} BarFrame 无效: {error:?}")))?;
        if frame.ts.len() as u64 > MAX_ARTIFACT_ROWS {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                format!("{instrument} BarFrame 超过 {MAX_ARTIFACT_ROWS} 行上限"),
            ));
        }
        let fingerprint = qx_data::fingerprint_bar_frame(&frame)
            .map_err(|error| invalid_report(format!("{instrument} BarFrame 指纹失败: {error}")))?;
        if frame.instrument.to_string() != instrument
            || market.get("content_hash").and_then(Value::as_str) != Some(fingerprint.as_str())
            || market.get("row_count").and_then(Value::as_u64) != Some(frame.ts.len() as u64)
            || configured.instrument != instrument
        {
            return Err(invalid_report(format!(
                "{instrument} BarFrame 与运行输入身份不一致"
            )));
        }
        let effective_spec = configured
            .market_spec
            .as_ref()
            .map(|snapshot| qx_app::market_spec_from_value(&frame.instrument, snapshot))
            .transpose()
            .map_err(|error| invalid_report(format!("{instrument} market spec 无效: {error}")))?
            .unwrap_or_else(|| {
                qx_core::TradingInstrumentSpec::spot_default(
                    &frame.instrument,
                    &spec.settlement_currency,
                )
            });
        market_specs.insert(frame.instrument.clone(), effective_spec);
        let final_close = frame
            .close_raw
            .last()
            .copied()
            .ok_or_else(|| invalid_report(format!("{instrument} BarFrame 缺少末尾收盘价")))?;
        final_market_marks.insert(
            frame.instrument.clone(),
            qx_core::Price::from_raw(final_close),
        );
        data_hasher.write_text(instrument);
        data_hasher.write_text(&fingerprint);
        input_components.insert(format!("barframe:{instrument}"), fingerprint.clone());
        if let Some(targets) = event_targets.get(instrument) {
            for (seq, event_ts, event_priority) in targets {
                let (bar_context, status) =
                    match multi_market_bar_context(&frame, *event_ts, *event_priority) {
                        Some(context) => (context, "verified"),
                        None => (Value::Null, "before_first_visible_bar"),
                    };
                market_event_contexts.insert(
                    *seq,
                    serde_json::json!({
                        "instrument": instrument,
                        "data_fingerprint": fingerprint,
                        "bar_context": bar_context,
                        "bar_context_status": status
                    }),
                );
            }
        }
        let quality = if let Some(recorded) = quality_objects.get(instrument) {
            let quality_path = recorded
                .get("manifest_path")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_report(format!("{instrument} 质量清单缺少路径")))?;
            if configured.dataset_manifest_path.as_deref() != Some(quality_path) {
                return Err(invalid_report(format!(
                    "{instrument} 质量清单路径与 spec 不一致"
                )));
            }
            let quality_path = rooted_file(quality_path, &data_root)?;
            let bytes = std::fs::read(&quality_path)
                .map_err(|error| AppError::from_io("读取多市场数据质量清单", &error))?;
            let sha256 = qx_data::sha256_hex_bytes(&bytes);
            let payload = std::str::from_utf8(&bytes)
                .map_err(|error| invalid_report(format!("数据质量清单不是 UTF-8: {error}")))?;
            let manifest = qx_data::DatasetManifestV2::from_json(payload)
                .map_err(|error| invalid_report(format!("数据质量清单无效: {error}")))?;
            let frame_sha256 = qx_data::sha256_hex_bytes(
                &std::fs::read(&frame_path)
                    .map_err(|error| AppError::from_io("读取多市场 BarFrame 原文", &error))?,
            );
            let interval = manifest
                .quality_report
                .interval_ms
                .filter(|value| *value > 0)
                .ok_or_else(|| invalid_report("数据质量清单缺少正 interval_ms"))?;
            let missing = frame
                .ts
                .windows(2)
                .try_fold(0_u64, |total, pair| {
                    total.checked_add((pair[1] - pair[0]).div_ceil(interval).saturating_sub(1))
                })
                .ok_or_else(|| invalid_report("BarFrame 缺口计数溢出"))?;
            let manifest_fingerprint = manifest
                .fingerprint()
                .map_err(|error| invalid_report(format!("数据质量清单指纹失败: {error}")))?;
            if manifest.instrument != instrument
                || manifest.tier != qx_data::DatasetTier::Bar
                || manifest.provider_version != qx_data::BARFRAME_DATASET_VERSION
                || manifest.dataset.fingerprint != fingerprint
                || manifest.content_hash != frame_sha256
                || manifest.quality_report.row_count != frame.ts.len() as u64
                || manifest.dataset.start_timestamp != frame.ts.first().copied().unwrap_or_default()
                || manifest.dataset.end_timestamp != frame.ts.last().copied().unwrap_or_default()
                || manifest.quality_report.missing_intervals != missing
                || manifest.quality_report.duplicate_rows != 0
                || manifest.quality_report.out_of_order_rows != 0
                || recorded.get("manifest_sha256").and_then(Value::as_str) != Some(sha256.as_str())
                || recorded.get("manifest_fingerprint").and_then(Value::as_str)
                    != Some(manifest_fingerprint.as_str())
                || recorded.get("quality_report")
                    != Some(
                        &serde_json::to_value(&manifest.quality_report)
                            .map_err(|error| invalid_report(error.to_string()))?,
                    )
            {
                return Err(invalid_report(format!(
                    "{instrument} 数据质量清单与输入不一致"
                )));
            }
            data_hasher.write_text(&manifest_fingerprint);
            input_components.insert(
                format!("dataset-quality:{instrument}"),
                manifest_fingerprint,
            );
            "verified"
        } else {
            if configured.dataset_manifest_path.is_some() {
                return Err(invalid_report(format!(
                    "{instrument} spec 声明了质量清单但产物未记录"
                )));
            }
            "not_provided"
        };
        let fill_count = per_market_fills
            .get(instrument)
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid_report(format!("{instrument} 逐市场成交数无效")))?;
        aggregate_market_fills = aggregate_market_fills
            .checked_add(fill_count)
            .ok_or_else(|| invalid_report("逐市场成交数总和溢出"))?;
        markets.push(MarketResearchView {
            instrument: instrument.to_owned(),
            data_fingerprint: fingerprint,
            fill_count,
            quality_status: quality,
            bars: barframe_view(&frame),
            pnl_contribution: None,
            data_tier: None,
            depth: None,
            source_end_ts: frame.ts.last().copied(),
        });
    }
    if seen_instruments.len() != spec.markets.len() {
        return Err(invalid_report("spec 中有未进入 market_data 的标的"));
    }
    if per_market_fills.len() != spec.markets.len()
        || aggregate_market_fills != requested.successful_fills
    {
        return Err(invalid_report("逐市场成交数与共享账户成交总数不一致"));
    }
    let data_fingerprint = format!("{:016x}", data_hasher.finish());
    let spec_fingerprint = multi_market_spec_fingerprint(&spec, &artifact)?;
    if data_fingerprint != requested.data_fingerprint
        || spec_fingerprint != requested.spec_fingerprint
    {
        return Err(invalid_report("多市场数据或规格指纹与结果摘要不一致"));
    }

    let instrument_equity_sidecar =
        if let Some(sidecar_path) = artifact.get("instrument_equity_contributions_path") {
            let sidecar_path = sidecar_path
                .as_str()
                .ok_or_else(|| invalid_report("逐标的权益贡献路径必须是字符串"))?;
            let sidecar_path = rooted_file(sidecar_path, &artifact_root)?;
            check_artifact_size(Path::new(&sidecar_path))?;
            let bytes = std::fs::read(&sidecar_path)
                .map_err(|error| AppError::from_io("读取多市场逐标的权益贡献", &error))?;
            input_components.insert(
                "derived:instrument-equity-contributions".into(),
                qx_data::sha256_hex_bytes(&bytes),
            );
            Some(serde_json::from_slice::<Value>(&bytes).map_err(|error| {
                invalid_report(format!("多市场逐标的权益贡献 JSON 无效: {error}"))
            })?)
        } else {
            None
        };

    let manifest_path = artifact
        .get("run_manifest_path")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_report("多市场产物缺少 RunManifest 路径"))?;
    let manifest_path = rooted_file(manifest_path, &artifact_root)?;
    check_artifact_size(Path::new(&manifest_path))?;
    let manifest = qx_core::RunManifest::from_json(
        &std::fs::read_to_string(&manifest_path)
            .map_err(|error| AppError::from_io("读取多市场 RunManifest", &error))?,
    )
    .map_err(|error| invalid_report(format!("多市场 RunManifest 无效: {error:?}")))?;
    let result_hash = &requested.shared_result_hash;
    if manifest.run_id != requested.run_id
        || manifest.data_fingerprint != data_fingerprint
        || manifest.input_components != input_components
        || manifest.result_hash != *result_hash
        || manifest.input_event_hash != data_fingerprint
        || manifest.strategy_version != format!("multi-market-strategy-set:{spec_fingerprint}")
        || manifest.global_seed != spec.seed
    {
        return Err(invalid_report("多市场 RunManifest 与输入/结果身份不一致"));
    }

    let typed_events: Vec<qx_core::Event> = serde_json::from_value(Value::Array(events.clone()))
        .map_err(|error| invalid_report(format!("多市场事件流无效: {error}")))?;
    let (event_log, ledger) = qx_core::ReplayVerifier::replay_owned(typed_events)
        .map_err(|error| invalid_report(format!("多市场事件流重放失败: {error:?}")))?;
    if format!("{:016x}", event_log.digest()) != manifest.output_event_hash
        || serde_json::to_value(ledger.entries())
            .map_err(|error| invalid_report(error.to_string()))?
            != artifact
                .get("ledger_entries")
                .cloned()
                .unwrap_or(Value::Null)
    {
        return Err(invalid_report("多市场事件重放账簿与 RunManifest 不一致"));
    }

    let equity_path = artifact
        .get("equity_path")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_report("多市场产物缺少 equity_path"))?;
    let fills_path = artifact
        .get("fills_path")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_report("多市场产物缺少 fills_path"))?;
    let equity_path = rooted_file(equity_path, &artifact_root)?;
    let fills_path = rooted_file(fills_path, &artifact_root)?;
    check_artifact_size(Path::new(&equity_path))?;
    check_artifact_size(Path::new(&fills_path))?;
    let equity = read_json(Path::new(&equity_path))?;
    let fills = read_json(Path::new(&fills_path))?;
    if equity != artifact.get("equity").cloned().unwrap_or(Value::Null) {
        return Err(invalid_report("多市场权益 sidecar 与运行产物不一致"));
    }
    let event_fills: Vec<Value> = events
        .iter()
        .filter_map(|event| event.pointer("/kind/Filled/fill").cloned())
        .collect();
    let fill_rows = fills
        .as_array()
        .ok_or_else(|| invalid_report("多市场成交 sidecar 必须是数组"))?;
    if fills != Value::Array(event_fills) || fill_rows.len() as u64 != requested.successful_fills {
        return Err(invalid_report(
            "多市场成交 sidecar 与已重放 Filled 事件不一致",
        ));
    }
    let full_equity_rows = equity
        .as_array()
        .ok_or_else(|| invalid_report("多市场权益曲线必须是数组"))?;
    if full_equity_rows.len() as u64 != requested.equity_points || full_equity_rows.is_empty() {
        return Err(invalid_report("多市场权益点数与结果摘要不一致"));
    }
    let first_equity = full_equity_rows
        .first()
        .and_then(|row| row.get(1))
        .and_then(raw_i128)
        .ok_or_else(|| invalid_report("多市场权益首点无效"))?;
    let last_equity = full_equity_rows
        .last()
        .and_then(|row| row.get(1))
        .and_then(raw_i128)
        .ok_or_else(|| invalid_report("多市场权益末点无效"))?;
    if first_equity != spec.initial_cash_raw || last_equity != requested.final_equity_raw {
        return Err(invalid_report("多市场权益曲线与初始资金/期末权益不一致"));
    }
    let computed_return = last_equity
        .checked_sub(first_equity)
        .and_then(|value| value.checked_mul(10_000))
        .and_then(|value| value.checked_div(first_equity))
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid_report("多市场收益基点无法复算"))?;
    let computed_drawdown = equity_drawdown_bps(full_equity_rows)?;
    if computed_return != requested.return_bps || computed_drawdown != requested.max_drawdown_bps {
        return Err(invalid_report("多市场聚合收益或最大回撤与权益曲线不一致"));
    }
    if let Some(sidecar) = instrument_equity_sidecar.as_ref() {
        verify_multi_market_pnl_contributions(
            sidecar,
            &mut markets,
            full_equity_rows,
            &spec,
            &ledger,
            &market_specs,
            &final_market_marks,
        )?;
    }

    let mut event_rows = events[page.offset as usize..event_end].to_vec();
    for row in &mut event_rows {
        attach_research_fact_projection(row);
        let seq = row.get("seq").and_then(Value::as_u64);
        if let Some(context) = seq.and_then(|seq| market_event_contexts.get(&seq)) {
            row["market_context"] = serde_json::json!({
                "instrument": context["instrument"],
                "data_fingerprint": context["data_fingerprint"]
            });
            row["bar_context"] = context["bar_context"].clone();
            row["bar_context_status"] = context["bar_context_status"].clone();
        } else {
            row["market_context"] = Value::Null;
            row["bar_context"] = Value::Null;
            row["bar_context_status"] = Value::String("input_unavailable".into());
        }
    }
    let equity_total = full_equity_rows.len() as u64;
    let (equity_truncated, equity_rows) = sampled_equity_rows(full_equity_rows);
    let fills_rows = fill_rows
        .iter()
        .rev()
        .take(MAX_FILL_ROWS)
        .cloned()
        .collect::<Vec<_>>();
    let instrument = format!(
        "portfolio[{}]",
        markets
            .iter()
            .map(|market| market.instrument.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    let bound_strategy_diagnostics = strategy_diagnostics
        .map(|report| bind_multi_market_diagnostic(report, &spec, &markets))
        .transpose()?;
    let bound_market_strategy_diagnostics = market_strategy_diagnostics
        .map(|reports| bind_multi_market_diagnostics(reports, &spec, &markets))
        .transpose()?;
    let summary = serde_json::json!({
        "run_id": requested.run_id,
        "account_id": requested.account_id,
        "markets_total": requested.markets_total,
        "strategies_total": requested.strategies_total,
        "execution_assumptions": {
            "settlement_currency": spec.settlement_currency,
            "markets": spec.markets.iter().map(|market| serde_json::json!({
                "instrument": market.instrument,
                "priority": market.priority,
                "market_spec": market.market_spec,
                "fee_schedule": market.fee_schedule,
            })).collect::<Vec<_>>(),
        },
        "successful_fills": requested.successful_fills,
        "fills": requested.successful_fills,
        "event_count": events.len(),
        "account": {"account_id": requested.account_id, "initial_cash_raw": spec.initial_cash_raw.to_string()},
        "metrics": {
            "return_bps": requested.return_bps,
            "max_drawdown_bps": requested.max_drawdown_bps,
            "final_equity_raw": requested.final_equity_raw.to_string()
        },
        "result_hash": requested.shared_result_hash,
        "data_fingerprint": data_fingerprint,
        "markets": markets.iter().map(|market| serde_json::json!({
            "instrument": market.instrument,
            "data_fingerprint": market.data_fingerprint,
            "fills": market.fill_count,
            "quality_status": market.quality_status,
            "bar_count": market.bars.total
        })).collect::<Vec<_>>()
    });
    Ok(ResearchArtifactView {
        schema_version: if instrument_equity_sidecar.is_some() {
            12
        } else if bound_market_strategy_diagnostics.is_some() {
            9
        } else if bound_strategy_diagnostics.is_some() {
            8
        } else {
            3
        },
        run_kind: Some("multi_market"),
        run_id: requested.run_id.clone(),
        instrument,
        verification: VerificationResult {
            run_id: requested.run_id,
            result_hash: requested.shared_result_hash,
            data_fingerprint,
            verified: true,
            checks: vec![
                "多市场 outcome 与磁盘产物相等".into(),
                "各标的 BarFrame、数据清单与聚合指纹已复算".into(),
                "RunManifest 输入组件、规格与结果身份已核对".into(),
                "EventLog 已完整重放且账簿/成交副产物相等".into(),
                "共享权益曲线、收益和最大回撤已复算".into(),
                if instrument_equity_sidecar.is_some() {
                    "逐标的权益贡献采样已与共享权益守恒并和重放账簿期末估值核对".into()
                } else {
                    "该历史产物没有逐标的权益贡献副产物".into()
                },
            ],
            mismatches: Vec::new(),
        },
        summary,
        equity: CsvView {
            total: equity_total,
            truncated: equity_truncated,
            rows: equity_rows,
        },
        fills: CsvView {
            total: requested.successful_fills,
            truncated: requested.successful_fills > MAX_FILL_ROWS as u64,
            rows: fills_rows,
        },
        events: EventView {
            total: events.len() as u64,
            offset: page.offset,
            truncated: event_end < events.len(),
            rows: event_rows,
        },
        bars: empty_bars("unsupported_input_kind"),
        depth: None,
        event_trace_available: Some(true),
        event_instrument: None,
        strategy_diagnostics: bound_strategy_diagnostics,
        market_strategy_diagnostics: bound_market_strategy_diagnostics,
        markets: Some(markets),
        accounts: None,
        account_event_offsets: None,
    })
}

fn verify_multi_market_pnl_contributions(
    sidecar: &Value,
    markets: &mut [MarketResearchView],
    equity_rows: &[Value],
    spec: &MultiMarketBacktestSpec,
    ledger: &qx_core::Ledger,
    market_specs: &BTreeMap<qx_core::InstrumentId, qx_core::TradingInstrumentSpec>,
    final_market_marks: &BTreeMap<qx_core::InstrumentId, qx_core::Price>,
) -> Result<(), AppError> {
    let sidecar = sidecar
        .as_object()
        .ok_or_else(|| invalid_report("逐标的权益贡献必须是对象"))?;
    let expected_instruments = markets
        .iter()
        .map(|market| market.instrument.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let sidecar_instruments = sidecar
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if sidecar_instruments != expected_instruments {
        return Err(invalid_report("逐标的权益贡献标的集合与市场输入不一致"));
    }

    let mut curves = BTreeMap::<String, Vec<(u64, u64, i128)>>::new();
    for instrument in &expected_instruments {
        let rows = sidecar
            .get(instrument)
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_report(format!("{instrument} 权益贡献必须是数组")))?;
        if rows.is_empty() || rows.len() > MAX_BAR_POINTS as usize {
            return Err(invalid_report(format!("{instrument} 权益贡献采样点数无效")));
        }
        let mut curve = Vec::with_capacity(rows.len());
        for row in rows {
            let index = row
                .get("equity_index")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid_report(format!("{instrument} 权益贡献索引无效")))?;
            let ts = row
                .get("ts")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid_report(format!("{instrument} 权益贡献时间戳无效")))?;
            let raw = row
                .get("pnl_contribution_raw")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<i128>().ok())
                .ok_or_else(|| invalid_report(format!("{instrument} 权益贡献金额无效")))?;
            if row.as_object().is_none_or(|fields| {
                fields.len() != 3
                    || !fields.contains_key("equity_index")
                    || !fields.contains_key("ts")
                    || !fields.contains_key("pnl_contribution_raw")
            }) {
                return Err(invalid_report(format!(
                    "{instrument} 权益贡献点字段不受支持"
                )));
            }
            if curve
                .last()
                .is_some_and(|(previous, _, _)| index <= *previous)
            {
                return Err(invalid_report(format!(
                    "{instrument} 权益贡献索引未严格递增"
                )));
            }
            curve.push((index, ts, raw));
        }
        if curve.first().map(|sample| sample.0) != Some(0)
            || curve.last().map(|sample| sample.0) != Some(equity_rows.len() as u64 - 1)
        {
            return Err(invalid_report(format!(
                "{instrument} 权益贡献采样没有覆盖权益曲线首尾"
            )));
        }
        curves.insert(instrument.clone(), curve);
    }

    let sample_count = curves
        .values()
        .next()
        .map(Vec::len)
        .ok_or_else(|| invalid_report("逐标的权益贡献没有曲线"))?;
    if curves.values().any(|curve| curve.len() != sample_count) {
        return Err(invalid_report("逐标的权益贡献采样索引集合不一致"));
    }
    for sample_index in 0..sample_count {
        let (equity_index, ts) = curves
            .values()
            .next()
            .and_then(|curve| curve.get(sample_index))
            .map(|(index, ts, _)| (*index, *ts))
            .ok_or_else(|| invalid_report("逐标的权益贡献采样点缺失"))?;
        let equity_row = equity_rows
            .get(usize::try_from(equity_index).map_err(|_| invalid_report("权益索引超出范围"))?)
            .ok_or_else(|| invalid_report("权益贡献索引超出共享权益曲线"))?;
        if equity_row.get(0).and_then(Value::as_u64) != Some(ts) {
            return Err(invalid_report("逐标的权益贡献时间戳与共享权益曲线不一致"));
        }
        let mut contribution_sum = 0_i128;
        for (instrument, curve) in &curves {
            let (index, instrument_ts, contribution) = curve[sample_index];
            if index != equity_index || instrument_ts != ts {
                return Err(invalid_report("各标的权益贡献采样轴不一致"));
            }
            contribution_sum = contribution_sum
                .checked_add(contribution)
                .ok_or_else(|| invalid_report("逐标的权益贡献求和溢出"))?;
            if sample_index + 1 == sample_count {
                let instrument_id = qx_core::InstrumentId::parse(instrument)
                    .ok_or_else(|| invalid_report(format!("无效标的标识: {instrument}")))?;
                let instrument_spec = market_specs
                    .get(&instrument_id)
                    .ok_or_else(|| invalid_report(format!("{instrument} 缺少有效市场规格")))?;
                let mark = final_market_marks
                    .get(&instrument_id)
                    .ok_or_else(|| invalid_report(format!("{instrument} 缺少末尾行情标记")))?;
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
                        invalid_report(format!("{instrument} 期末盈亏贡献复算失败: {error}"))
                    })?;
                if recomputed != contribution {
                    return Err(invalid_report(format!(
                        "{instrument} 期末盈亏贡献与重放账簿估值不一致"
                    )));
                }
            }
        }
        let reconstructed_equity = spec
            .initial_cash_raw
            .checked_add(contribution_sum)
            .ok_or_else(|| invalid_report("共享权益归因重建溢出"))?;
        let expected_equity = equity_row
            .get(1)
            .and_then(raw_i128)
            .ok_or_else(|| invalid_report("权益贡献校验目标无效"))?;
        if reconstructed_equity != expected_equity {
            return Err(invalid_report(format!(
                "第 {equity_index} 个采样点的逐标的贡献与共享账户权益不守恒"
            )));
        }
    }

    for market in markets {
        let curve = curves
            .get(&market.instrument)
            .ok_or_else(|| invalid_report("市场缺少权益贡献曲线"))?;
        let rows = curve
            .iter()
            .map(|(equity_index, ts, raw)| {
                json!({
                    "equity_index": equity_index,
                    "ts": ts,
                    "pnl_contribution_raw": raw.to_string()
                })
            })
            .collect::<Vec<_>>();
        market.pnl_contribution = Some(PnlContributionView {
            currency: spec.settlement_currency.clone(),
            total: equity_rows.len() as u64,
            truncated: rows.len() < equity_rows.len(),
            rows,
        });
    }
    Ok(())
}

/// Independently verify and expose shared-account multi-market L1/L2 backtests. Depth runs use
/// a different persisted artifact from Bar portfolios, so they must be dispatched and replayed
/// explicitly instead of being coerced into either neighboring result shape.
fn build_multi_market_depth(
    outcome_value: &Value,
    paths: &AppPaths,
    requested_page: Option<EventPage>,
    market_strategy_diagnostics: Option<&Value>,
) -> Result<ResearchArtifactView, AppError> {
    let requested: MultiMarketDepthBacktestOutcome = serde_json::from_value(outcome_value.clone())
        .map_err(|error| invalid_report(format!("多市场深度结果结构无效: {error}")))?;
    let artifact_root = std::fs::canonicalize(&paths.artifact_root)
        .map_err(|error| AppError::from_io("解析 API 研究产物根目录", &error))?;
    let data_root = std::fs::canonicalize(&paths.data_root)
        .map_err(|error| AppError::from_io("解析 API 数据根目录", &error))?;
    let artifact_path = rooted_file(&requested.artifact_path, &artifact_root)?;
    check_artifact_size(Path::new(&artifact_path))?;
    let artifact = read_json(Path::new(&artifact_path))?;
    if artifact.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(invalid_report("多市场深度产物 schema_version 不受支持"));
    }
    let embedded: MultiMarketDepthBacktestOutcome = serde_json::from_value(
        artifact
            .get("outcome")
            .cloned()
            .ok_or_else(|| invalid_report("多市场深度产物缺少 outcome"))?,
    )
    .map_err(|error| invalid_report(format!("多市场深度产物 outcome 无效: {error}")))?;
    if embedded != requested {
        return Err(invalid_report("请求结果与多市场深度产物身份不一致"));
    }
    let spec: MultiMarketDepthBacktestSpec = serde_json::from_value(
        artifact
            .get("spec")
            .cloned()
            .ok_or_else(|| invalid_report("多市场深度产物缺少 spec"))?,
    )
    .map_err(|error| invalid_report(format!("多市场深度 spec 无效: {error}")))?;
    spec.validate()?;
    if spec.run_id != requested.run_id
        || spec.account_id != requested.account_id
        || spec.markets.len() != requested.markets_total
        || spec.markets.len() > MAX_MULTI_MARKETS
    {
        return Err(invalid_report("多市场深度 spec 与结果摘要不一致"));
    }

    let market_data = artifact
        .get("market_data")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_report("多市场深度产物缺少 market_data"))?;
    let per_market_fills = artifact
        .get("per_market_fills")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_report("多市场深度产物缺少逐市场成交统计"))?;
    if market_data.len() != spec.markets.len() || per_market_fills.len() != spec.markets.len() {
        return Err(invalid_report("多市场深度输入/成交统计数量与 spec 不一致"));
    }

    let mut data_hasher = qx_core::Fnv1a::new();
    let mut input_components = BTreeMap::new();
    let mut markets = Vec::with_capacity(spec.markets.len());
    let mut seen_instruments = std::collections::BTreeSet::new();
    let mut total_snapshots = 0_u64;
    let mut aggregate_fills = 0_u64;
    for configured in &spec.markets {
        let instrument = configured.instrument.as_str();
        if !seen_instruments.insert(instrument.to_owned()) {
            return Err(invalid_report("多市场深度 spec 存在重复标的"));
        }
        let identity = market_data
            .iter()
            .find(|item| item.get("instrument").and_then(Value::as_str) == Some(instrument))
            .ok_or_else(|| invalid_report(format!("market_data 缺少 {instrument}")))?;
        let source_path = identity
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_report(format!("{instrument} 缺少深度输入路径")))?;
        let frame_path = rooted_file(source_path, &data_root)?;
        if rooted_file(&configured.depth_path, &data_root)? != frame_path {
            return Err(invalid_report(format!(
                "{instrument} 深度输入路径与 spec 不一致"
            )));
        }
        check_artifact_size(Path::new(&frame_path))?;
        let frame_bytes = std::fs::read(&frame_path)
            .map_err(|error| AppError::from_io("读取多市场深度输入", &error))?;
        let frame_text = std::str::from_utf8(&frame_bytes).map_err(|error| {
            invalid_report(format!("{instrument} DepthFrame 不是 UTF-8: {error}"))
        })?;
        let frame = qx_xingban::DepthFrame::from_json(frame_text)
            .map_err(|error| invalid_report(format!("{instrument} DepthFrame 无效: {error}")))?;
        let tier = configured.data_tier.as_deref().unwrap_or("l2");
        let frame_hash = format!("{:016x}", frame.input_hash());
        let snapshot_count = u64::try_from(frame.snapshots.len())
            .map_err(|_| invalid_report("深度快照数量超出 u64"))?;
        if frame.instrument.to_string() != instrument
            || identity.get("data_tier").and_then(Value::as_str) != Some(tier)
            || identity.get("source").and_then(Value::as_str) != Some(frame.source.as_str())
            || identity.get("content_hash").and_then(Value::as_str) != Some(frame_hash.as_str())
            || identity.get("snapshot_count").and_then(Value::as_u64) != Some(snapshot_count)
        {
            return Err(invalid_report(format!(
                "{instrument} DepthFrame 输入身份不一致"
            )));
        }
        if snapshot_count > MAX_ARTIFACT_ROWS {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                format!("{instrument} DepthFrame 超过 {MAX_ARTIFACT_ROWS} 条快照上限"),
            ));
        }
        if tier == "l1"
            && frame
                .snapshots
                .iter()
                .any(|snapshot| snapshot.bids.len() != 1 || snapshot.asks.len() != 1)
        {
            return Err(invalid_report(format!(
                "{instrument} 声明 L1 但输入含多档盘口"
            )));
        }
        total_snapshots = total_snapshots
            .checked_add(snapshot_count)
            .ok_or_else(|| invalid_report("多市场深度快照总数溢出"))?;
        if total_snapshots > MAX_ARTIFACT_ROWS {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                format!("多市场深度总快照超过 {MAX_ARTIFACT_ROWS} 条上限"),
            ));
        }

        let fingerprint = format!("depth:{tier}:{}:{frame_hash}", frame.source);
        data_hasher.write_text(instrument);
        data_hasher.write_text(tier);
        data_hasher.write_text(&frame.source);
        data_hasher.write_u64(frame.input_hash());
        input_components.insert(format!("depthframe:{instrument}"), fingerprint.clone());

        let quality_status = if let Some(manifest_path) =
            configured.dataset_manifest_path.as_deref()
        {
            let recorded = identity
                .get("dataset_quality")
                .filter(|value| !value.is_null())
                .ok_or_else(|| {
                    invalid_report(format!("{instrument} spec 声明质量清单但产物未记录"))
                })?;
            let manifest_path = rooted_file(manifest_path, &data_root)?;
            let recorded_path = recorded
                .get("manifest_path")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_report(format!("{instrument} 质量清单记录缺少路径")))?;
            if rooted_file(recorded_path, &data_root)? != manifest_path {
                return Err(invalid_report(format!(
                    "{instrument} 质量清单路径与 spec 不一致"
                )));
            }
            check_artifact_size(Path::new(&manifest_path))?;
            let bytes = std::fs::read(&manifest_path)
                .map_err(|error| AppError::from_io("读取多市场深度质量清单", &error))?;
            let payload = std::str::from_utf8(&bytes).map_err(|error| {
                invalid_report(format!("{instrument} 质量清单不是 UTF-8: {error}"))
            })?;
            let manifest = qx_data::DatasetManifestV2::from_json(payload)
                .map_err(|error| invalid_report(format!("{instrument} 质量清单无效: {error}")))?;
            let manifest_sha = qx_data::sha256_hex_bytes(&bytes);
            let manifest_fp = manifest.fingerprint().map_err(|error| {
                invalid_report(format!("{instrument} 质量清单指纹失败: {error}"))
            })?;
            let expected_tier = if tier == "l1" {
                qx_data::DatasetTier::L1
            } else {
                qx_data::DatasetTier::L2
            };
            let first_ts = frame
                .snapshots
                .first()
                .map(|row| row.ts)
                .unwrap_or_default();
            let last_ts = frame.snapshots.last().map(|row| row.ts).unwrap_or_default();
            if manifest.instrument != instrument
                || manifest.tier != expected_tier
                || manifest.provider_version != qx_data::DEPTHFRAME_DATASET_VERSION
                || manifest.dataset.dataset_id != format!("depth-bars:{instrument}")
                || manifest.dataset.version != qx_data::DEPTHFRAME_DATASET_VERSION
                || manifest.dataset.source != frame.source
                || manifest.dataset.schema_version != frame.schema_version
                || manifest.source_lineage.primary_source != frame.source
                || manifest.content_hash != qx_data::sha256_hex_bytes(&frame_bytes)
                || manifest.dataset.fingerprint != frame_hash
                || manifest.dataset.start_timestamp != first_ts
                || manifest.dataset.end_timestamp != last_ts
                || manifest.quality_report.row_count != snapshot_count
                || manifest.quality_report.usable_tiers != [tier]
                || recorded.get("manifest_sha256").and_then(Value::as_str)
                    != Some(manifest_sha.as_str())
                || recorded.get("manifest_fingerprint").and_then(Value::as_str)
                    != Some(manifest_fp.as_str())
                || recorded.get("content_hash").and_then(Value::as_str)
                    != Some(manifest.content_hash.as_str())
                || recorded.get("quality_report")
                    != Some(
                        &serde_json::to_value(&manifest.quality_report)
                            .map_err(|error| invalid_report(error.to_string()))?,
                    )
            {
                return Err(invalid_report(format!(
                    "{instrument} 深度质量清单与输入不一致"
                )));
            }
            let interval = manifest
                .quality_report
                .interval_ms
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    invalid_report(format!("{instrument} 深度质量清单缺少正 interval_ms"))
                })?;
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
                .ok_or_else(|| invalid_report(format!("{instrument} 深度质量计数溢出")))?;
            if manifest.quality_report.duplicate_rows != duplicates
                || manifest.quality_report.out_of_order_rows != out_of_order
                || manifest.quality_report.missing_intervals != missing
            {
                return Err(invalid_report(format!(
                    "{instrument} 深度质量报告与输入不一致"
                )));
            }
            data_hasher.write_text(&manifest_fp);
            input_components.insert(format!("dataset-quality:{instrument}"), manifest_fp);
            "verified"
        } else {
            if identity
                .get("dataset_quality")
                .is_some_and(|value| !value.is_null())
            {
                return Err(invalid_report(format!(
                    "{instrument} 产物包含 spec 未声明的质量清单"
                )));
            }
            "not_provided"
        };

        let fill_count = per_market_fills
            .get(instrument)
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid_report(format!("{instrument} 逐市场成交数无效")))?;
        aggregate_fills = aggregate_fills
            .checked_add(fill_count)
            .ok_or_else(|| invalid_report("多市场逐市场成交数溢出"))?;
        markets.push(MarketResearchView {
            instrument: instrument.to_owned(),
            data_fingerprint: fingerprint,
            fill_count,
            quality_status,
            bars: empty_bars("not_applicable_depth"),
            pnl_contribution: None,
            data_tier: Some(tier.to_owned()),
            depth: Some(depthframe_view(&frame)),
            source_end_ts: frame.snapshots.last().map(|snapshot| snapshot.ts),
        });
    }
    if aggregate_fills != requested.successful_fills {
        return Err(invalid_report("多市场逐市场成交数与共享账户成交总数不一致"));
    }
    let data_fingerprint = format!("{:016x}", data_hasher.finish());
    if data_fingerprint != requested.data_fingerprint {
        return Err(invalid_report("多市场深度数据指纹与结果摘要不一致"));
    }

    let mut identity_spec = spec.clone();
    identity_spec.output_dir.clear();
    for market in &mut identity_spec.markets {
        market.depth_path.clear();
        market.dataset_manifest_path = None;
    }
    let identity_json = identity_spec.to_json()?;
    let mut spec_hasher = qx_core::Fnv1a::new();
    spec_hasher.write_text(&identity_json);
    let mut identities: Vec<qx_app::StrategyArtifactIdentity> = serde_json::from_value(
        artifact
            .get("strategy_identities")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )
    .map_err(|error| invalid_report(format!("多市场深度策略身份无效: {error}")))?;
    identities.sort_by(|left, right| left.strategy_id.cmp(&right.strategy_id));
    let strategy_ids = spec
        .markets
        .iter()
        .map(|market| market.strategy.strategy_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut seen_strategy_ids = std::collections::BTreeSet::new();
    for identity in &identities {
        if !strategy_ids.contains(identity.strategy_id.as_str())
            || !seen_strategy_ids.insert(identity.strategy_id.as_str())
        {
            return Err(invalid_report(
                "多市场深度自定义策略身份与 spec 不一致或重复",
            ));
        }
        spec_hasher.write_text(&identity.strategy_id);
        spec_hasher.write_text(&identity.strategy_version);
        spec_hasher.write_text(&identity.source_sha256);
    }
    let bound_market_strategy_diagnostics = market_strategy_diagnostics
        .map(|reports| bind_multi_market_depth_diagnostics(reports, &spec, &markets, &identities))
        .transpose()?;
    let spec_fingerprint = format!("{:016x}", spec_hasher.finish());
    if spec_fingerprint != requested.spec_fingerprint {
        return Err(invalid_report("多市场深度规格指纹与结果摘要不一致"));
    }

    let manifest_path = artifact
        .get("run_manifest_path")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_report("多市场深度产物缺少 RunManifest 路径"))?;
    let manifest_path = rooted_file(manifest_path, &artifact_root)?;
    check_artifact_size(Path::new(&manifest_path))?;
    let manifest = qx_core::RunManifest::from_json(
        &std::fs::read_to_string(&manifest_path)
            .map_err(|error| AppError::from_io("读取多市场深度 RunManifest", &error))?,
    )
    .map_err(|error| invalid_report(format!("多市场深度 RunManifest 无效: {error:?}")))?;
    let config_hash = qx_strategy::sha256_hex(
        &serde_json::to_vec(&identity_spec)
            .map_err(|error| invalid_report(format!("多市场深度配置编码失败: {error}")))?,
    );
    let result_hash = &requested.shared_result_hash;
    if manifest.run_id != requested.run_id
        || manifest.config_hash != config_hash
        || manifest.data_fingerprint != data_fingerprint
        || manifest.input_components != input_components
        || manifest.result_hash != *result_hash
        || manifest.input_event_hash != data_fingerprint
        || manifest.output_event_hash != *result_hash
        || manifest.strategy_version != format!("multi-market-depth:{spec_fingerprint}")
        || manifest.instrument_spec_version != "multi-market-depth-spec-v1"
        || manifest.model_fingerprint
            != qx_strategy::sha256_hex(
                b"OrderBookPortfolioBacktestEngine/OrderBookExecutionModel/v1",
            )
        || manifest.global_seed != 0
        || !manifest.determinism_mode
    {
        return Err(invalid_report(
            "多市场深度 RunManifest 与输入/结果身份不一致",
        ));
    }
    if !requested.execution_models.is_empty() {
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
        if requested.execution_models != declared {
            return Err(invalid_report("多市场深度生效撮合参数与 spec 不一致"));
        }
    }

    let events_value = artifact
        .get("event_log")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_report("多市场深度产物缺少事件流"))?;
    if events_value.len() as u64 > MAX_ARTIFACT_ROWS {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            "多市场深度事件轨迹超过 500 万行上限",
        ));
    }
    let typed_events: Vec<qx_core::Event> =
        serde_json::from_value(Value::Array(events_value.clone()))
            .map_err(|error| invalid_report(format!("多市场深度事件流无效: {error}")))?;
    let (event_log, ledger) = qx_core::ReplayVerifier::replay_owned(typed_events)
        .map_err(|error| invalid_report(format!("多市场深度事件重放失败: {error}")))?;
    event_log
        .validate()
        .map_err(|error| invalid_report(format!("多市场深度事件流校验失败: {error}")))?;
    let replay_hash = format!("{:016x}", event_log.digest());
    if replay_hash != *result_hash
        || artifact.get("replay_verified").and_then(Value::as_bool) != Some(true)
        || artifact.get("ledger_entries")
            != Some(
                &serde_json::to_value(ledger.entries())
                    .map_err(|error| invalid_report(error.to_string()))?,
            )
    {
        return Err(invalid_report("多市场深度事件重放与账簿副产物不一致"));
    }

    let equity_path = artifact
        .get("equity_path")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_report("多市场深度产物缺少权益 sidecar"))?;
    let equity_path = rooted_file(equity_path, &artifact_root)?;
    check_artifact_size(Path::new(&equity_path))?;
    let equity_full = read_json(Path::new(&equity_path))?;
    if artifact.get("equity") != Some(&equity_full) {
        return Err(invalid_report("多市场深度权益 sidecar 与产物不一致"));
    }
    let full_equity_rows = equity_full
        .as_array()
        .ok_or_else(|| invalid_report("多市场深度权益曲线必须是数组"))?;
    if full_equity_rows.len() as u64 != requested.equity_points || full_equity_rows.is_empty() {
        return Err(invalid_report("多市场深度权益点数与结果摘要不一致"));
    }
    let first_equity = full_equity_rows
        .first()
        .and_then(|row| row.get(1))
        .and_then(raw_i128)
        .ok_or_else(|| invalid_report("多市场深度权益首点无效"))?;
    let last_equity = full_equity_rows
        .last()
        .and_then(|row| row.get(1))
        .and_then(raw_i128)
        .ok_or_else(|| invalid_report("多市场深度权益末点无效"))?;
    if first_equity != spec.initial_cash_raw || last_equity != requested.final_equity_raw {
        return Err(invalid_report(
            "多市场深度权益曲线与初始资金/期末权益不一致",
        ));
    }
    let computed_return = last_equity
        .checked_sub(first_equity)
        .and_then(|value| value.checked_mul(10_000))
        .and_then(|value| value.checked_div(first_equity))
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid_report("多市场深度收益基点无法复算"))?;
    if computed_return != requested.return_bps
        || equity_drawdown_bps(full_equity_rows)? != requested.max_drawdown_bps
    {
        return Err(invalid_report(
            "多市场深度聚合收益或最大回撤与权益曲线不一致",
        ));
    }

    let fills_path = artifact
        .get("fills_path")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_report("多市场深度产物缺少成交 sidecar"))?;
    let fills_path = rooted_file(fills_path, &artifact_root)?;
    check_artifact_size(Path::new(&fills_path))?;
    let fills_full = read_json(Path::new(&fills_path))?;
    let fills_rows = fills_full
        .as_array()
        .ok_or_else(|| invalid_report("多市场深度成交 sidecar 必须是数组"))?;
    let event_fills = event_log
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            qx_core::EventKind::Filled { fill } => Some(serde_json::to_value(fill)),
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| invalid_report(format!("多市场深度成交编码失败: {error}")))?;
    if fills_full != Value::Array(event_fills)
        || fills_rows.len() as u64 != requested.successful_fills
    {
        return Err(invalid_report(
            "多市场深度成交 sidecar 与重放 Filled 事件不一致",
        ));
    }

    let page = requested_page.unwrap_or(EventPage {
        offset: events_value.len().saturating_sub(MAX_EVENT_ROWS) as u64,
        limit: MAX_EVENT_ROWS,
    });
    if page.offset > events_value.len() as u64 {
        return Err(invalid_report("事件分页 offset 超出多市场深度事件流范围"));
    }
    let event_end = (page.offset as usize)
        .saturating_add(page.limit)
        .min(events_value.len());
    let event_rows = event_log
        .events()
        .get(page.offset as usize..event_end)
        .unwrap_or_default()
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| invalid_report(format!("多市场深度事件序列化失败: {error}")))?;
    let all_events = event_log.events();
    let instrument = format!(
        "portfolio[{}]",
        spec.markets
            .iter()
            .map(|market| market.instrument.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    let (equity_truncated, equity_rows) = sampled_equity_rows(full_equity_rows);
    let fills_preview = fills_rows
        .iter()
        .rev()
        .take(MAX_FILL_ROWS)
        .cloned()
        .collect::<Vec<_>>();
    let summary = json!({
        "run_id": requested.run_id,
        "account_id": requested.account_id,
        "settlement_currency": spec.settlement_currency,
        "markets_total": requested.markets_total,
        "execution_assumptions": {
            "settlement_currency": spec.settlement_currency,
            "markets": spec.markets.iter().map(|market| json!({
                "instrument": market.instrument,
                "data_tier": market.data_tier,
                "priority": market.priority,
                "market_spec": market.market_spec,
                "fee_bps": market.fee_bps,
                "latency_snapshots": market.latency_snapshots,
                "queue_position_bps": market.queue_position_bps,
                "queue_position_model": market.queue_position_model,
                "market_impact_bps": market.market_impact_bps,
                "order_policy": market.order_policy,
            })).collect::<Vec<_>>(),
            "effective_execution_models": requested.execution_models,
        },
        "successful_fills": requested.successful_fills,
        "fills": requested.successful_fills,
        "pending_orders": requested.pending_orders,
        "equity_points": requested.equity_points,
        "event_count": all_events.len(),
        "account": {"account_id": requested.account_id, "initial_cash_raw": spec.initial_cash_raw.to_string()},
        "metrics": {
            "return_bps": requested.return_bps,
            "max_drawdown_bps": requested.max_drawdown_bps,
            "final_equity_raw": requested.final_equity_raw.to_string()
        },
        "result_hash": requested.shared_result_hash,
        "data_fingerprint": data_fingerprint,
        "markets": markets.iter().map(|market| json!({
            "instrument": market.instrument,
            "data_tier": market.data_tier,
            "data_fingerprint": market.data_fingerprint,
            "fills": market.fill_count,
            "quality_status": market.quality_status,
            "snapshot_count": market.depth.as_ref().map(|depth| depth.total).unwrap_or_default()
        })).collect::<Vec<_>>()
    });
    Ok(ResearchArtifactView {
        schema_version: if bound_market_strategy_diagnostics.is_some() {
            11
        } else {
            5
        },
        run_kind: Some("multi_market_depth"),
        run_id: requested.run_id.clone(),
        instrument,
        verification: VerificationResult {
            run_id: requested.run_id,
            result_hash: requested.shared_result_hash,
            data_fingerprint,
            verified: true,
            checks: vec![
                "多市场深度 outcome 与磁盘产物相等".into(),
                "各标的 DepthFrame、L1/L2 档位及可选质量清单已复核".into(),
                "RunManifest 输入组件、规格与结果身份已核对".into(),
                "共享 EventLog 已重放且账簿/成交副产物相等".into(),
                "共享权益曲线、收益和最大回撤已复算".into(),
            ],
            mismatches: Vec::new(),
        },
        summary,
        equity: CsvView {
            total: full_equity_rows.len() as u64,
            truncated: equity_truncated,
            rows: equity_rows,
        },
        fills: CsvView {
            total: requested.successful_fills,
            truncated: requested.successful_fills > MAX_FILL_ROWS as u64,
            rows: fills_preview,
        },
        events: EventView {
            total: all_events.len() as u64,
            offset: page.offset,
            truncated: event_end < all_events.len(),
            rows: event_rows,
        },
        bars: empty_bars("not_applicable_depth"),
        depth: None,
        event_trace_available: Some(true),
        event_instrument: None,
        strategy_diagnostics: None,
        market_strategy_diagnostics: bound_market_strategy_diagnostics,
        markets: Some(markets),
        accounts: None,
        account_event_offsets: None,
    })
}

fn depthframe_view(frame: &qx_xingban::DepthFrame) -> DepthView {
    let total = frame.snapshots.len() as u64;
    let stride = total
        .div_ceil(MAX_BAR_POINTS.saturating_sub(1).max(1))
        .max(1) as usize;
    let selected = (0..frame.snapshots.len())
        .step_by(stride)
        .chain((!frame.snapshots.is_empty()).then_some(frame.snapshots.len() - 1))
        .collect::<std::collections::BTreeSet<_>>();
    let rows = selected
        .into_iter()
        .map(|index| {
            let snapshot = &frame.snapshots[index];
            let levels = |items: &[qx_xingban::BookLevel]| {
                items
                    .iter()
                    .take(20)
                    .map(|level| qx_app::artifact_store::VerifiedDepthLevel {
                        price_raw: level.price.raw().to_string(),
                        qty_raw: level.qty.raw().to_string(),
                    })
                    .collect::<Vec<_>>()
            };
            qx_app::artifact_store::VerifiedDepthFramePoint {
                ts: snapshot.ts,
                sequence: snapshot.sequence,
                bids: levels(&snapshot.bids),
                asks: levels(&snapshot.asks),
                levels_truncated: snapshot.bids.len() > 20 || snapshot.asks.len() > 20,
            }
        })
        .collect::<Vec<_>>();
    DepthView {
        source_status: "verified",
        total,
        truncated: rows.len() < total as usize || rows.iter().any(|row| row.levels_truncated),
        rows,
    }
}

/// Verify every successful account independently, then expose the batch as a bounded
/// interactive report. The batch's weighted summary is recomputed from the verified runs.
fn build_multi_account(
    outcome_value: &Value,
    paths: &AppPaths,
    requested_page: Option<EventPage>,
    requested_event_offsets: Option<&BTreeMap<String, u64>>,
    event_instrument: Option<&str>,
    account_strategy_diagnostics: Option<&Value>,
) -> Result<ResearchArtifactView, AppError> {
    let runs = outcome_value
        .get("runs")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_report("多账户研究结果缺少 runs"))?;
    if !(2..=MAX_MULTI_ACCOUNTS).contains(&runs.len()) {
        return Err(invalid_report(format!(
            "多账户研究结果数量必须在 2..={MAX_MULTI_ACCOUNTS}"
        )));
    }
    let known_account_ids = runs
        .iter()
        .filter_map(|run| run.get("account_id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    let mut account_diagnostic_by_id = BTreeMap::<String, &Value>::new();
    if let Some(entries) = account_strategy_diagnostics {
        let entries = entries
            .as_array()
            .ok_or_else(|| invalid_report("账户策略诊断必须是数组"))?;
        for entry in entries {
            let account_id = entry
                .get("account_id")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_report("账户策略诊断缺少 account_id"))?;
            if !known_account_ids.contains(account_id) {
                return Err(invalid_report(format!(
                    "账户策略诊断引用未知账户 {account_id}"
                )));
            }
            let member = runs
                .iter()
                .find(|run| run.get("account_id").and_then(Value::as_str) == Some(account_id));
            if member
                .and_then(|run| run.get("outcome"))
                .is_none_or(Value::is_null)
            {
                return Err(invalid_report(format!(
                    "失败账户 {account_id} 不能附加策略诊断"
                )));
            }
            if account_diagnostic_by_id
                .insert(account_id.to_owned(), entry)
                .is_some()
            {
                return Err(invalid_report(format!("账户策略诊断重复引用 {account_id}")));
            }
        }
    }
    let matching_account_ids = if let Some(instrument) = event_instrument {
        let matching = runs
            .iter()
            .filter(|run| {
                run.get("outcome")
                    .and_then(|outcome| outcome.get("instrument"))
                    .and_then(Value::as_str)
                    == Some(instrument)
            })
            .filter_map(|run| run.get("account_id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>();
        if matching.is_empty() {
            return Err(invalid_report(format!(
                "多账户批次中不存在标的 {instrument}"
            )));
        }
        matching
    } else {
        known_account_ids.clone()
    };
    let page = requested_page.unwrap_or(EventPage {
        offset: 0,
        limit: MAX_EVENT_ROWS,
    });
    if requested_event_offsets.is_none() && page.offset != 0 {
        return Err(invalid_report(
            "多账户合并事件分页必须提供 account_event_offsets 游标",
        ));
    }
    if requested_event_offsets.is_some_and(|offsets| {
        offsets
            .keys()
            .any(|account_id| !known_account_ids.contains(account_id))
    }) {
        return Err(invalid_report("多账户事件分页游标包含未知 account_id"));
    }
    if requested_event_offsets.is_some_and(|offsets| {
        offsets
            .iter()
            .any(|(account_id, offset)| !matching_account_ids.contains(account_id) && *offset != 0)
    }) {
        return Err(invalid_report("被标的筛选排除的账户事件游标必须为 0"));
    }
    let requested_offset_sum = requested_event_offsets
        .into_iter()
        .flat_map(|offsets| {
            offsets
                .iter()
                .filter(|(account_id, _)| matching_account_ids.contains(*account_id))
                .map(|(_, value)| value)
        })
        .try_fold(0_u64, |total, value| total.checked_add(*value))
        .ok_or_else(|| invalid_report("多账户事件分页游标总数溢出"))?;
    if requested_event_offsets.is_some() && requested_offset_sum != page.offset {
        return Err(invalid_report(
            "多账户事件 offset 必须等于 account_event_offsets 游标总和",
        ));
    }
    let has_depth = runs.iter().any(|run| {
        run.get("outcome")
            .and_then(Value::as_object)
            .is_some_and(|outcome| outcome.contains_key("tier"))
    });
    let (batch_id, settlement_currency, spec_fingerprint, reported_summary) = if has_depth {
        let result: qx_app::MultiAccountDepthBacktestResult =
            serde_json::from_value(outcome_value.clone())
                .map_err(|error| invalid_report(format!("多账户深度结果结构无效: {error}")))?;
        (
            result.batch_id,
            result.settlement_currency,
            result.spec_fingerprint,
            serde_json::to_value(result.summary)
                .map_err(|error| invalid_report(format!("多账户深度摘要编码失败: {error}")))?,
        )
    } else {
        let result: qx_app::MultiAccountBacktestResult =
            serde_json::from_value(outcome_value.clone())
                .map_err(|error| invalid_report(format!("多账户 Bar 结果结构无效: {error}")))?;
        (
            result.batch_id,
            result.settlement_currency,
            result.spec_fingerprint,
            serde_json::to_value(result.summary)
                .map_err(|error| invalid_report(format!("多账户摘要编码失败: {error}")))?,
        )
    };
    if batch_id.is_empty()
        || settlement_currency.is_empty()
        || spec_fingerprint.len() != 16
        || !spec_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid_report("多账户批次身份字段无效"));
    }

    let mut seen_accounts = std::collections::BTreeSet::new();
    let mut accounts = Vec::with_capacity(runs.len());
    let mut successful = 0_usize;
    let mut successful_fills = 0_u64;
    let mut successful_initial_cash_raw = 0_i128;
    let mut weighted_return_numerator = 0_i128;
    let mut data_hasher = qx_core::Fnv1a::new();
    let mut result_hasher = qx_core::Fnv1a::new();
    let mut next_account_event_offsets = BTreeMap::new();
    let mut merged_event_rows = Vec::new();
    let mut merged_event_total = 0_u64;
    data_hasher.write_text(&batch_id);
    result_hasher.write_text(&batch_id);
    result_hasher.write_text(&spec_fingerprint);

    for run in runs {
        let account_id = run
            .get("account_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid_report("多账户成员缺少 account_id"))?;
        if !seen_accounts.insert(account_id.to_owned()) {
            return Err(invalid_report("多账户研究结果含重复 account_id"));
        }
        let Some(account_outcome) = run.get("outcome").filter(|value| !value.is_null()) else {
            let event_offset = requested_event_offsets
                .and_then(|offsets| offsets.get(account_id))
                .copied()
                .unwrap_or(0);
            if event_offset != 0 {
                return Err(invalid_report(format!(
                    "失败账户 {account_id} 的事件游标必须为 0"
                )));
            }
            next_account_event_offsets.insert(account_id.to_owned(), 0);
            accounts.push(AccountResearchView {
                account_id: account_id.to_owned(),
                status: "failed",
                run_id: None,
                instrument: None,
                data_fingerprint: None,
                result_hash: None,
                return_bps: None,
                max_drawdown_bps: None,
                fills: None,
                equity_points: None,
                report: None,
            });
            continue;
        };
        let account_outcome_account = account_outcome.get("account_id").and_then(Value::as_str);
        if account_outcome_account != Some(account_id) {
            return Err(invalid_report(format!(
                "账户 {account_id} 的 outcome.account_id 不匹配"
            )));
        }
        let include_events = matching_account_ids.contains(account_id);
        let event_offset = if include_events {
            requested_event_offsets
                .and_then(|offsets| offsets.get(account_id))
                .copied()
                .unwrap_or(0)
        } else {
            0
        };
        let mut child_request = json!({
            "outcome": account_outcome,
            "event_page": {"offset": event_offset, "limit": page.limit}
        });
        if let Some(diagnostics) = account_diagnostic_by_id.get(account_id) {
            if let Some(strategy) = diagnostics.get("strategy_diagnostics") {
                child_request["strategy_diagnostics"] = (*strategy).clone();
            }
            if let Some(markets) = diagnostics.get("market_strategy_diagnostics") {
                child_request["market_strategy_diagnostics"] = (*markets).clone();
            }
        }
        let child_body = serde_json::to_string(&child_request)
            .map_err(|error| invalid_report(format!("账户 {account_id} 结果编码失败: {error}")))?;
        let child = build(&child_body, paths)?;
        if !child.verification.verified {
            return Err(invalid_report(format!(
                "账户 {account_id} 运行产物复核未通过: {}",
                child.verification.mismatches.join("; ")
            )));
        }
        let verified_instrument = child.instrument.clone();
        if include_events
            && event_instrument.is_some_and(|instrument| instrument != verified_instrument)
        {
            return Err(invalid_report(format!(
                "账户 {account_id} 的复核标的与事件筛选标的不匹配"
            )));
        }
        if include_events
            && (event_offset > child.events.total || child.events.offset != event_offset)
        {
            return Err(invalid_report(format!(
                "账户 {account_id} 事件游标超出已复核事件范围"
            )));
        }
        if include_events {
            merged_event_total = merged_event_total
                .checked_add(child.events.total)
                .ok_or_else(|| invalid_report("多账户事件总数溢出"))?;
            next_account_event_offsets.insert(account_id.to_owned(), event_offset);
            for mut event in child.events.rows.iter().cloned() {
                let Some(event_object) = event.as_object_mut() else {
                    return Err(invalid_report(format!(
                        "账户 {account_id} 的事件行不是 JSON 对象"
                    )));
                };
                event_object.insert("account_id".into(), Value::String(account_id.to_owned()));
                event_object.insert(
                    "instrument".into(),
                    Value::String(verified_instrument.clone()),
                );
                merged_event_rows.push(event);
            }
        } else {
            next_account_event_offsets.insert(account_id.to_owned(), 0);
        }
        let initial_cash_raw = child
            .summary
            .pointer("/account/initial_cash_raw")
            .and_then(raw_i128)
            .or_else(|| {
                child
                    .equity
                    .rows
                    .first()
                    .and_then(|row| row.get("equity_raw"))
                    .and_then(raw_i128)
            })
            .ok_or_else(|| invalid_report(format!("账户 {account_id} summary 缺少有效初始资金")))?;
        let return_bps = account_outcome
            .get("return_bps")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| invalid_report(format!("账户 {account_id} return_bps 无效")))?;
        let fills = account_outcome
            .get("fills")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid_report(format!("账户 {account_id} fills 无效")))?;
        successful = successful
            .checked_add(1)
            .ok_or_else(|| invalid_report("账户成功数溢出"))?;
        successful_fills = successful_fills
            .checked_add(fills)
            .ok_or_else(|| invalid_report("多账户成交总数溢出"))?;
        successful_initial_cash_raw = successful_initial_cash_raw
            .checked_add(initial_cash_raw)
            .ok_or_else(|| invalid_report("多账户初始资金总数溢出"))?;
        weighted_return_numerator = weighted_return_numerator
            .checked_add(
                i128::from(return_bps)
                    .checked_mul(initial_cash_raw)
                    .ok_or_else(|| invalid_report("多账户加权收益乘法溢出"))?,
            )
            .ok_or_else(|| invalid_report("多账户加权收益求和溢出"))?;
        data_hasher.write_text(account_id);
        data_hasher.write_text(&child.verification.data_fingerprint);
        result_hasher.write_text(account_id);
        result_hasher.write_text(&child.verification.result_hash);
        let report = serde_json::to_value(child)
            .map_err(|error| invalid_report(format!("账户 {account_id} 报告编码失败: {error}")))?;
        accounts.push(AccountResearchView {
            account_id: account_id.to_owned(),
            status: "succeeded",
            run_id: account_outcome
                .get("run_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            instrument: Some(verified_instrument),
            data_fingerprint: account_outcome
                .get("data_fingerprint")
                .and_then(Value::as_str)
                .map(str::to_owned),
            result_hash: account_outcome
                .get("result_hash")
                .and_then(Value::as_str)
                .map(str::to_owned),
            return_bps: Some(return_bps),
            max_drawdown_bps: account_outcome
                .get("max_drawdown_bps")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok()),
            fills: Some(fills),
            equity_points: account_outcome.get("equity_points").and_then(Value::as_u64),
            report: Some(report),
        });
    }

    merged_event_rows.sort_by(compare_merged_events);
    merged_event_rows.truncate(page.limit);
    for event in &merged_event_rows {
        let account_id = event
            .get("account_id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_report("合并事件缺少 account_id"))?;
        let offset = next_account_event_offsets
            .get_mut(account_id)
            .ok_or_else(|| invalid_report("合并事件引用未知账户"))?;
        *offset = offset
            .checked_add(1)
            .ok_or_else(|| invalid_report("账户事件游标溢出"))?;
    }

    let failed = runs.len() - successful;
    let weighted_return_bps = if successful_initial_cash_raw == 0 {
        None
    } else {
        Some(
            i32::try_from(weighted_return_numerator / successful_initial_cash_raw)
                .map_err(|_| invalid_report("多账户加权收益超出 i32 范围"))?,
        )
    };
    let expected_summary = json!({
        "accounts_total": runs.len(),
        "accounts_succeeded": successful,
        "accounts_failed": failed,
        "successful_initial_cash_raw": successful_initial_cash_raw,
        "successful_fills": successful_fills,
        "weighted_return_bps": weighted_return_bps,
        "complete": failed == 0
    });
    if reported_summary != expected_summary {
        return Err(invalid_report("多账户已声明汇总与逐账户复核结果不一致"));
    }
    let data_fingerprint = format!("{:016x}", data_hasher.finish());
    let result_hash = format!("{:016x}", result_hasher.finish());
    let event_trace_available = accounts.iter().any(|account| {
        account.status == "succeeded"
            && account.report.as_ref().is_some_and(|report| {
                report
                    .get("event_trace_available")
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| {
                        report
                            .pointer("/events/total")
                            .and_then(Value::as_u64)
                            .is_some()
                    })
            })
    });
    let event_offset = requested_offset_sum;
    let event_page_end = event_offset
        .checked_add(merged_event_rows.len() as u64)
        .ok_or_else(|| invalid_report("多账户事件分页结束位置溢出"))?;
    Ok(ResearchArtifactView {
        schema_version: 7,
        run_kind: Some("multi_account"),
        run_id: batch_id.clone(),
        instrument: format!("multi-account[{}]", settlement_currency),
        verification: VerificationResult {
            run_id: batch_id.clone(),
            result_hash,
            data_fingerprint,
            verified: true,
            checks: vec![
                "多账户批次成员身份与汇总计数已核对".into(),
                "每个成功账户的输入、RunManifest、事件重放和副产物均已独立复核".into(),
                "成功本金、成交总数和加权收益已从账户结果复算".into(),
                "跨账户事件按时间、优先级、账户和序号稳定合并，所有账户事件页均已独立重放复核"
                    .into(),
            ],
            mismatches: Vec::new(),
        },
        summary: json!({
            "batch_id": batch_id,
            "settlement_currency": settlement_currency,
            "spec_fingerprint": spec_fingerprint,
            "accounts_total": runs.len(),
            "accounts_succeeded": successful,
            "accounts_failed": failed,
            "successful_initial_cash_raw": successful_initial_cash_raw.to_string(),
            "successful_fills": successful_fills,
            "weighted_return_bps": weighted_return_bps,
            "complete": failed == 0
        }),
        equity: empty_view(),
        fills: empty_view(),
        events: EventView {
            total: merged_event_total,
            offset: event_offset,
            truncated: event_page_end < merged_event_total,
            rows: merged_event_rows,
        },
        bars: empty_bars("unsupported_input_kind"),
        depth: None,
        event_trace_available: Some(event_trace_available),
        event_instrument: event_instrument.map(str::to_owned),
        strategy_diagnostics: None,
        market_strategy_diagnostics: None,
        markets: None,
        accounts: Some(accounts),
        account_event_offsets: Some(next_account_event_offsets),
    })
}

fn invalid_report(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::Conflict, message)
}

fn multi_market_spec_fingerprint(
    spec: &MultiMarketBacktestSpec,
    artifact: &Value,
) -> Result<String, AppError> {
    let mut identity_spec = spec.clone();
    identity_spec.output_dir.clear();
    for market in &mut identity_spec.markets {
        market.bars_path.clear();
        market.dataset_manifest_path = None;
    }
    let spec_json = identity_spec.to_json()?;
    let mut base_hasher = qx_core::Fnv1a::new();
    base_hasher.write_text(&spec_json);
    let base_fingerprint = base_hasher.finish();
    let mut identities: Vec<qx_app::StrategyArtifactIdentity> = serde_json::from_value(
        artifact
            .get("strategy_identities")
            .cloned()
            .ok_or_else(|| invalid_report("多市场回测产物缺少策略身份列表"))?,
    )
    .map_err(|error| invalid_report(format!("多市场策略身份列表无效: {error}")))?;
    identities.sort_by(|left, right| left.strategy_id.cmp(&right.strategy_id));
    if identities.is_empty() {
        return Ok(format!("{base_fingerprint:016x}"));
    }
    let mut hasher = qx_core::Fnv1a::new();
    hasher.write_u64(base_fingerprint);
    for identity in identities {
        hasher.write_text(&identity.strategy_id);
        hasher.write_text(&identity.strategy_version);
        hasher.write_text(&identity.source_sha256);
    }
    Ok(format!("{:016x}", hasher.finish()))
}

fn barframe_view(frame: &qx_datastruct::BarFrame) -> BarView {
    let total = frame.ts.len() as u64;
    let stride = total
        .div_ceil(MAX_BAR_POINTS.saturating_sub(1).max(1))
        .max(1) as usize;
    let rows = (0..frame.ts.len())
        .step_by(stride)
        .chain((!frame.ts.is_empty()).then_some(frame.ts.len() - 1))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|index| qx_app::artifact_store::VerifiedBarFramePoint {
            ts: frame.ts[index],
            open_raw: frame.open_raw[index].to_string(),
            high_raw: frame.high_raw[index].to_string(),
            low_raw: frame.low_raw[index].to_string(),
            close_raw: frame.close_raw[index].to_string(),
            volume_raw: frame.volume_raw[index].to_string(),
        })
        .collect();
    BarView {
        source_status: "verified",
        total,
        truncated: total > MAX_BAR_POINTS,
        rows,
    }
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
            .ok_or_else(|| invalid_report("权益曲线包含无效金额"))?;
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

fn sampled_equity_rows(rows: &[Value]) -> (bool, Vec<Value>) {
    let total = rows.len() as u64;
    let stride = total
        .div_ceil(MAX_EQUITY_POINTS.saturating_sub(1).max(1))
        .max(1) as usize;
    let selected = (0..rows.len())
        .step_by(stride)
        .chain((!rows.is_empty()).then_some(rows.len() - 1))
        .collect::<std::collections::BTreeSet<_>>();
    let sampled = selected
        .into_iter()
        .map(|index| {
            let row = &rows[index];
            serde_json::json!({
                "ts": row.get(0).and_then(raw_i128).unwrap_or_default().to_string(),
                "equity_raw": row.get(1).and_then(raw_i128).unwrap_or_default().to_string()
            })
        })
        .collect();
    (total > MAX_EQUITY_POINTS, sampled)
}

fn raw_bars_view(
    summary: &Value,
    data_root: &Path,
    artifact_root: &Path,
) -> Result<BarView, AppError> {
    let Some(input) = summary.get("input") else {
        return Ok(empty_bars("input_not_declared"));
    };
    if input.get("kind").and_then(Value::as_str) != Some("barframe") {
        return Ok(empty_bars("unsupported_input_kind"));
    }
    if input.get("path").and_then(Value::as_str).is_none() {
        return Ok(empty_bars("input_path_missing"));
    }
    ensure_declared_artifact_paths(summary, artifact_root)?;
    let preview = qx_app::artifact_store::verified_barframe_preview(
        summary,
        data_root,
        MAX_BAR_POINTS as usize,
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("原始 Bar 输入身份复核失败: {error}"),
        )
    })?;
    let Some(preview) = preview else {
        return Ok(empty_bars("source_outside_data_root"));
    };
    Ok(BarView {
        source_status: "verified",
        total: preview.total,
        truncated: preview.truncated,
        rows: preview.rows,
    })
}

fn empty_bars(source_status: &'static str) -> BarView {
    BarView {
        source_status,
        total: 0,
        truncated: false,
        rows: Vec::new(),
    }
}

fn raw_depth_view(
    summary: &Value,
    data_root: &Path,
    artifact_root: &Path,
) -> Result<DepthView, AppError> {
    let Some(input) = summary.get("input") else {
        return Ok(empty_depth("input_not_declared"));
    };
    if input.get("kind").and_then(Value::as_str) != Some("depth-frame") {
        return Ok(empty_depth("unsupported_input_kind"));
    }
    if input.get("path").and_then(Value::as_str).is_none() {
        return Ok(empty_depth("input_path_missing"));
    }
    ensure_declared_artifact_paths(summary, artifact_root)?;
    let preview = qx_app::artifact_store::verified_depthframe_preview(
        summary,
        data_root,
        MAX_BAR_POINTS as usize,
        20,
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("原始深度输入身份复核失败: {error}"),
        )
    })?;
    let Some(preview) = preview else {
        return Ok(empty_depth("source_outside_data_root"));
    };
    Ok(DepthView {
        source_status: "verified",
        total: preview.total,
        truncated: preview.truncated,
        rows: preview.rows,
    })
}

fn ensure_declared_artifact_paths(summary: &Value, artifact_root: &Path) -> Result<(), AppError> {
    for field in ["run_manifest", "run_record"] {
        let Some(path) = summary.get(field).and_then(Value::as_str) else {
            continue;
        };
        rooted_file(path, artifact_root).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("摘要声明的 {field} 必须位于服务配置的产物根目录内: {error}"),
            )
        })?;
    }
    Ok(())
}

fn verify_declared_run_artifacts(summary: &Value, artifact_root: &Path) -> Result<(), AppError> {
    ensure_declared_artifact_paths(summary, artifact_root)?;
    qx_app::artifact_store::verify_declared_run_manifest(summary).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("研究报告 RunManifest 身份复核失败: {error}"),
        )
    })?;
    qx_app::artifact_store::verify_declared_run_record(summary).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("研究报告 RunRecord 工件摘要复核失败: {error}"),
        )
    })
}

fn empty_depth(source_status: &'static str) -> DepthView {
    DepthView {
        source_status,
        total: 0,
        truncated: false,
        rows: Vec::new(),
    }
}

fn depth_fill_view(path: &Path) -> Result<CsvView, AppError> {
    let (headers, total) = csv_shape(path)?;
    for field in ["ts", "order_id", "qty_raw", "price_raw", "fee_raw"] {
        column(&headers, &[field])?;
    }
    let file = File::open(path).map_err(|error| {
        AppError::from_io(&format!("读取深度成交产物 {}", path.display()), &error)
    })?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| AppError::from_io("读取深度成交 CSV 表头", &error))?;
    let required = ["ts", "order_id", "qty_raw", "price_raw", "fee_raw"]
        .iter()
        .map(|field| column(&headers, &[*field]))
        .collect::<Result<Vec<_>, _>>()?;
    let mut rows = Vec::with_capacity(MAX_FILL_ROWS.min(total as usize));
    let mut seen = 0_u64;
    loop {
        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .map_err(|error| AppError::from_io("读取深度成交 CSV 数据", &error))?;
        if bytes == 0 {
            break;
        }
        check_line_size(bytes)?;
        if line.trim().is_empty() {
            continue;
        }
        let values = csv_record(line.trim_end())?;
        if headers.len() != values.len() {
            return Err(malformed_csv("深度成交 CSV 行列数与表头不一致"));
        }
        for index in &required {
            values[*index]
                .parse::<i128>()
                .map_err(|_| malformed_csv("深度成交 CSV 原始数值不是整数"))?;
        }
        if rows.len() < MAX_FILL_ROWS {
            rows.push(csv_object(&headers, values)?);
        }
        seen += 1;
    }
    if seen != total {
        return Err(malformed_csv("深度成交 CSV 行数在读取期间发生变化"));
    }
    Ok(CsvView {
        total,
        truncated: rows.len() < total as usize,
        rows,
    })
}

fn attach_bar_event_context(
    events: &mut EventView,
    summary: &Value,
    data_root: &Path,
) -> Result<(), AppError> {
    let event_times = events
        .rows
        .iter()
        .map(|row| {
            let ts = row
                .get("ts")
                .and_then(Value::as_u64)
                .ok_or_else(|| malformed_csv("回放事件缺少有效 ts，无法构建因果时间线"))?;
            let priority = row
                .get("prio")
                .and_then(Value::as_u64)
                .and_then(|value| u8::try_from(value).ok())
                .ok_or_else(|| malformed_csv("回放事件缺少有效优先级，无法构建因果时间线"))?;
            // Bar decisions happen at Bar(t), but the strategy adapter only receives
            // completed history through Bar(t-1). Associate command facts with that
            // strict prefix so the report cannot imply visibility of Bar(t).
            let causal_priority = if priority == qx_core::Priority::COMMAND {
                qx_core::Priority::FEEDBACK
            } else {
                priority
            };
            Ok((ts, causal_priority))
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let contexts = qx_app::artifact_store::verified_barframe_event_contexts(
        summary,
        data_root,
        &event_times,
        MAX_EVENT_ROWS,
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("研究事件与 Bar 的因果时间关联失败: {error}"),
        )
    })?;
    match contexts {
        Some(contexts) => {
            for (row, context) in events.rows.iter_mut().zip(contexts) {
                match context {
                    Some(context) => {
                        row["bar_context"] = serde_json::to_value(context).map_err(|error| {
                            AppError::new(
                                AppErrorCategory::InternalInvariant,
                                format!("因果时间线行情上下文序列化失败: {error}"),
                            )
                        })?;
                        row["bar_context_status"] = Value::String("verified".into());
                    }
                    None => {
                        row["bar_context"] = Value::Null;
                        row["bar_context_status"] =
                            Value::String("before_first_visible_bar".into());
                    }
                }
            }
        }
        None => {
            for row in &mut events.rows {
                row["bar_context"] = Value::Null;
                row["bar_context_status"] = Value::String("input_unavailable".into());
            }
        }
    }
    for row in &mut events.rows {
        attach_research_fact_projection(row);
    }
    Ok(())
}

fn attach_research_fact_projection(row: &mut Value) {
    let Some(kind) = row.get("kind").and_then(Value::as_object) else {
        return;
    };
    if let Some(decision) = kind
        .get("StrategyDecision")
        .and_then(|event| event.get("decision"))
    {
        row["research_fact"] = serde_json::json!({
            "kind": "strategy_decision",
            "strategy_id": decision.get("strategy_id").cloned().unwrap_or(Value::Null),
            "signal_id": decision.get("signal_id").map(|value| value.to_string()).unwrap_or_default(),
            "confidence": decision.get("confidence").map(|value| value.to_string()).unwrap_or_default(),
            "priority": decision.get("priority").map(|value| value.to_string()).unwrap_or_default(),
            "expires_at": decision.get("expires_at").map(|value| value.to_string()).unwrap_or_default(),
            "intent_count": decision.get("intents").and_then(Value::as_array).map_or(0, Vec::len),
        });
    } else if let Some(decision) = kind
        .get("OrderRiskDecision")
        .and_then(|event| event.get("decision"))
    {
        let raw = |field: &str| {
            decision
                .get(field)
                .map(|value| value.to_string())
                .unwrap_or_default()
        };
        row["research_fact"] = serde_json::json!({
            "kind": "risk_decision",
            "client_order_id": decision.get("client_order_id").map(|value| value.to_string()).unwrap_or_default(),
            "instrument": decision.get("instrument").cloned().unwrap_or(Value::Null),
            "allowed": decision.get("allowed").and_then(Value::as_bool).unwrap_or(false),
            "rule_set_version": decision.get("rule_set_version").cloned().unwrap_or(Value::Null),
            "violations": decision.get("violations").cloned().unwrap_or_else(|| Value::Array(Vec::new())),
            "projected_position_raw": raw("projected_position_raw"),
            "projected_margin_raw": raw("projected_margin_raw"),
            "reference_price_raw": raw("reference_price_raw"),
        });
    }
}

/// Resolve the market represented by one event in a shared-account portfolio.
/// Events without a direct instrument can still be linked through the strategy
/// identity persisted in the event metadata/spec.
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
    if let Some(instrument) = instrument {
        if let Some(instrument) = canonical_instrument(instrument) {
            return Some(instrument);
        }
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

/// Return the market Bar visible to an event under the engine's causal ordering.
/// Strategy commands deliberately use a strict prefix: the bar that triggered the
/// strategy is not part of its completed-history input.
fn multi_market_bar_context(
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

type ParsedResearchRequest<'a> = (
    &'a Value,
    Option<&'a Value>,
    Option<&'a Value>,
    Option<EventPage>,
    Option<BTreeMap<String, u64>>,
    Option<String>,
);

fn parse_request(request: &Value) -> Result<ParsedResearchRequest<'_>, AppError> {
    let Some(object) = request.as_object() else {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "研究报告请求必须是 JSON 对象",
        ));
    };
    if let Some(outcome) = object.get("outcome") {
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "outcome"
                    | "strategy_diagnostics"
                    | "market_strategy_diagnostics"
                    | "account_strategy_diagnostics"
                    | "event_page"
                    | "account_event_offsets"
                    | "event_instrument"
            )
        }) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "研究报告请求包含未知字段",
            ));
        }
        let strategy_diagnostics = object
            .get("strategy_diagnostics")
            .filter(|value| !value.is_null());
        let market_strategy_diagnostics = object
            .get("market_strategy_diagnostics")
            .filter(|value| !value.is_null());
        let account_strategy_diagnostics = object
            .get("account_strategy_diagnostics")
            .filter(|value| !value.is_null());
        if account_strategy_diagnostics.is_some()
            && (strategy_diagnostics.is_some() || market_strategy_diagnostics.is_some())
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "account_strategy_diagnostics 不能与顶层策略诊断字段同时提供",
            ));
        }
        if strategy_diagnostics.is_some() && market_strategy_diagnostics.is_some() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "strategy_diagnostics 与 market_strategy_diagnostics 不能同时提供",
            ));
        }
        if let Some(report) = strategy_diagnostics {
            validate_diagnostic_payload(report, "strategy_diagnostics")?;
        }
        if let Some(reports) = market_strategy_diagnostics {
            let count = reports.as_array().map(Vec::len).ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    "market_strategy_diagnostics 必须是 JSON 数组",
                )
            })?;
            if count == 0 || count > MAX_MULTI_MARKET_DIAGNOSTICS {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!(
                        "market_strategy_diagnostics 必须包含 1..={MAX_MULTI_MARKET_DIAGNOSTICS} 份报告"
                    ),
                ));
            }
            validate_diagnostic_payload(reports, "market_strategy_diagnostics")?;
        }
        if let Some(reports) = account_strategy_diagnostics {
            validate_account_strategy_diagnostics(reports)?;
        }
        let event_page = object.get("event_page").map(parse_event_page).transpose()?;
        let account_event_offsets = object
            .get("account_event_offsets")
            .map(parse_account_event_offsets)
            .transpose()?;
        let event_instrument = object
            .get("event_instrument")
            .map(parse_event_instrument)
            .transpose()?;
        Ok((
            outcome,
            strategy_diagnostics,
            market_strategy_diagnostics,
            event_page,
            account_event_offsets,
            event_instrument,
        ))
    } else {
        if request
            .as_object()
            .is_some_and(|object| object.contains_key("account_strategy_diagnostics"))
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "account_strategy_diagnostics 必须与 outcome 一起通过请求包装对象提供",
            ));
        }
        Ok((request, None, None, None, None, None))
    }
}

fn validate_account_strategy_diagnostics(value: &Value) -> Result<(), AppError> {
    let entries = value.as_array().ok_or_else(|| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            "account_strategy_diagnostics 必须是 JSON 数组",
        )
    })?;
    if entries.is_empty() || entries.len() > MAX_MULTI_ACCOUNT_DIAGNOSTICS {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!(
                "account_strategy_diagnostics 必须包含 1..={MAX_MULTI_ACCOUNT_DIAGNOSTICS} 个账户条目"
            ),
        ));
    }
    let mut account_ids = std::collections::BTreeSet::new();
    for entry in entries {
        let object = entry.as_object().ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                "account_strategy_diagnostics 每项必须是 JSON 对象",
            )
        })?;
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "account_id" | "strategy_diagnostics" | "market_strategy_diagnostics"
            )
        }) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "账户策略诊断条目包含未知字段",
            ));
        }
        let account_id = object
            .get("account_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 256)
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    "账户策略诊断条目缺少有效 account_id",
                )
            })?;
        if !account_ids.insert(account_id) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "account_strategy_diagnostics 含重复 account_id",
            ));
        }
        let strategy = object
            .get("strategy_diagnostics")
            .filter(|value| !value.is_null());
        let market = object
            .get("market_strategy_diagnostics")
            .filter(|value| !value.is_null());
        if strategy.is_some() == market.is_some() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "每个账户须且只能提供 strategy_diagnostics 或 market_strategy_diagnostics",
            ));
        }
        if let Some(report) = strategy {
            validate_diagnostic_payload(report, "account.strategy_diagnostics")?;
        }
        if let Some(reports) = market {
            let count = reports.as_array().map(Vec::len).ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    "账户 market_strategy_diagnostics 必须是 JSON 数组",
                )
            })?;
            if count == 0 || count > MAX_MULTI_MARKET_DIAGNOSTICS {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!(
                        "账户 market_strategy_diagnostics 必须包含 1..={MAX_MULTI_MARKET_DIAGNOSTICS} 份报告"
                    ),
                ));
            }
            validate_diagnostic_payload(reports, "account.market_strategy_diagnostics")?;
        }
    }
    validate_diagnostic_payload(value, "account_strategy_diagnostics")
}

fn validate_diagnostic_payload(value: &Value, field: &str) -> Result<(), AppError> {
    let encoded = serde_json::to_vec(value).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("{field} 无法编码: {error}"),
        )
    })?;
    if encoded.len() > MAX_DIAGNOSTIC_BYTES {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!("{field} 超过 1 MiB 上限"),
        ));
    }
    Ok(())
}

fn bind_multi_market_diagnostic(
    report: &Value,
    spec: &MultiMarketBacktestSpec,
    markets: &[MarketResearchView],
) -> Result<BoundStrategyDiagnostics, AppError> {
    let strategy_id = report
        .get("strategy_id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_report("多市场策略诊断缺少 strategy_id"))?;
    let instrument = report
        .get("instrument")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_report("多市场策略诊断缺少 instrument"))?;
    let declared_strategy = spec.markets.iter().any(|market| {
        market.instrument == instrument
            && market
                .strategies
                .iter()
                .any(|member| member.strategy.strategy_id == strategy_id)
    });
    let market_view = markets
        .iter()
        .find(|market| market.instrument == instrument)
        .ok_or_else(|| invalid_report("策略诊断标的不存在于已复核多市场输入"))?;
    if !declared_strategy {
        return Err(invalid_report(
            "策略诊断 strategy_id 不属于声明的标的策略槽",
        ));
    }
    let identity = json!({
        "strategy_id": strategy_id,
        "instrument": instrument,
        "data_fingerprint": market_view.data_fingerprint,
    });
    bind_strategy_diagnostics(report, &identity)
}

fn bind_multi_market_diagnostics(
    reports: &Value,
    spec: &MultiMarketBacktestSpec,
    markets: &[MarketResearchView],
) -> Result<Vec<BoundStrategyDiagnostics>, AppError> {
    let reports = reports
        .as_array()
        .filter(|reports| !reports.is_empty() && reports.len() <= MAX_MULTI_MARKET_DIAGNOSTICS)
        .ok_or_else(|| {
            invalid_report(format!(
                "market_strategy_diagnostics 必须包含 1..={MAX_MULTI_MARKET_DIAGNOSTICS} 份报告"
            ))
        })?;
    let encoded = serde_json::to_vec(reports)
        .map_err(|error| invalid_report(format!("多市场策略诊断报告无法编码: {error}")))?;
    if encoded.len() > MAX_DIAGNOSTIC_BYTES {
        return Err(invalid_report("多市场策略诊断集合超过 1 MiB 上限"));
    }
    let mut identities = std::collections::BTreeSet::new();
    reports
        .iter()
        .map(|report| {
            let strategy_id = report
                .get("strategy_id")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_report("多市场策略诊断缺少 strategy_id"))?;
            let instrument = report
                .get("instrument")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_report("多市场策略诊断缺少 instrument"))?;
            if !identities.insert((instrument, strategy_id)) {
                return Err(invalid_report("多市场策略诊断集合包含重复策略/标的身份"));
            }
            bind_multi_market_diagnostic(report, spec, markets)
        })
        .collect()
}

fn bind_multi_market_depth_diagnostics(
    reports: &Value,
    spec: &MultiMarketDepthBacktestSpec,
    markets: &[MarketResearchView],
    custom_strategies: &[qx_app::StrategyArtifactIdentity],
) -> Result<Vec<BoundStrategyDiagnostics>, AppError> {
    let reports = reports
        .as_array()
        .filter(|reports| !reports.is_empty() && reports.len() <= MAX_MULTI_MARKET_DIAGNOSTICS)
        .ok_or_else(|| {
            invalid_report(format!(
                "market_strategy_diagnostics 必须包含 1..={MAX_MULTI_MARKET_DIAGNOSTICS} 份报告"
            ))
        })?;
    let encoded = serde_json::to_vec(reports)
        .map_err(|error| invalid_report(format!("多市场深度策略诊断报告无法编码: {error}")))?;
    if encoded.len() > MAX_DIAGNOSTIC_BYTES {
        return Err(invalid_report("多市场深度策略诊断集合超过 1 MiB 上限"));
    }
    let mut identities = std::collections::BTreeSet::new();
    reports
        .iter()
        .map(|report| {
            let strategy_id = report
                .get("strategy_id")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_report("多市场深度策略诊断缺少 strategy_id"))?;
            let instrument = report
                .get("instrument")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_report("多市场深度策略诊断缺少 instrument"))?;
            if !identities.insert((instrument, strategy_id)) {
                return Err(invalid_report(
                    "多市场深度策略诊断集合包含重复策略/标的身份",
                ));
            }
            let configured = spec
                .markets
                .iter()
                .find(|market| market.instrument == instrument)
                .ok_or_else(|| invalid_report("深度策略诊断标的不存在于已复核输入"))?;
            if configured.strategy.strategy_id != strategy_id {
                return Err(invalid_report(
                    "深度策略诊断 strategy_id 与该标的策略槽不一致",
                ));
            }
            let market_view = markets
                .iter()
                .find(|market| market.instrument == instrument)
                .ok_or_else(|| invalid_report("深度策略诊断标的没有对应的复核市场"))?;
            let is_callback = custom_strategies
                .iter()
                .any(|identity| identity.strategy_id == strategy_id);
            let expected_input_kind = if !is_callback {
                "depth_midpoint_bar"
            } else if configured.data_tier.as_deref().unwrap_or("l2") == "l1" {
                "tick"
            } else {
                "order_book"
            };
            let input_events = report.get("input_events").and_then(Value::as_u64);
            if report.get("input_kind").and_then(Value::as_str) != Some(expected_input_kind)
                || input_events != market_view.depth.as_ref().map(|depth| depth.total)
                || report.get("as_of").and_then(Value::as_u64) != market_view.source_end_ts
                || report.get("input_bars").is_some()
            {
                return Err(invalid_report(
                    "深度策略诊断输入类型或事件数与已复核 DepthFrame 不一致",
                ));
            }
            let expected_identity = json!({
                "strategy_id": strategy_id,
                "instrument": instrument,
                "data_fingerprint": market_view.data_fingerprint,
            });
            bind_strategy_diagnostics(report, &expected_identity)
        })
        .collect()
}

fn parse_event_instrument(value: &Value) -> Result<String, AppError> {
    let instrument = value
        .as_str()
        .map(str::trim)
        .filter(|instrument| !instrument.is_empty() && instrument.len() <= 256)
        .filter(|instrument| !instrument.chars().any(char::is_control))
        .ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                "event_instrument 必须是 1..=256 字节的可见标的身份",
            )
        })?;
    Ok(instrument.to_owned())
}

fn parse_account_event_offsets(value: &Value) -> Result<BTreeMap<String, u64>, AppError> {
    let Some(object) = value.as_object() else {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "account_event_offsets 必须是账户 ID 到非负整数的 JSON 对象",
        ));
    };
    if object.len() > MAX_MULTI_ACCOUNTS {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!("account_event_offsets 最多支持 {MAX_MULTI_ACCOUNTS} 个账户"),
        ));
    }
    object
        .iter()
        .map(|(account_id, offset)| {
            let offset = offset.as_u64().ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    "account_event_offsets 的游标必须是非负整数",
                )
            })?;
            if account_id.is_empty() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "account_event_offsets 不能包含空 account_id",
                ));
            }
            Ok((account_id.clone(), offset))
        })
        .collect()
}

fn compare_merged_events(left: &Value, right: &Value) -> std::cmp::Ordering {
    let number =
        |event: &Value, key: &str| event.get(key).and_then(Value::as_u64).unwrap_or(u64::MAX);
    number(left, "ts")
        .cmp(&number(right, "ts"))
        .then_with(|| number(left, "prio").cmp(&number(right, "prio")))
        .then_with(|| {
            left.get("account_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .cmp(
                    right
                        .get("account_id")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                )
        })
        .then_with(|| number(left, "seq").cmp(&number(right, "seq")))
}

fn parse_event_page(value: &Value) -> Result<EventPage, AppError> {
    let Some(object) = value.as_object() else {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "研究事件分页必须是 JSON 对象",
        ));
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "offset" | "limit"))
    {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "研究事件分页包含未知字段",
        ));
    }
    let offset = object
        .get("offset")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                "研究事件分页 offset 必须是非负整数",
            )
        })?;
    let limit = match object.get("limit") {
        Some(value) => value.as_u64().ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                "研究事件分页 limit 必须是正整数",
            )
        })?,
        None => MAX_EVENT_ROWS as u64,
    };
    if limit == 0 || limit > MAX_EVENT_ROWS as u64 {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!("研究事件分页 limit 必须在 1 到 {MAX_EVENT_ROWS} 之间"),
        ));
    }
    Ok(EventPage {
        offset,
        limit: limit as usize,
    })
}

fn bind_strategy_diagnostics(
    report: &Value,
    summary: &Value,
) -> Result<BoundStrategyDiagnostics, AppError> {
    let encoded = serde_json::to_vec(report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("策略诊断报告无法编码: {error}"),
        )
    })?;
    if encoded.len() > MAX_DIAGNOSTIC_BYTES {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            "策略诊断报告超过 1 MiB 上限",
        ));
    }
    let matches = |key: &str| {
        report.get(key).and_then(Value::as_str) == summary.get(key).and_then(Value::as_str)
    };
    let input_bars = report
        .get("input_bars")
        .and_then(Value::as_u64)
        .filter(|count| *count > 0);
    let input_events = report
        .get("input_events")
        .and_then(Value::as_u64)
        .filter(|count| *count > 0);
    let valid_input_coverage = match (input_bars, input_events) {
        (Some(_), None) => report.get("input_kind").is_none_or(|kind| kind == "bar"),
        (None, Some(_)) => matches!(
            report.get("input_kind").and_then(Value::as_str),
            Some("tick" | "order_book" | "depth_midpoint_bar")
        ),
        _ => false,
    };
    if report.get("schema_version").and_then(Value::as_u64) != Some(1)
        || report.get("kind").and_then(Value::as_str) != Some("qianxing-strategy-bias-diagnostics")
        || !matches("strategy_id")
        || !matches("instrument")
        || !matches("data_fingerprint")
        || report.get("as_of").and_then(Value::as_u64).is_none()
        || !valid_input_coverage
        || report.get("lookahead").and_then(Value::as_object).is_none()
        || report.get("startup").and_then(Value::as_object).is_none()
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "策略诊断身份/结构与已复核回测不匹配",
        ));
    }
    Ok(BoundStrategyDiagnostics {
        identity_bound: true,
        computation: "client_computed_not_reexecuted_by_api",
        report: report.clone(),
    })
}

fn empty_view() -> CsvView {
    CsvView {
        total: 0,
        truncated: false,
        rows: Vec::new(),
    }
}

fn format_raw(raw: i128) -> String {
    let scale = qx_core::SCALE;
    let whole = raw / scale;
    let fraction = raw.unsigned_abs() % scale as u128;
    if fraction == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{fraction:08}")
            .trim_end_matches('0')
            .to_string()
    }
}

fn multi_leg_funding_cost(
    ledger: &qx_core::Ledger,
    account_id: &str,
    instrument: &str,
) -> Result<i128, AppError> {
    let instrument_id = qx_core::InstrumentId::parse(instrument)
        .ok_or_else(|| invalid_report(format!("多腿标的 ID 无效: {instrument}")))?;
    let cashflow = ledger
        .entries()
        .iter()
        .filter(|entry| {
            entry.account_id == account_id
                && entry.kind == qx_core::LedgerEntryKind::Funding
                && entry.instrument.as_ref() == Some(&instrument_id)
        })
        .try_fold(0_i128, |sum, entry| sum.checked_add(entry.amount.raw()))
        .ok_or_else(|| invalid_report("多腿重放资金费累计溢出"))?;
    cashflow
        .checked_neg()
        .ok_or_else(|| invalid_report("多腿重放资金费符号溢出"))
}

// Keep ledger, execution, funding, and initial-capital inputs explicit for reconciliation.
#[allow(clippy::too_many_arguments)]
fn multi_leg_leg_return_bps(
    ledger: &qx_core::Ledger,
    account_id: &str,
    instrument: &str,
    market_spec: &qx_core::TradingInstrumentSpec,
    frame: &qx_datastruct::BarFrame,
    fills: &[qx_core::Fill],
    funding_cost_raw: i128,
    initial_cash_raw: i128,
) -> Result<i32, AppError> {
    let instrument_id = qx_core::InstrumentId::parse(instrument)
        .ok_or_else(|| invalid_report(format!("多腿标的 ID 无效: {instrument}")))?;
    let position = ledger.position_for(account_id, &instrument_id);
    let unrealized = if position.quantity.raw() == 0 {
        0
    } else {
        let mark = frame
            .close_raw
            .last()
            .copied()
            .ok_or_else(|| invalid_report("多腿最终估值缺少 Bar"))?;
        market_spec
            .unrealized_pnl(position.quantity.raw(), position.average_entry.raw(), mark)
            .map_err(|error| invalid_report(format!("多腿未实现盈亏复算失败: {error:?}")))?
    };
    let fees = fills
        .iter()
        .try_fold(0_i128, |sum, fill| sum.checked_add(fill.fee.raw()))
        .ok_or_else(|| invalid_report("多腿逐腿费用累计溢出"))?;
    if initial_cash_raw <= 0 {
        return Err(invalid_report("多腿逐腿收益分母必须为正"));
    }
    position
        .realized_pnl
        .raw()
        .checked_add(unrealized)
        .and_then(|value| value.checked_sub(fees))
        .and_then(|value| value.checked_sub(funding_cost_raw))
        .and_then(|value| value.checked_mul(10_000))
        .and_then(|value| value.checked_div(initial_cash_raw))
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid_report("多腿逐腿收益无法复算"))
}

fn empty_event_view() -> EventView {
    EventView {
        total: 0,
        offset: 0,
        truncated: false,
        rows: Vec::new(),
    }
}

fn rooted_file(path: &str, root: &Path) -> Result<String, AppError> {
    let path = PathBuf::from(path);
    let candidate = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| AppError::from_io("解析 API 工作目录", &error))?
            .join(path)
    };
    let canonical = std::fs::canonicalize(&candidate).map_err(|error| {
        AppError::from_io(&format!("解析研究报告产物 {}", candidate.display()), &error)
    })?;
    if !canonical.starts_with(root) {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "研究报告产物必须位于服务配置的产物根目录内",
        ));
    }
    if !canonical.is_file() {
        return Err(AppError::new(
            AppErrorCategory::DataUnavailable,
            "研究报告产物不是普通文件",
        ));
    }
    Ok(canonical.to_string_lossy().into_owned())
}

fn check_artifact_size(path: &Path) -> Result<(), AppError> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| AppError::from_io(&format!("读取研究产物 {}", path.display()), &error))?;
    if metadata.len() > MAX_ARTIFACT_BYTES {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            format!(
                "研究报告产物超过明细接口上限 {} MiB",
                MAX_ARTIFACT_BYTES / 1024 / 1024
            ),
        ));
    }
    Ok(())
}

fn read_json(path: &Path) -> Result<Value, AppError> {
    let body = std::fs::read_to_string(path)
        .map_err(|error| AppError::from_io(&format!("读取研究摘要 {}", path.display()), &error))?;
    serde_json::from_str(&body).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("研究摘要 {} 格式损坏: {error}", path.display()),
        )
    })
}

fn equity_view(path: &Path) -> Result<CsvView, AppError> {
    let (headers, total) = csv_shape(path)?;
    let timestamp = column(&headers, &["ts", "timestamp", "index"])?;
    let equity = column(&headers, &["equity_raw"])?;
    let position = headers.iter().position(|header| header == "position_raw");
    let stride = total
        .div_ceil(MAX_EQUITY_POINTS.saturating_sub(1).max(1))
        .max(1);
    let file = File::open(path)
        .map_err(|error| AppError::from_io(&format!("读取权益曲线 {}", path.display()), &error))?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| AppError::from_io("读取权益曲线表头", &error))?;
    let mut rows = Vec::with_capacity(MAX_EQUITY_POINTS as usize);
    let mut index = 0u64;
    loop {
        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .map_err(|error| AppError::from_io("读取权益曲线数据", &error))?;
        if bytes == 0 {
            break;
        }
        check_line_size(bytes)?;
        if line.trim().is_empty() {
            continue;
        }
        if index.is_multiple_of(stride) || index + 1 == total {
            let values = csv_record(line.trim_end())?;
            let ts = values
                .get(timestamp)
                .ok_or_else(|| malformed_csv("权益曲线缺少时间列"))?;
            let equity_raw = values
                .get(equity)
                .ok_or_else(|| malformed_csv("权益曲线缺少权益列"))?;
            equity_raw
                .parse::<i128>()
                .map_err(|_| malformed_csv("权益曲线 equity_raw 不是整数"))?;
            let mut row = serde_json::json!({"ts": ts, "equity_raw": equity_raw});
            if let Some(position_index) = position {
                let position_raw = values
                    .get(position_index)
                    .ok_or_else(|| malformed_csv("权益曲线缺少 position_raw"))?;
                position_raw
                    .parse::<i128>()
                    .map_err(|_| malformed_csv("权益曲线 position_raw 不是整数"))?;
                row["position_raw"] = Value::String(position_raw.to_string());
            }
            rows.push(row);
        }
        index += 1;
    }
    Ok(CsvView {
        total,
        truncated: rows.len() < total as usize,
        rows,
    })
}

fn event_view(
    path: &Path,
    summary: &Value,
    manifest: &Value,
    requested_page: Option<EventPage>,
) -> Result<(EventView, BTreeMap<FillKey, usize>), AppError> {
    let file = File::open(path)
        .map_err(|error| AppError::from_io(&format!("读取事件轨迹 {}", path.display()), &error))?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut typed_events = Vec::new();
    let mut total = 0u64;
    loop {
        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .map_err(|error| AppError::from_io("读取事件轨迹", &error))?;
        if bytes == 0 {
            break;
        }
        check_line_size(bytes)?;
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
        typed_events.push(event);
    }
    let (events, ledger) =
        qx_core::ReplayVerifier::replay_owned(typed_events).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("事件轨迹重放失败: {error}"),
            )
        })?;
    let expected_digest = summary
        .pointer("/replay/log_digest")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed_csv("摘要缺少 replay.log_digest，无法验证事件轨迹"))?;
    let expected_digest = u64::from_str_radix(expected_digest, 16)
        .map_err(|_| malformed_csv("摘要 replay.log_digest 不是十六进制摘要"))?;
    if events.digest() != expected_digest {
        return Err(malformed_csv(
            "事件轨迹摘要与 summary.replay.log_digest 不一致",
        ));
    }
    let expected_manifest_digest = format!("{:016x}", events.digest());
    if manifest
        .pointer("/output_event_hash")
        .and_then(Value::as_str)
        != Some(expected_manifest_digest.as_str())
    {
        return Err(malformed_csv(
            "RunManifest.output_event_hash 与 replay-verified 事件轨迹不一致",
        ));
    }
    let expected_count = summary
        .pointer("/replay/events")
        .and_then(Value::as_u64)
        .ok_or_else(|| malformed_csv("摘要缺少 replay.events，无法验证事件数量"))?;
    if total != expected_count {
        return Err(malformed_csv("事件轨迹行数与 summary.replay.events 不一致"));
    }
    let replayed_ledger_count = ledger.entries().len() as u64;
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
            return Err(malformed_csv(&format!(
                "summary{path} 与重放重建账簿条数不一致"
            )));
        }
    }
    let all_events = events.events();
    let mut event_fills = BTreeMap::new();
    for event in all_events {
        if let qx_core::EventKind::Filled { fill } = &event.kind {
            *event_fills.entry(fill_key(fill)).or_insert(0) += 1;
        }
    }
    let page = requested_page.unwrap_or(EventPage {
        offset: total.saturating_sub(MAX_EVENT_ROWS as u64),
        limit: MAX_EVENT_ROWS,
    });
    let offset = page.offset.min(total);
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(all_events.len());
    let end = start.saturating_add(page.limit).min(all_events.len());
    let rows = all_events
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("事件轨迹视图序列化失败: {error}"),
            )
        })?
        .into_iter()
        .collect::<VecDeque<_>>();
    Ok((
        EventView {
            total,
            offset,
            truncated: rows.len() < total as usize,
            rows: rows.into_iter().collect(),
        },
        event_fills,
    ))
}

fn fill_key(fill: &qx_core::Fill) -> FillKey {
    FillKey {
        ts: fill.ts,
        order_id: fill.order_id,
        qty_raw: fill.qty.raw(),
        price_raw: fill.price.raw(),
        fee_raw: fill.fee.raw(),
        strategy_id: fill.strategy_id.clone().unwrap_or_default(),
    }
}

fn validated_fill_view(
    path: &Path,
    mut expected: BTreeMap<FillKey, usize>,
) -> Result<CsvView, AppError> {
    let (headers, total) = csv_shape(path)?;
    let columns = [
        column(&headers, &["ts"])?,
        column(&headers, &["order_id"])?,
        column(&headers, &["qty_raw"])?,
        column(&headers, &["price_raw"])?,
        column(&headers, &["fee_raw"])?,
        column(&headers, &["strategy_id"])?,
    ];
    if total as usize != expected.values().sum::<usize>() {
        return Err(malformed_csv(
            "fills.csv 成交数与 replay-verified Filled 事件数不一致",
        ));
    }
    let file = File::open(path)
        .map_err(|error| AppError::from_io(&format!("读取成交产物 {}", path.display()), &error))?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| AppError::from_io("读取成交 CSV 表头", &error))?;
    let mut rows = Vec::with_capacity(MAX_FILL_ROWS.min(total as usize));
    loop {
        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .map_err(|error| AppError::from_io("读取成交 CSV 数据", &error))?;
        if bytes == 0 {
            break;
        }
        check_line_size(bytes)?;
        if line.trim().is_empty() {
            continue;
        }
        let values = csv_record(line.trim_end())?;
        let field = |position: usize| {
            values
                .get(position)
                .ok_or_else(|| malformed_csv("成交 CSV 行缺少声明字段"))
        };
        let parse_integer = |position: usize| -> Result<i128, AppError> {
            field(position)?
                .parse::<i128>()
                .map_err(|_| malformed_csv("成交 CSV 定点值不是整数"))
        };
        let key = FillKey {
            ts: parse_integer(columns[0])?
                .try_into()
                .map_err(|_| malformed_csv("成交 CSV 时间戳超出范围"))?,
            order_id: parse_integer(columns[1])?
                .try_into()
                .map_err(|_| malformed_csv("成交 CSV 订单 ID 超出范围"))?,
            qty_raw: parse_integer(columns[2])?,
            price_raw: parse_integer(columns[3])?,
            fee_raw: parse_integer(columns[4])?,
            strategy_id: field(columns[5])?.clone(),
        };
        let Some(count) = expected.get_mut(&key) else {
            return Err(malformed_csv(
                "fills.csv 中存在未匹配到 replay-verified Filled 事件的成交",
            ));
        };
        *count -= 1;
        if *count == 0 {
            expected.remove(&key);
        }
        if rows.len() < MAX_FILL_ROWS {
            rows.push(csv_object(&headers, values)?);
        }
    }
    if !expected.is_empty() {
        return Err(malformed_csv(
            "replay-verified Filled 事件存在未写入 fills.csv 的成交",
        ));
    }
    Ok(CsvView {
        total,
        truncated: rows.len() < total as usize,
        rows,
    })
}

fn csv_shape(path: &Path) -> Result<(Vec<String>, u64), AppError> {
    let file = File::open(path)
        .map_err(|error| AppError::from_io(&format!("读取 CSV 产物 {}", path.display()), &error))?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let bytes = reader
        .read_line(&mut line)
        .map_err(|error| AppError::from_io("读取 CSV 表头", &error))?;
    if bytes == 0 {
        return Err(malformed_csv("CSV 产物为空"));
    }
    check_line_size(bytes)?;
    let headers = csv_record(line.trim_end())?;
    if headers.is_empty() || headers.iter().any(|header| header.is_empty()) {
        return Err(malformed_csv("CSV 表头为空或存在空字段"));
    }
    let mut total = 0u64;
    loop {
        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .map_err(|error| AppError::from_io("统计 CSV 数据行", &error))?;
        if bytes == 0 {
            break;
        }
        check_line_size(bytes)?;
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

fn column(headers: &[String], candidates: &[&str]) -> Result<usize, AppError> {
    headers
        .iter()
        .position(|header| candidates.contains(&header.as_str()))
        .ok_or_else(|| malformed_csv("CSV 缺少报告所需字段"))
}

fn csv_object(headers: &[String], values: Vec<String>) -> Result<Value, AppError> {
    if headers.len() != values.len() {
        return Err(malformed_csv("CSV 行列数与表头不一致"));
    }
    Ok(Value::Object(
        headers
            .iter()
            .cloned()
            .zip(values)
            .map(|(header, value)| (header, Value::String(value)))
            .collect(),
    ))
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

fn check_line_size(bytes: usize) -> Result<(), AppError> {
    if bytes > MAX_LINE_BYTES {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            "研究报告单行产物超过 1 MiB 上限",
        ));
    }
    Ok(())
}

fn malformed_csv(message: &str) -> AppError {
    AppError::new(AppErrorCategory::StorageFailure, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_surface::{self, AppPaths};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn ambiguous_strategy_decision_does_not_get_a_single_market_projection() {
        let strategy_markets = BTreeMap::from([("multi".to_owned(), "BTCUSDT.BINANCE".to_owned())]);
        let order_markets = BTreeMap::new();
        let one_market = json!({
            "kind": {"StrategyDecision": {"decision": {
                "strategy_id": "multi",
                "intents": [{"instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"}}]
            }}}
        });
        assert_eq!(
            event_market_instrument(&one_market, &strategy_markets, &order_markets).as_deref(),
            Some("BTCUSDT.BINANCE")
        );

        let two_markets = json!({
            "kind": {"StrategyDecision": {"decision": {
                "strategy_id": "multi",
                "intents": [
                    {"instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"}},
                    {"instrument": {"symbol": "ETHUSDT", "venue": "KRAKEN"}}
                ]
            }}}
        });
        assert_eq!(
            event_market_instrument(&two_markets, &strategy_markets, &order_markets),
            None,
            "a multi-market decision must not be mislabeled with its first intent"
        );
    }

    fn backtest_fixture() -> (AppPaths, crate::ApiResponse) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join(format!(
                "qx-api-research-artifacts-{}-{unique}",
                std::process::id()
            ));
        let data_root = workspace.join("data");
        let artifact_root = workspace.join("artifacts");
        std::fs::create_dir_all(&data_root).expect("create research fixture data root");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy/qianxing.bar-frame.example.json");
        std::fs::copy(fixture, data_root.join("bars.json")).expect("copy BarFrame fixture");
        let paths = AppPaths {
            data_root: data_root.clone(),
            artifact_root,
        };
        let spec = serde_json::json!({
            "schema_version": 1,
            "run_id": "api-research-artifacts",
            "instrument": "BTCUSDT.BINANCE",
            "bars_path": data_root.join("bars.json"),
            "settlement_currency": "USDT",
            "initial_cash_raw": 1_000_000_000_000_000_i128,
            "seed": 17,
            "output_dir": "client-path-is-overridden",
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": "api-research-artifacts",
                "quantity_raw": qx_core::SCALE,
                "fast_window": 2,
                "slow_window": 4
            }
        });
        let response = app_surface::post_run_backtest(&spec.to_string(), &paths);
        (paths, response)
    }

    fn depth_backtest_fixture() -> (AppPaths, crate::ApiResponse) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join(format!(
                "qx-api-research-depth-{}-{unique}",
                std::process::id()
            ));
        let data_root = workspace.join("data");
        let artifact_root = workspace.join("artifacts");
        std::fs::create_dir_all(&data_root).expect("create research depth fixture data root");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy/qianxing.depth-frame.example.json");
        std::fs::copy(fixture, data_root.join("depth.json")).expect("copy DepthFrame fixture");
        let paths = AppPaths {
            data_root: data_root.clone(),
            artifact_root,
        };
        let spec = serde_json::json!({
            "schema_version": 1,
            "run_id": "api-research-depth",
            "depth_path": data_root.join("depth.json"),
            "tier": "l2",
            "settlement_currency": "USDT",
            "initial_cash_raw": 1_000_000_000_000_000_i128,
            "fee_bps": 0,
            "latency_snapshots": 0,
            "queue_position_bps": 0,
            "market_impact_bps": 0,
            "output_dir": "client-path-is-overridden",
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": "api-research-depth",
                "quantity_raw": qx_core::SCALE,
                "fast_window": 2,
                "slow_window": 4
            }
        });
        let response = app_surface::depth_backtest(&spec.to_string(), &paths);
        (paths, response)
    }

    fn multi_market_fixture() -> (AppPaths, crate::ApiResponse) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join(format!(
                "qx-api-research-multi-market-{}-{unique}",
                std::process::id()
            ));
        let data_root = workspace.join("data");
        let artifact_root = workspace.join("artifacts");
        std::fs::create_dir_all(&data_root).expect("create multi-market fixture data root");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy/qianxing.bar-frame.example.json");
        let source = std::fs::read_to_string(fixture).expect("read BarFrame fixture");
        let base = qx_datastruct::BarFrame::from_json(&source).expect("parse BarFrame fixture");
        let instruments = ["BTCUSDT.BINANCE", "ETHUSDT.KRAKEN", "SOLUSDT.BYBIT"];
        let markets = instruments
            .iter()
            .enumerate()
            .map(|(index, instrument)| {
                let mut frame = base.clone();
                frame.instrument = qx_core::InstrumentId::parse(instrument)
                    .expect("fixture market instrument is valid");
                let path = data_root.join(format!("market-{index}.json"));
                std::fs::write(&path, frame.to_json()).expect("write multi-market BarFrame");
                serde_json::json!({
                    "instrument": instrument,
                    "bars_path": path,
                    "priority": index,
                    "market_spec": null,
                    "fee_schedule": null,
                    "strategies": [{
                        "priority": 0,
                        "reserved_capital_raw": 20_000 * qx_core::SCALE,
                        "strategy": {
                            "kind": "sma_cross",
                            "strategy_id": format!("sma-{index}"),
                            "quantity_raw": qx_core::SCALE,
                            "fast_window": 2,
                            "slow_window": 3
                        }
                    }]
                })
            })
            .collect::<Vec<_>>();
        let paths = AppPaths {
            data_root: data_root.clone(),
            artifact_root,
        };
        let spec = serde_json::json!({
            "schema_version": 1,
            "run_id": "api-research-multi-market",
            "account_id": "shared-account",
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000 * qx_core::SCALE,
            "seed": 17,
            "output_dir": "client-path-is-overridden",
            "markets": markets
        });
        let response = app_surface::multi_market(&spec.to_string(), &paths);
        (paths, response)
    }

    fn multi_leg_fixture() -> (AppPaths, crate::ApiResponse) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join(format!(
                "qx-api-research-multi-leg-{}-{unique}",
                std::process::id()
            ));
        let data_root = workspace.join("data");
        let artifact_root = workspace.join("artifacts");
        std::fs::create_dir_all(&data_root).expect("create multi-leg research data root");
        let deploy = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy");
        for (source, target) in [
            (
                "qianxing.bar-frame.pairs-primary.example.json",
                "primary.json",
            ),
            (
                "qianxing.bar-frame.pairs-reference.example.json",
                "reference.json",
            ),
        ] {
            std::fs::copy(deploy.join(source), data_root.join(target))
                .expect("copy multi-leg BarFrame fixture");
        }
        let paths = AppPaths {
            data_root: data_root.clone(),
            artifact_root,
        };
        let spec = json!({
            "schema_version": 1,
            "run_id": "api-research-multi-leg",
            "primary_bars_path": data_root.join("primary.json"),
            "reference_bars_path": data_root.join("reference.json"),
            "settlement_currency": "USDT",
            "primary_initial_cash_raw": 1_000_000_000_000_000_i128,
            "reference_initial_cash_raw": 1_000_000_000_000_000_i128,
            "seed": 17,
            "output_dir": "client-controlled-path-is-overridden",
            "strategy": {
                "kind": "pairs_arbitrage",
                "strategy_id": "api-research-pairs",
                "quantity_raw": qx_core::SCALE,
                "fast_window": 5,
                "slow_window": 20,
                "period": 14,
                "threshold_bps": 100
            }
        });
        let response = app_surface::multi_leg_backtest(&spec.to_string(), &paths);
        (paths, response)
    }

    fn multi_market_depth_fixture() -> (AppPaths, crate::ApiResponse) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join(format!(
                "qx-api-research-multi-market-depth-{}-{unique}",
                std::process::id()
            ));
        let data_root = workspace.join("data");
        let artifact_root = workspace.join("artifacts");
        std::fs::create_dir_all(&data_root).expect("create multi-market depth fixture data root");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy/qianxing.depth-frame.l1.example.json");
        let base: Value =
            serde_json::from_slice(&std::fs::read(fixture).expect("read L1 depth fixture"))
                .expect("parse L1 depth fixture");
        let markets = [
            ("BTCUSDT.BINANCE", "BTC", 65_000_i64),
            ("ETHUSDT.BYBIT", "ETH", 3_000_i64),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (instrument, base_currency, price))| {
            let (symbol, venue) = instrument.split_once('.').expect("instrument venue");
            let mut frame = base.clone();
            frame["source"] = json!(format!("api-multi-market-depth-{index}"));
            frame["instrument"] = json!({"symbol": symbol, "venue": venue});
            let snapshots = frame["snapshots"]
                .as_array_mut()
                .expect("fixture snapshots");
            for snapshot in snapshots {
                snapshot["instrument"] = json!({"symbol": symbol, "venue": venue});
                snapshot["bids"][0]["price"] = json!(price * qx_core::SCALE as i64);
                snapshot["asks"][0]["price"] = json!(price * qx_core::SCALE as i64);
            }
            let path = data_root.join(format!("depth-{index}.json"));
            std::fs::write(
                &path,
                serde_json::to_vec(&frame).expect("encode depth fixture"),
            )
            .expect("write depth fixture");
            let quality_manifest_path = if index == 0 {
                let manifest_path = data_root.join(format!("depth-{index}.manifest.json"));
                let quality_spec = qx_app::DepthDatasetManifestValidationSpec::new(
                    path.to_string_lossy(),
                    qx_data::DatasetTier::L1,
                    1_000,
                    "UTC",
                    manifest_path.to_string_lossy(),
                );
                qx_app::validate_depth_dataset_manifest(
                    &quality_spec,
                    &qx_app::RunContext::new(
                        qx_app::CallerCapability::Research,
                        format!("api-depth-quality-{index}"),
                    ),
                )
                .expect("write verified L1 quality manifest");
                Some(manifest_path)
            } else {
                None
            };
            let mut market = json!({
                "instrument": instrument,
                "depth_path": path,
                "data_tier": "l1",
                "priority": index,
                "market_spec": {
                    "instrument": {"symbol": symbol, "venue": venue},
                    "product": "spot",
                    "base_currency": base_currency,
                    "quote_currency": "USDT",
                    "settlement_currency": "USDT",
                    "contract_size": qx_core::SCALE,
                    "linear": false,
                    "inverse": false,
                    "price_tick": 1,
                    "qty_step": 1,
                    "min_qty": 1,
                    "max_leverage": 1,
                    "maintenance_margin_bps": 0,
                    "valid_from": 1,
                    "valid_to": null
                },
                "fee_bps": 0,
                "latency_snapshots": 0,
                "queue_position_bps": 0,
                "market_impact_bps": 0,
                "strategy": {
                    "kind": "sma_cross",
                    "strategy_id": format!("api-multi-market-depth-{index}"),
                    "quantity_raw": qx_core::SCALE,
                    "fast_window": 2,
                    "slow_window": 3
                }
            });
            if let Some(manifest_path) = quality_manifest_path {
                market["dataset_manifest_path"] = json!(manifest_path);
            }
            market
        })
        .collect::<Vec<_>>();
        let paths = AppPaths {
            data_root: data_root.clone(),
            artifact_root,
        };
        let spec = json!({
            "schema_version": 1,
            "run_id": "api-research-multi-market-depth",
            "account_id": "shared-depth-account",
            "settlement_currency": "USDT",
            "initial_cash_raw": 100 * qx_core::SCALE,
            "output_dir": "client-path-is-overridden",
            "markets": markets
        });
        let response = app_surface::multi_market_depth(&spec.to_string(), &paths);
        (paths, response)
    }

    fn multi_account_fixture() -> (AppPaths, crate::ApiResponse) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join(format!(
                "qx-api-research-multi-account-{}-{unique}",
                std::process::id()
            ));
        let data_root = workspace.join("data");
        let artifact_root = workspace.join("artifacts");
        std::fs::create_dir_all(&data_root).expect("create multi-account fixture data root");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy/qianxing.bar-frame.example.json");
        std::fs::copy(&fixture, data_root.join("bars.json")).expect("copy multi-account BarFrame");
        let mut eth_frame: Value = serde_json::from_slice(
            &std::fs::read(&fixture).expect("read source BarFrame for ETH fixture"),
        )
        .expect("parse source BarFrame for ETH fixture");
        eth_frame["instrument"] = json!("ETHUSDT.BINANCE");
        std::fs::write(
            data_root.join("eth-bars.json"),
            serde_json::to_vec_pretty(&eth_frame).expect("serialize ETH BarFrame"),
        )
        .expect("write ETH BarFrame");
        let paths = AppPaths {
            data_root: data_root.clone(),
            artifact_root,
        };
        let make_run = |run_id: &str, account_id: &str, instrument: &str, bars_path: &Path| {
            json!({
                "schema_version": 1,
                "run_id": run_id,
                "account_id": account_id,
                "instrument": instrument,
                "bars_path": bars_path,
                "settlement_currency": "USDT",
                "initial_cash_raw": 1_000_000_000_000_000_i128,
                "seed": 17,
                "output_dir": "client-path-is-overridden",
                "strategy": {
                    "kind": "sma_cross",
                    "strategy_id": run_id,
                    "quantity_raw": qx_core::SCALE,
                    "fast_window": 2,
                    "slow_window": 4
                }
            })
        };
        let spec = json!({
            "schema_version": 1,
            "batch_id": "api-research-multi-account",
            "runs": [
                make_run("api-research-account-one", "account-one", "BTCUSDT.BINANCE", &data_root.join("bars.json")),
                make_run("api-research-account-two", "account-two", "ETHUSDT.BINANCE", &data_root.join("eth-bars.json"))
            ]
        });
        let response = app_surface::multi_account(&spec.to_string(), &paths);
        (paths, response)
    }

    fn multi_account_depth_fixture() -> (AppPaths, crate::ApiResponse) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join(format!(
                "qx-api-research-multi-account-depth-{}-{unique}",
                std::process::id()
            ));
        let data_root = workspace.join("data");
        let artifact_root = workspace.join("artifacts");
        std::fs::create_dir_all(&data_root).expect("create multi-account depth fixture data root");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy/qianxing.depth-frame.l1.example.json");
        std::fs::copy(fixture, data_root.join("depth.json"))
            .expect("copy multi-account DepthFrame");
        let paths = AppPaths {
            data_root: data_root.clone(),
            artifact_root,
        };
        let make_run = |run_id: &str, account_id: &str| {
            json!({
                "schema_version": 1,
                "run_id": run_id,
                "account_id": account_id,
                "depth_path": data_root.join("depth.json"),
                "tier": "l1",
                "settlement_currency": "USDT",
                "initial_cash_raw": 1_000_000_000_000_000_i128,
                "fee_bps": 0,
                "latency_snapshots": 0,
                "queue_position_bps": 0,
                "market_impact_bps": 0,
                "output_dir": "client-path-is-overridden",
                "strategy": {
                    "kind": "sma_cross",
                    "strategy_id": run_id,
                    "quantity_raw": qx_core::SCALE,
                    "fast_window": 2,
                    "slow_window": 3
                }
            })
        };
        let spec = json!({
            "schema_version": 1,
            "batch_id": "api-research-multi-account-depth",
            "runs": [
                make_run("api-research-depth-account-one", "account-one"),
                make_run("api-research-depth-account-two", "account-two")
            ]
        });
        let response = app_surface::depth_batch(&spec.to_string(), &paths);
        (paths, response)
    }

    #[test]
    fn verified_multi_leg_report_replays_shared_account_and_exposes_both_leg_timelines() {
        let (paths, outcome) = multi_leg_fixture();
        assert_eq!(outcome.status, 200, "multi-leg fixture must complete");
        let outcome_value: Value =
            serde_json::from_str(&outcome.body).expect("multi-leg outcome JSON");
        let response = route(&outcome.body, &paths);
        assert_eq!(
            response.status, 200,
            "multi-leg artifact must verify: {}",
            response.body
        );
        let view: Value = serde_json::from_str(&response.body).expect("multi-leg report JSON");
        assert_eq!(view["schema_version"], 10);
        assert_eq!(view["run_kind"], "multi_leg");
        assert_eq!(view["verification"]["verified"], true);
        assert_eq!(
            view["summary"]["execution_assumptions"]["fill_model"],
            "next_bar_open"
        );
        assert!(view["summary"]["execution_assumptions"]["market_specs"].is_object());
        assert_eq!(view["markets"].as_array().unwrap().len(), 2);
        assert!(view["markets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|market| market["bars"]["source_status"] == "verified"));
        assert_eq!(view["events"]["total"], view["summary"]["event_count"]);
        assert_eq!(
            view["fills"]["total"].as_u64().unwrap(),
            outcome_value["primary_fills"].as_u64().unwrap()
                + outcome_value["reference_fills"].as_u64().unwrap()
        );
        assert!(view["fills"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|fill| fill["instrument"].is_string()));

        let diagnostic = |instrument: &str, market_index: usize| {
            json!({
                "schema_version": 1,
                "kind": "qianxing-strategy-bias-diagnostics",
                "strategy_id": "api-research-pairs",
                "instrument": instrument,
                "data_fingerprint": view["markets"][market_index]["data_fingerprint"],
                "as_of": 147_600_000_u64,
                "input_bars": 12,
                "lookahead": {"status": "clear"},
                "startup": {"status": "stable"}
            })
        };
        let primary = view["markets"][0]["instrument"].as_str().unwrap();
        let reference = view["markets"][1]["instrument"].as_str().unwrap();
        let attached = route(
            &json!({
                "outcome": outcome_value.clone(),
                "strategy_diagnostics": diagnostic(primary, 0)
            })
            .to_string(),
            &paths,
        );
        assert_eq!(
            attached.status, 200,
            "leg-bound strategy diagnostics attach"
        );
        let attached: Value =
            serde_json::from_str(&attached.body).expect("bound leg diagnostic JSON");
        assert_eq!(attached["strategy_diagnostics"]["identity_bound"], true);
        let attached_batch = route(
            &json!({
                "outcome": outcome_value.clone(),
                "market_strategy_diagnostics": [diagnostic(primary, 0), diagnostic(reference, 1)]
            })
            .to_string(),
            &paths,
        );
        assert_eq!(attached_batch.status, 200, "both leg diagnostics attach");
        let attached_batch: Value =
            serde_json::from_str(&attached_batch.body).expect("bound leg diagnostic batch JSON");
        assert_eq!(
            attached_batch["market_strategy_diagnostics"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let attached_page = route(
            &json!({
                "outcome": outcome_value.clone(),
                "market_strategy_diagnostics": [diagnostic(primary, 0), diagnostic(reference, 1)],
                "event_page": {"offset": 0, "limit": 1}
            })
            .to_string(),
            &paths,
        );
        assert_eq!(
            attached_page.status, 200,
            "leg diagnostics survive event paging"
        );
        let attached_page: Value =
            serde_json::from_str(&attached_page.body).expect("paged multi-leg diagnostic JSON");
        assert_eq!(
            attached_page["market_strategy_diagnostics"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let page = route(
            &json!({
                "outcome": outcome_value.clone(),
                "event_page": {"offset": 0, "limit": 1}
            })
            .to_string(),
            &paths,
        );
        assert_eq!(page.status, 200, "multi-leg event pagination replays");
        let page: Value = serde_json::from_str(&page.body).expect("multi-leg event page JSON");
        assert_eq!(page["events"]["offset"], 0);
        assert_eq!(page["events"]["rows"].as_array().unwrap().len(), 1);

        let artifact_path = outcome_value["attribution_artifact"]
            .as_str()
            .expect("multi-leg attribution artifact path");
        let mut artifact: Value = serde_json::from_slice(
            &std::fs::read(artifact_path).expect("read multi-leg artifact for tamper check"),
        )
        .expect("parse multi-leg artifact for tamper check");
        artifact["shared_result_hash"] = json!("0000000000000000");
        std::fs::write(
            artifact_path,
            serde_json::to_vec(&artifact).expect("encode tampered multi-leg artifact"),
        )
        .expect("write tampered multi-leg artifact");
        let tampered = route(&json!({"outcome": outcome_value}).to_string(), &paths);
        assert_eq!(
            tampered.status, 409,
            "shared EventLog digest tampering is refused"
        );
    }

    #[test]
    fn verified_multi_market_report_replays_shared_ledger_and_exposes_linked_market_samples() {
        let (paths, outcome) = multi_market_fixture();
        assert_eq!(outcome.status, 200, "multi-market fixture must complete");
        let response = route(&outcome.body, &paths);
        assert_eq!(
            response.status, 200,
            "verified multi-market report must be readable"
        );
        let view: Value = serde_json::from_str(&response.body).expect("multi-market view JSON");
        assert_eq!(view["schema_version"], 12);
        assert_eq!(view["run_kind"], "multi_market");
        assert_eq!(view["verification"]["verified"], true);
        assert_eq!(
            view["summary"]["execution_assumptions"]["markets"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(view["markets"].as_array().unwrap().len(), 3);
        assert_eq!(view["markets"][0]["bars"]["source_status"], "verified");
        assert!(view["markets"].as_array().unwrap().iter().all(|market| {
            market["pnl_contribution"]["rows"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
                && market["pnl_contribution"]["currency"] == "USDT"
        }));
        assert_eq!(view["events"]["total"], view["summary"]["event_count"]);
        assert!(view["equity"]["rows"].as_array().unwrap().len() >= 2);
        let final_contributions = view["markets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|market| {
                market["pnl_contribution"]["rows"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()["pnl_contribution_raw"]
                    .as_str()
                    .unwrap()
                    .parse::<i128>()
                    .unwrap()
            })
            .sum::<i128>();
        let initial_cash = view["summary"]["account"]["initial_cash_raw"]
            .as_str()
            .unwrap()
            .parse::<i128>()
            .unwrap();
        let final_equity = view["summary"]["metrics"]["final_equity_raw"]
            .as_str()
            .unwrap()
            .parse::<i128>()
            .unwrap();
        assert_eq!(initial_cash + final_contributions, final_equity);
        assert!(view["verification"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check.as_str().unwrap().contains("逐标的权益贡献")));

        let outcome_value: Value = serde_json::from_str(&outcome.body).expect("outcome JSON");
        let diagnostic = json!({
            "schema_version": 1,
            "kind": "qianxing-strategy-bias-diagnostics",
            "strategy_id": "sma-0",
            "instrument": "BTCUSDT.BINANCE",
            "data_fingerprint": view["markets"][0]["data_fingerprint"],
            "as_of": 1_700_000_000_u64,
            "input_bars": 12,
            "lookahead": {"status": "clear"},
            "startup": {"status": "stable"}
        });
        let attached = route(
            &json!({"outcome": outcome_value.clone(), "strategy_diagnostics": diagnostic.clone()})
                .to_string(),
            &paths,
        );
        assert_eq!(
            attached.status, 200,
            "matching market strategy diagnostic attaches"
        );
        let attached: Value = serde_json::from_str(&attached.body).expect("diagnostic view JSON");
        assert_eq!(attached["schema_version"], 12);
        assert_eq!(attached["strategy_diagnostics"]["identity_bound"], true);
        assert_eq!(
            attached["strategy_diagnostics"]["report"]["strategy_id"],
            "sma-0"
        );

        let make_diagnostic = |strategy_id: &str, instrument: &str, market_index: usize| {
            json!({
                "schema_version": 1,
                "kind": "qianxing-strategy-bias-diagnostics",
                "strategy_id": strategy_id,
                "instrument": instrument,
                "data_fingerprint": view["markets"][market_index]["data_fingerprint"],
                "as_of": 1_700_000_000_u64,
                "input_bars": 12,
                "lookahead": {"status": "clear"},
                "startup": {"status": "stable"}
            })
        };
        let diagnostic_batch = json!([
            make_diagnostic("sma-0", "BTCUSDT.BINANCE", 0),
            make_diagnostic("sma-1", "ETHUSDT.KRAKEN", 1)
        ]);
        let attached_batch = route(
            &json!({"outcome": outcome_value.clone(), "market_strategy_diagnostics": diagnostic_batch.clone()})
                .to_string(),
            &paths,
        );
        assert_eq!(
            attached_batch.status, 200,
            "matching diagnostic batch attaches"
        );
        let attached_batch: Value =
            serde_json::from_str(&attached_batch.body).expect("diagnostic batch view JSON");
        assert_eq!(attached_batch["schema_version"], 12);
        assert_eq!(
            attached_batch["market_strategy_diagnostics"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(attached_batch["market_strategy_diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|report| report["identity_bound"] == true));
        let diagnostic_page = route(
            &json!({
                "outcome": outcome_value.clone(),
                "market_strategy_diagnostics": diagnostic_batch.clone(),
                "event_page": {"offset": 0, "limit": 3}
            })
            .to_string(),
            &paths,
        );
        assert_eq!(
            diagnostic_page.status, 200,
            "diagnostic batch survives event paging"
        );
        let diagnostic_page: Value =
            serde_json::from_str(&diagnostic_page.body).expect("diagnostic event page JSON");
        assert_eq!(diagnostic_page["schema_version"], 12);
        assert_eq!(
            diagnostic_page["market_strategy_diagnostics"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let duplicate_batch = json!([
            make_diagnostic("sma-0", "BTCUSDT.BINANCE", 0),
            make_diagnostic("sma-0", "BTCUSDT.BINANCE", 0)
        ]);
        let duplicate_response = route(
            &json!({"outcome": outcome_value.clone(), "market_strategy_diagnostics": duplicate_batch})
                .to_string(),
            &paths,
        );
        assert_ne!(
            duplicate_response.status, 200,
            "duplicate diagnostics are rejected"
        );

        let mut wrong_fingerprint_batch = json!([make_diagnostic("sma-1", "ETHUSDT.KRAKEN", 1)]);
        wrong_fingerprint_batch[0]["data_fingerprint"] = json!("wrong-market-fingerprint");
        let wrong_fingerprint_response = route(
            &json!({
                "outcome": outcome_value.clone(),
                "market_strategy_diagnostics": wrong_fingerprint_batch
            })
            .to_string(),
            &paths,
        );
        assert_ne!(
            wrong_fingerprint_response.status, 200,
            "diagnostics with an unverified market fingerprint are rejected"
        );

        let mixed_attachment = route(
            &json!({
                "outcome": outcome_value.clone(),
                "strategy_diagnostics": diagnostic.clone(),
                "market_strategy_diagnostics": [make_diagnostic("sma-1", "ETHUSDT.KRAKEN", 1)]
            })
            .to_string(),
            &paths,
        );
        assert_ne!(
            mixed_attachment.status, 200,
            "single and batch forms cannot be mixed"
        );

        let wrong_strategy = json!({
            "schema_version": 1,
            "kind": "qianxing-strategy-bias-diagnostics",
            "strategy_id": "sma-1",
            "instrument": "BTCUSDT.BINANCE",
            "data_fingerprint": view["markets"][0]["data_fingerprint"],
            "as_of": 1_700_000_000_u64,
            "input_bars": 12,
            "lookahead": {"status": "clear"},
            "startup": {"status": "stable"}
        });
        let rejected_diagnostic = route(
            &json!({"outcome": outcome_value.clone(), "strategy_diagnostics": wrong_strategy})
                .to_string(),
            &paths,
        );
        assert_ne!(
            rejected_diagnostic.status, 200,
            "another market's strategy cannot bind"
        );

        let page_request = serde_json::json!({
            "outcome": outcome_value,
            "event_page": {"offset": 0, "limit": 300}
        });
        let page = route(&page_request.to_string(), &paths);
        assert_eq!(page.status, 200, "verified multi-market event page");
        let page: Value = serde_json::from_str(&page.body).expect("event page JSON");
        assert_eq!(page["events"]["total"], view["events"]["total"]);
        let rows = page["events"]["rows"].as_array().unwrap();
        assert!(rows.len() >= 5);
        assert!(rows.iter().all(|row| {
            row.get("bar_context").is_some()
                && row.get("bar_context_status").is_some()
                && row.get("market_context").is_some()
        }));
        let linked_risk_decision = rows
            .iter()
            .find(|row| {
                row.pointer("/research_fact/kind") == Some(&Value::String("risk_decision".into()))
            })
            .expect("multi-market risk decision is present on event page");
        assert!(linked_risk_decision["market_context"]["instrument"].is_string());
        assert!(linked_risk_decision["market_context"]["data_fingerprint"].is_string());
        assert_eq!(linked_risk_decision["bar_context_status"], "verified");
        let linked_markets = rows
            .iter()
            .filter(|row| {
                row.pointer("/research_fact/kind") == Some(&Value::String("risk_decision".into()))
            })
            .filter_map(|row| {
                row.pointer("/market_context/instrument")
                    .and_then(Value::as_str)
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            linked_markets.len(),
            3,
            "each strategy risk event must bind its market"
        );
        let decision_ts = linked_risk_decision["ts"].as_u64().unwrap();
        let decision_bar_ts = linked_risk_decision
            .pointer("/bar_context/bar/ts")
            .and_then(Value::as_u64)
            .expect("risk decision has a visible market Bar");
        assert!(
            decision_bar_ts < decision_ts,
            "command-time risk decisions must only see prior Bars"
        );
        let fills = rows
            .iter()
            .filter(|row| row.pointer("/kind/Filled").is_some())
            .collect::<Vec<_>>();
        assert_eq!(fills.len(), 3);
        assert!(
            fills.iter().all(|row| {
                row["market_context"]["instrument"].is_string()
                    && row["bar_context_status"] == "verified"
            }),
            "fills must inherit market identity through their order ledger chain"
        );

        let outcome_value: Value = serde_json::from_str(&outcome.body).expect("outcome JSON");
        let artifact_path = outcome_value["artifact_path"].as_str().unwrap();
        let artifact: Value = serde_json::from_slice(
            &std::fs::read(artifact_path).expect("read multi-market artifact"),
        )
        .expect("parse multi-market artifact");
        let contribution_path = artifact["instrument_equity_contributions_path"]
            .as_str()
            .expect("instrument contribution sidecar path");
        let mut contribution_sidecar: Value = serde_json::from_slice(
            &std::fs::read(contribution_path).expect("read instrument contribution sidecar"),
        )
        .expect("parse instrument contribution sidecar");
        contribution_sidecar["BTCUSDT.BINANCE"][0]["pnl_contribution_raw"] = json!("1");
        let contribution_bytes = serde_json::to_vec(&contribution_sidecar).unwrap();
        std::fs::write(contribution_path, &contribution_bytes)
            .expect("tamper instrument contribution sample");
        let manifest_path = artifact["run_manifest_path"].as_str().unwrap();
        let mut manifest: Value = serde_json::from_slice(
            &std::fs::read(manifest_path).expect("read multi-market manifest"),
        )
        .expect("parse multi-market manifest");
        manifest["input_components"]["derived:instrument-equity-contributions"] =
            json!(qx_data::sha256_hex_bytes(&contribution_bytes));
        std::fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap())
            .expect("update manifest to bind tampered contribution");
        let rejected_contribution = route(&outcome.body, &paths);
        assert_ne!(
            rejected_contribution.status, 200,
            "a manifest-bound but nonconserving contribution curve is rejected"
        );

        let first_bar = paths.data_root.join("market-0.json");
        let mut changed: Value =
            serde_json::from_slice(&std::fs::read(&first_bar).expect("read market fixture"))
                .expect("BarFrame JSON");
        changed["close_raw"][0] = Value::from(123_i64);
        std::fs::write(&first_bar, serde_json::to_vec(&changed).unwrap())
            .expect("tamper market input");
        let rejected = route(&outcome.body, &paths);
        assert_ne!(
            rejected.status, 200,
            "changed input must not pass report verification"
        );
    }

    #[test]
    fn verified_multi_market_depth_report_replays_shared_ledger_and_exposes_depth_samples() {
        let (paths, outcome) = multi_market_depth_fixture();
        assert_eq!(
            outcome.status, 200,
            "multi-market depth fixture must complete: {}",
            outcome.body
        );
        let response = route(&outcome.body, &paths);
        assert_eq!(
            response.status, 200,
            "multi-market depth report must be independently verifiable: {}",
            response.body
        );
        let view: Value = serde_json::from_str(&response.body).expect("depth report JSON");
        assert_eq!(view["schema_version"], 5);
        assert_eq!(view["run_kind"], "multi_market_depth");
        assert_eq!(view["verification"]["verified"], true);
        assert_eq!(
            view["summary"]["execution_assumptions"]["markets"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(view["markets"].as_array().unwrap().len(), 2);
        for (index, market) in view["markets"].as_array().unwrap().iter().enumerate() {
            assert_eq!(market["data_tier"], "l1");
            assert_eq!(market["depth"]["source_status"], "verified");
            assert!(market["depth"]["total"].as_u64().unwrap() >= 2);
            assert!(market["depth"]["rows"].as_array().unwrap().len() >= 2);
            assert_eq!(
                market["quality_status"],
                if index == 0 {
                    "verified"
                } else {
                    "not_provided"
                }
            );
        }
        assert_eq!(view["events"]["total"], view["summary"]["event_count"]);
        assert!(view["equity"]["rows"].as_array().unwrap().len() >= 2);

        let outcome_value: Value = serde_json::from_str(&outcome.body).expect("depth outcome");
        let depth_diagnostics = view["markets"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(index, market)| {
                json!({
                    "schema_version": 1,
                    "kind": "qianxing-strategy-bias-diagnostics",
                    "strategy_id": format!("api-multi-market-depth-{index}"),
                    "instrument": market["instrument"],
                    "data_fingerprint": market["data_fingerprint"],
                    "as_of": market["depth"]["rows"].as_array().unwrap().last().unwrap()["ts"],
                    "input_kind": "depth_midpoint_bar",
                    "input_events": market["depth"]["total"],
                    "lookahead": {"status": "no_change_detected"},
                    "startup": {"status": "stable_across_requested_windows"}
                })
            })
            .collect::<Vec<_>>();
        let attached = route(
            &json!({
                "outcome": outcome_value.clone(),
                "market_strategy_diagnostics": depth_diagnostics
            })
            .to_string(),
            &paths,
        );
        assert_eq!(
            attached.status, 200,
            "matching per-market depth diagnostics bind to verified strategy slots: {}",
            attached.body
        );
        let attached: Value = serde_json::from_str(&attached.body).expect("depth diagnostics view");
        assert_eq!(attached["schema_version"], 11);
        assert_eq!(
            attached["market_strategy_diagnostics"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(attached["market_strategy_diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["identity_bound"] == true));
        let mut wrong_event_count = attached["market_strategy_diagnostics"][0]["report"].clone();
        wrong_event_count["input_events"] = json!(0);
        let rejected_coverage = route(
            &json!({
                "outcome": outcome_value.clone(),
                "market_strategy_diagnostics": [wrong_event_count]
            })
            .to_string(),
            &paths,
        );
        assert_ne!(
            rejected_coverage.status, 200,
            "depth diagnostic event count must match the verified frame"
        );
        let mut wrong_timestamp = attached["market_strategy_diagnostics"][0]["report"].clone();
        wrong_timestamp["as_of"] = json!(wrong_timestamp["as_of"].as_u64().unwrap() + 1);
        let rejected_timestamp = route(
            &json!({
                "outcome": outcome_value.clone(),
                "market_strategy_diagnostics": [wrong_timestamp]
            })
            .to_string(),
            &paths,
        );
        assert_ne!(
            rejected_timestamp.status, 200,
            "depth diagnostic decision time must match the final input snapshot"
        );
        let mut wrong_strategy_kind = attached["market_strategy_diagnostics"][0]["report"].clone();
        wrong_strategy_kind["input_kind"] = json!("order_book");
        let rejected_kind = route(
            &json!({
                "outcome": outcome_value.clone(),
                "market_strategy_diagnostics": [wrong_strategy_kind]
            })
            .to_string(),
            &paths,
        );
        assert_ne!(
            rejected_kind.status, 200,
            "built-in depth strategy must not claim callback OrderBook input"
        );
        let attached_page = route(
            &json!({
                "outcome": outcome_value.clone(),
                "market_strategy_diagnostics": attached["market_strategy_diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|item| item["report"].clone())
                    .collect::<Vec<_>>(),
                "event_page": {"offset": 0, "limit": 3}
            })
            .to_string(),
            &paths,
        );
        assert_eq!(
            attached_page.status, 200,
            "depth diagnostics survive event paging"
        );
        let attached_page: Value =
            serde_json::from_str(&attached_page.body).expect("depth diagnostic event page");
        assert_eq!(attached_page["schema_version"], 11);
        assert_eq!(
            attached_page["market_strategy_diagnostics"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let page_request = json!({
            "outcome": outcome_value,
            "event_page": {"offset": 0, "limit": 3}
        });
        let page = route(&page_request.to_string(), &paths);
        assert_eq!(page.status, 200, "depth event page is replay-verified");
        let page: Value = serde_json::from_str(&page.body).expect("depth event page");
        assert_eq!(page["events"]["total"], view["events"]["total"]);
        assert!(page["events"]["rows"].as_array().unwrap().len() <= 3);

        let first_depth_path = paths.data_root.join("depth-0.json");
        let original_depth = std::fs::read(&first_depth_path).expect("read original depth frame");
        let mut changed_depth: Value =
            serde_json::from_slice(&original_depth).expect("parse original depth frame");
        changed_depth["source"] = json!("changed-depth-source");
        std::fs::write(
            &first_depth_path,
            serde_json::to_vec(&changed_depth).unwrap(),
        )
        .expect("tamper depth source");
        let rejected_input = route(&outcome.body, &paths);
        assert_ne!(
            rejected_input.status, 200,
            "changed per-market depth input must fail identity verification"
        );
        std::fs::write(&first_depth_path, original_depth).expect("restore depth frame");

        let artifact_path = outcome_value["artifact_path"]
            .as_str()
            .expect("artifact path");
        let mut artifact: Value = read_json(Path::new(artifact_path)).expect("artifact JSON");
        let event = artifact["event_log"]
            .as_array_mut()
            .and_then(|events| events.first_mut())
            .expect("at least one event");
        event["ts"] = json!(event["ts"].as_u64().unwrap_or_default() + 1);
        std::fs::write(artifact_path, serde_json::to_vec(&artifact).unwrap())
            .expect("tamper event log");
        let rejected = route(&outcome.body, &paths);
        assert_ne!(
            rejected.status, 200,
            "changed shared event stream must fail digest/replay verification"
        );
    }

    #[test]
    fn verified_multi_account_report_exposes_independent_account_drilldowns() {
        let (paths, outcome) = multi_account_fixture();
        assert_eq!(outcome.status, 200, "multi-account fixture must complete");
        let response = route(&outcome.body, &paths);
        assert_eq!(
            response.status, 200,
            "verified multi-account report must be readable: {}",
            response.body
        );
        let view: Value = serde_json::from_str(&response.body).expect("multi-account view JSON");
        assert_eq!(view["schema_version"], 7);
        assert_eq!(view["run_kind"], "multi_account");
        assert_eq!(view["verification"]["verified"], true);
        assert_eq!(view["summary"]["accounts_total"], 2);
        assert_eq!(view["summary"]["accounts_succeeded"], 2);
        assert_eq!(view["accounts"].as_array().unwrap().len(), 2);
        let instrument_by_account = view["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|account| {
                (
                    account["account_id"].as_str().unwrap().to_owned(),
                    account["instrument"].as_str().unwrap().to_owned(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_ne!(
            instrument_by_account["account-one"],
            instrument_by_account["account-two"]
        );
        for account in view["accounts"].as_array().unwrap() {
            assert_eq!(account["status"], "succeeded");
            assert_eq!(account["report"]["verification"]["verified"], true);
            assert_eq!(account["report"]["bars"]["source_status"], "verified");
            assert!(
                account["report"]["equity"]["rows"]
                    .as_array()
                    .unwrap()
                    .len()
                    >= 2
            );
        }

        let batch: Value = serde_json::from_str(&outcome.body).expect("multi-account outcome");
        let account_diagnostics = batch["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|member| {
                let account_id = member["account_id"].as_str().unwrap();
                let summary_path = member["outcome"]["artifacts"]["summary"]
                    .as_str()
                    .expect("member summary artifact");
                let summary = read_json(Path::new(summary_path)).expect("member summary");
                json!({
                    "account_id": account_id,
                    "strategy_diagnostics": {
                        "schema_version": 1,
                        "kind": "qianxing-strategy-bias-diagnostics",
                        "strategy_id": summary["strategy_id"],
                        "strategy_version": summary["strategy_version"],
                        "data_fingerprint": summary["data_fingerprint"],
                        "instrument": summary["instrument"],
                        "as_of": summary["clock_end"],
                        "input_bars": summary["bars"],
                        "lookahead": {"status": "no_change_detected"},
                        "startup": {"status": "stable_across_requested_windows"}
                    }
                })
            })
            .collect::<Vec<_>>();
        let diagnostic_batch_request = json!({
            "outcome": batch.clone(),
            "account_strategy_diagnostics": account_diagnostics.clone()
        });
        let diagnostic_batch_response = route(&diagnostic_batch_request.to_string(), &paths);
        assert_eq!(
            diagnostic_batch_response.status, 200,
            "per-account diagnostics should bind to each independently verified report: {}",
            diagnostic_batch_response.body
        );
        let diagnostic_batch_view: Value =
            serde_json::from_str(&diagnostic_batch_response.body).expect("diagnostic batch view");
        for account in diagnostic_batch_view["accounts"].as_array().unwrap() {
            assert_eq!(
                account["report"]["strategy_diagnostics"]["identity_bound"],
                true
            );
            assert_eq!(
                account["report"]["strategy_diagnostics"]["report"]["instrument"],
                account["instrument"]
            );
        }
        let diagnostic_page_request = json!({
            "outcome": batch.clone(),
            "account_strategy_diagnostics": account_diagnostics,
            "event_page": {"offset": 0, "limit": 2},
            "account_event_offsets": {"account-one": 0, "account-two": 0}
        });
        let diagnostic_page_response = route(&diagnostic_page_request.to_string(), &paths);
        assert_eq!(
            diagnostic_page_response.status, 200,
            "event paging must preserve account diagnostic attachments: {}",
            diagnostic_page_response.body
        );
        let diagnostic_page_view: Value =
            serde_json::from_str(&diagnostic_page_response.body).expect("diagnostic page view");
        for account in diagnostic_page_view["accounts"].as_array().unwrap() {
            assert_eq!(
                account["report"]["strategy_diagnostics"]["identity_bound"], true,
                "diagnostics must remain attached after an event-page request"
            );
        }
        let mut wrong_identity = diagnostic_batch_request.clone();
        wrong_identity["account_strategy_diagnostics"][0]["strategy_diagnostics"]
            ["data_fingerprint"] = json!("another-dataset");
        let wrong_identity_response = route(&wrong_identity.to_string(), &paths);
        assert_eq!(
            wrong_identity_response.status, 409,
            "a diagnostic cannot be rebound to another account's data fingerprint"
        );

        let all_events = route(&json!({"outcome": batch.clone()}).to_string(), &paths);
        assert_eq!(all_events.status, 200, "full verified event page must load");
        let all_events: Value = serde_json::from_str(&all_events.body).unwrap();
        assert_eq!(all_events["schema_version"], 7);
        assert_eq!(all_events["events"]["truncated"], false);
        let linked_instruments = all_events["events"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| {
                let account_id = event["account_id"].as_str().unwrap();
                let instrument = event["instrument"].as_str().unwrap();
                assert_eq!(instrument, instrument_by_account[account_id]);
                instrument.to_owned()
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert!(linked_instruments.contains("BTCUSDT.BINANCE"));
        assert!(linked_instruments.contains("ETHUSDT.BINANCE"));
        let filtered_request = json!({
            "outcome": batch.clone(),
            "event_page": {"offset": 0, "limit": 2},
            "event_instrument": "BTCUSDT.BINANCE"
        });
        let filtered_page = route(&filtered_request.to_string(), &paths);
        assert_eq!(
            filtered_page.status, 200,
            "instrument-filtered event page must load"
        );
        let filtered_page: Value = serde_json::from_str(&filtered_page.body).unwrap();
        assert_eq!(filtered_page["event_instrument"], "BTCUSDT.BINANCE");
        assert!(
            filtered_page["events"]["total"].as_u64().unwrap()
                < all_events["events"]["total"].as_u64().unwrap()
        );
        assert!(filtered_page["events"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["instrument"] == "BTCUSDT.BINANCE"));
        assert_eq!(filtered_page["account_event_offsets"]["account-two"], 0);
        let btc_next_offset = filtered_page["account_event_offsets"]
            .as_object()
            .unwrap()
            .values()
            .map(|offset| offset.as_u64().unwrap())
            .sum::<u64>();
        assert_eq!(btc_next_offset, 2);
        let filtered_next_request = json!({
            "outcome": batch.clone(),
            "event_page": {"offset": btc_next_offset, "limit": 2},
            "account_event_offsets": filtered_page["account_event_offsets"],
            "event_instrument": "BTCUSDT.BINANCE"
        });
        let filtered_next = route(&filtered_next_request.to_string(), &paths);
        assert_eq!(
            filtered_next.status, 200,
            "instrument-filtered cursor must replay"
        );
        let filtered_next: Value = serde_json::from_str(&filtered_next.body).unwrap();
        assert!(filtered_next["events"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["instrument"] == "BTCUSDT.BINANCE"));
        assert_eq!(filtered_next["events"]["offset"], btc_next_offset);
        assert_eq!(filtered_next["account_event_offsets"]["account-two"], 0);
        let mut excluded_cursor: Value =
            serde_json::from_value(filtered_page["account_event_offsets"].clone()).unwrap();
        excluded_cursor["account-two"] = json!(1);
        let excluded_cursor_request = json!({
            "outcome": batch.clone(),
            "event_page": {"offset": 3, "limit": 2},
            "account_event_offsets": excluded_cursor,
            "event_instrument": "BTCUSDT.BINANCE"
        });
        assert_ne!(
            route(&excluded_cursor_request.to_string(), &paths).status,
            200
        );
        let unknown_instrument_request = json!({
            "outcome": batch.clone(),
            "event_instrument": "UNKNOWN.BINANCE"
        });
        assert_ne!(
            route(&unknown_instrument_request.to_string(), &paths).status,
            200
        );
        let page_request = json!({
            "outcome": batch,
            "event_page": {"offset": 0, "limit": 2}
        });
        let page = route(&page_request.to_string(), &paths);
        assert_eq!(
            page.status, 200,
            "account event page must be replay-verified"
        );
        let page: Value = serde_json::from_str(&page.body).expect("account event page JSON");
        assert!(
            page["accounts"][0]["report"]["events"]["rows"]
                .as_array()
                .unwrap()
                .len()
                <= 2
        );
        let merged_rows = page["events"]["rows"].as_array().unwrap();
        assert_eq!(merged_rows.len(), 2);
        assert!(page["events"]["total"].as_u64().unwrap() > 2);
        assert_eq!(page["events"]["truncated"], true);
        assert!(merged_rows
            .iter()
            .all(|event| event["account_id"].is_string()));
        let sort_key = |event: &Value| {
            (
                event["ts"].as_u64().unwrap(),
                event["prio"].as_u64().unwrap(),
                event["account_id"].as_str().unwrap().to_owned(),
                event["seq"].as_u64().unwrap(),
            )
        };
        assert!(merged_rows
            .windows(2)
            .all(|pair| sort_key(&pair[0]) <= sort_key(&pair[1])));
        let next_offsets = page["account_event_offsets"].as_object().unwrap();
        let next_offset = next_offsets
            .values()
            .map(|offset| offset.as_u64().unwrap())
            .sum::<u64>();
        assert_eq!(next_offset, 2);
        assert_eq!(page["events"]["offset"], 0);

        if page["events"]["truncated"] == true {
            let next_request = json!({
                "outcome": batch,
                "event_page": {"offset": next_offset, "limit": 2},
                "account_event_offsets": page["account_event_offsets"]
            });
            let next_page = route(&next_request.to_string(), &paths);
            assert_eq!(
                next_page.status, 200,
                "merged account event cursor must be replayable"
            );
            let replay = route(&next_request.to_string(), &paths);
            assert_eq!(replay.status, 200);
            let next_page: Value = serde_json::from_str(&next_page.body).unwrap();
            let replay: Value = serde_json::from_str(&replay.body).unwrap();
            assert_eq!(next_page["events"], replay["events"]);
            assert_eq!(
                next_page["account_event_offsets"],
                replay["account_event_offsets"]
            );
            assert_eq!(next_page["events"]["offset"], next_offset);
            let next_rows = next_page["events"]["rows"].as_array().unwrap();
            assert!(next_rows.len() <= 2);
            let mut last_sequence_by_account = BTreeMap::new();
            for event in merged_rows.iter().chain(next_rows) {
                let account_id = event["account_id"].as_str().unwrap();
                let sequence = event["seq"].as_u64().unwrap();
                if let Some(previous) = last_sequence_by_account.insert(account_id, sequence) {
                    assert!(
                        sequence > previous,
                        "account cursor must not repeat or skip backwards"
                    );
                }
            }
        }

        let invalid_cursor = json!({
            "outcome": batch,
            "event_page": {"offset": 1, "limit": 2},
            "account_event_offsets": {}
        });
        assert_ne!(route(&invalid_cursor.to_string(), &paths).status, 200);
        let unknown_account_cursor = json!({
            "outcome": batch,
            "event_page": {"offset": 1, "limit": 2},
            "account_event_offsets": {"unknown-account": 1}
        });
        assert_ne!(
            route(&unknown_account_cursor.to_string(), &paths).status,
            200
        );

        let mut tampered: Value = serde_json::from_str(&outcome.body).expect("batch result");
        tampered["summary"]["successful_fills"] =
            json!(tampered["summary"]["successful_fills"].as_u64().unwrap() + 1);
        let rejected = route(&tampered.to_string(), &paths);
        assert_ne!(
            rejected.status, 200,
            "a batch summary inconsistent with verified member runs must be rejected"
        );
    }

    #[test]
    fn verified_multi_account_depth_report_exposes_independent_depth_drilldowns() {
        let (paths, outcome) = multi_account_depth_fixture();
        assert_eq!(
            outcome.status, 200,
            "multi-account depth fixture must complete"
        );
        let response = route(&outcome.body, &paths);
        assert_eq!(
            response.status, 200,
            "verified multi-account depth report must be readable: {}",
            response.body
        );
        let view: Value = serde_json::from_str(&response.body).expect("multi-account depth view");
        assert_eq!(view["schema_version"], 7);
        assert_eq!(view["run_kind"], "multi_account");
        assert_eq!(view["verification"]["verified"], true);
        assert_eq!(view["accounts"].as_array().unwrap().len(), 2);
        for account in view["accounts"].as_array().unwrap() {
            assert_eq!(account["status"], "succeeded");
            assert_eq!(account["report"]["run_kind"], "depth");
            assert_eq!(account["report"]["verification"]["verified"], true);
            assert_eq!(account["report"]["depth"]["source_status"], "verified");
        }
    }

    #[test]
    fn verified_depth_report_exposes_bounded_input_and_rejects_changed_source() {
        let (paths, outcome) = depth_backtest_fixture();
        assert_eq!(outcome.status, 200, "depth backtest fixture must complete");
        let response = route(&outcome.body, &paths);
        assert_eq!(
            response.status, 200,
            "verified depth detail must be readable"
        );
        let view: Value = serde_json::from_str(&response.body).expect("depth artifact view JSON");
        assert_eq!(view["schema_version"], 2);
        assert_eq!(view["verification"]["verified"], true);
        assert_eq!(view["run_kind"], "depth");
        assert!(view["verification"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check.as_str().unwrap().contains("深度 EventLog")));
        assert_eq!(view["event_trace_available"], true);
        assert!(view["events"]["total"].as_u64().unwrap() > 0);
        assert_eq!(view["bars"]["source_status"], "unsupported_input_kind");
        assert_eq!(view["depth"]["source_status"], "verified");
        assert!(view["depth"]["total"].as_u64().unwrap() > 0);
        assert!(view["depth"]["rows"][0]["bids"][0]["price_raw"].is_string());
        assert!(view["equity"]["total"].as_u64().unwrap() > 0);

        let outcome_value: Value = serde_json::from_str(&outcome.body).expect("depth outcome");
        let page_request = serde_json::json!({
            "outcome": outcome_value,
            "event_page": {"offset": 0, "limit": 5}
        });
        let page_response = route(&page_request.to_string(), &paths);
        assert_eq!(page_response.status, 200, "depth events can be paged");
        let page: Value = serde_json::from_str(&page_response.body).expect("depth event page");
        assert_eq!(page["events"]["offset"], 0);
        assert_eq!(page["events"]["rows"].as_array().unwrap().len(), 5);
        assert_eq!(
            page["events"]["rows"][0]["bar_context_status"],
            "not_applicable_depth"
        );

        let event_trace_path = PathBuf::from(
            outcome_value["artifacts"]["event_trace"]
                .as_str()
                .expect("depth event trace path"),
        );
        let event_trace = std::fs::read_to_string(&event_trace_path).expect("read depth trace");
        let mut changed_trace: Value =
            serde_json::from_str(event_trace.lines().next().expect("first depth event"))
                .expect("parse first depth event");
        changed_trace["ts"] = Value::from(changed_trace["ts"].as_u64().unwrap() + 1);
        let mut tampered = serde_json::to_string(&changed_trace).unwrap();
        tampered.push('\n');
        tampered.push_str(
            event_trace
                .lines()
                .skip(1)
                .collect::<Vec<_>>()
                .join("\n")
                .as_str(),
        );
        tampered.push('\n');
        std::fs::write(&event_trace_path, tampered).expect("tamper depth trace");
        let tampered_response = route(&outcome.body, &paths);
        assert_ne!(
            tampered_response.status, 200,
            "changed depth events must fail replay verification"
        );
        std::fs::write(&event_trace_path, &event_trace).expect("restore depth trace");

        let input_path = paths.data_root.join("depth.json");
        let original = std::fs::read_to_string(&input_path).expect("read depth input");
        let mut changed: Value = serde_json::from_str(&original).expect("parse depth input");
        let bid_price = changed["snapshots"][0]["bids"][0]["price"]
            .as_u64()
            .expect("raw bid price");
        changed["snapshots"][0]["bids"][0]["price"] = Value::from(bid_price + 1);
        std::fs::write(
            &input_path,
            serde_json::to_vec(&changed).expect("serialize changed depth input"),
        )
        .expect("change depth input values");
        let changed_source = route(&outcome.body, &paths);
        assert_eq!(
            changed_source.status, 503,
            "changed source bytes must fail depth input identity verification"
        );
    }

    #[test]
    fn verified_research_artifacts_are_bounded_and_event_digest_checked() {
        let (paths, outcome) = backtest_fixture();
        assert_eq!(outcome.status, 200, "backtest fixture must complete");
        let response = route(&outcome.body, &paths);
        assert_eq!(
            response.status, 200,
            "verified artifact view must be readable"
        );
        let view: Value = serde_json::from_str(&response.body).expect("artifact view JSON");
        assert_eq!(view["schema_version"], 1);
        assert!(
            view.get("run_kind").is_none(),
            "Bar output keeps the v1 response shape"
        );
        assert_eq!(view["verification"]["verified"], true);
        assert!(view["equity"]["total"].as_u64().unwrap() > 0);
        assert!(view["equity"]["rows"][0]["equity_raw"].is_string());
        assert!(view["equity"]["rows"][0]["position_raw"].is_string());
        assert_eq!(view["equity"]["rows"][0]["position_raw"], "0");
        assert!(view["equity"]["rows"]
            .as_array()
            .expect("equity samples")
            .iter()
            .any(|row| row["position_raw"].as_str().is_some_and(|raw| raw != "0")));
        assert!(view["summary"]["fees_raw"].is_number());
        assert!(
            view["summary"]["rejection_facts"].is_null()
                || view["summary"]["rejection_facts"].is_array()
        );
        assert!(view["events"]["total"].as_u64().unwrap() > 0);
        assert!(view["fills"]["total"].as_u64().unwrap() > 0);
        assert_eq!(view["bars"]["source_status"], "verified");
        assert_eq!(view["bars"]["total"], view["summary"]["bars"]);
        assert!(view["bars"]["rows"][0]["close_raw"].is_string());
        let timeline = view["events"]["rows"].as_array().expect("event timeline");
        assert!(timeline
            .iter()
            .any(|event| { event["bar_context"]["relation"] == "same_timestamp_visible" }));
        let strategy_decision = timeline
            .iter()
            .find(|event| {
                event["research_fact"]["kind"] == "strategy_decision"
                    && event["research_fact"]["intent_count"].as_u64().unwrap_or(0) > 0
            })
            .expect("the interactive report exposes a standalone strategy fact");
        assert_eq!(
            strategy_decision["metadata"]["context"]["strategy_id"],
            strategy_decision["research_fact"]["strategy_id"]
        );
        assert_eq!(
            strategy_decision["research_fact"]["intent_count"].as_u64(),
            Some(1)
        );
        for field in ["signal_id", "confidence", "priority", "expires_at"] {
            assert!(
                strategy_decision["research_fact"][field].is_string(),
                "strategy {field} remains an exact integer string in the research view"
            );
        }
        assert_eq!(
            strategy_decision["bar_context"]["relation"], "previous_visible",
            "a Bar decision at t only sees the completed input through t-1"
        );
        let risk_decision = timeline
            .iter()
            .find(|event| event["research_fact"]["kind"] == "risk_decision")
            .expect("the interactive report exposes the exact risk adjudication");
        assert_eq!(risk_decision["research_fact"]["allowed"], true);
        assert!(risk_decision["research_fact"]["instrument"]["symbol"].is_string());
        assert!(risk_decision["research_fact"]["instrument"]["venue"].is_string());
        for field in [
            "projected_position_raw",
            "projected_margin_raw",
            "reference_price_raw",
        ] {
            assert!(
                risk_decision["research_fact"][field].is_string(),
                "risk projection {field} remains an exact raw integer string or explicit null"
            );
        }
        assert_eq!(
            risk_decision["bar_context"]["relation"], "previous_visible",
            "risk facts must use the same strict visible-Bar boundary as the strategy decision"
        );
        assert!(
            timeline.iter().all(|event| {
                let Some(context) = event.get("bar_context").filter(|value| !value.is_null())
                else {
                    return true;
                };
                let event_ts = event["ts"].as_u64().unwrap();
                let bar_ts = context["bar"]["ts"].as_u64().unwrap();
                bar_ts < event_ts
                    || (bar_ts == event_ts
                        && event["prio"].as_u64().unwrap() >= qx_core::Priority::MARKET as u64)
            }),
            "same-timestamp events before market-data priority must not see that Bar"
        );

        let run_id = view["summary"]["run_id"].as_str().expect("summary run id");
        let fills_path = paths
            .artifact_root
            .join(run_id)
            .join(format!("{run_id}.fills.csv"));
        let original_fills = std::fs::read_to_string(&fills_path).expect("read fills");
        let mut fill_lines = original_fills.lines();
        let header = fill_lines.next().expect("fills header");
        let first_fill = fill_lines.next().expect("one fill in fixture");
        let mut columns = first_fill
            .split(',')
            .map(str::to_string)
            .collect::<Vec<_>>();
        columns[3] = if columns[3] == "1" { "2" } else { "1" }.to_string();
        let changed_fills = format!("{header}\n{}\n", columns.join(","));
        std::fs::write(&fills_path, changed_fills).expect("alter fill price");
        let rejected_fill = route(&outcome.body, &paths);
        assert_eq!(
            rejected_fill.status, 503,
            "same-count fill tampering must be rejected against replay events"
        );
        std::fs::write(&fills_path, original_fills).expect("restore fills");

        let trace = paths
            .artifact_root
            .join(run_id)
            .join(format!("{run_id}.events.jsonl"));
        let mut altered = std::fs::read_to_string(&trace).expect("read event trace");
        altered.push_str("{}\n");
        std::fs::write(&trace, altered).expect("alter event trace");
        let rejected = route(&outcome.body, &paths);
        assert_eq!(
            rejected.status, 503,
            "tampered replay trace must fail closed"
        );
    }

    #[test]
    fn verified_research_events_can_be_paged_without_weakening_replay_checks() {
        let (paths, outcome) = backtest_fixture();
        assert_eq!(outcome.status, 200, "backtest fixture must complete");
        let outcome: Value = serde_json::from_str(&outcome.body).expect("backtest outcome JSON");
        let request = |offset: u64, limit: u64| {
            serde_json::json!({
                "outcome": outcome.clone(),
                "event_page": {"offset": offset, "limit": limit}
            })
            .to_string()
        };
        let latest = route(&outcome.to_string(), &paths);
        assert_eq!(latest.status, 200, "latest event page must be readable");
        let latest: Value = serde_json::from_str(&latest.body).expect("latest event page JSON");
        let first = route(&request(0, 1), &paths);
        assert_eq!(
            first.status, 200,
            "first verified event page must be readable"
        );
        let first: Value = serde_json::from_str(&first.body).expect("first event page JSON");
        assert_eq!(first["verification"]["verified"], true);
        assert_eq!(first["events"]["offset"], 0);
        assert_eq!(first["events"]["rows"].as_array().unwrap().len(), 1);
        let total = first["events"]["total"].as_u64().unwrap();
        assert!(total > 1, "fixture should span multiple event pages");

        let last = route(&request(total - 1, 1), &paths);
        assert_eq!(
            last.status, 200,
            "last verified event page must be readable"
        );
        let last: Value = serde_json::from_str(&last.body).expect("last event page JSON");
        assert_eq!(last["events"]["offset"], total - 1);
        assert_eq!(last["events"]["rows"].as_array().unwrap().len(), 1);
        assert_eq!(
            last["events"]["rows"][0]["seq"].as_u64().unwrap(),
            latest["events"]["rows"].as_array().unwrap().last().unwrap()["seq"]
                .as_u64()
                .unwrap(),
            "the event cursor must retain causal log sequence"
        );

        let invalid = route(&request(0, (MAX_EVENT_ROWS as u64) + 1), &paths);
        assert_eq!(
            invalid.status, 400,
            "oversized event pages must be rejected"
        );
    }

    #[test]
    fn strategy_diagnostics_are_identity_bound_before_report_attachment() {
        let (paths, outcome) = backtest_fixture();
        assert_eq!(outcome.status, 200, "backtest fixture must complete");
        let outcome_value: Value = serde_json::from_str(&outcome.body).expect("outcome JSON");
        let summary = read_json(Path::new(
            &outcome_value["artifacts"]["summary"].as_str().unwrap(),
        ))
        .expect("read generated summary");
        let diagnostic = serde_json::json!({
            "schema_version": 1,
            "kind": "qianxing-strategy-bias-diagnostics",
            "strategy": null,
            "worker_kind": "external_jsonl",
            "strategy_id": summary["strategy_id"],
            "strategy_version": summary["strategy_version"],
            "data_fingerprint": summary["data_fingerprint"],
            "instrument": summary["instrument"],
            "as_of": summary["clock_end"],
            "input_bars": summary["bars"],
            "lookahead": {"status": "potential_lookahead_detected", "flagged_checks": 1},
            "visible_bars_at_as_of": summary["bars"],
            "startup": {"status": "startup_sensitive", "windows": []}
        });
        let request = serde_json::json!({
            "outcome": outcome_value,
            "strategy_diagnostics": diagnostic,
        });
        let response = route(&request.to_string(), &paths);
        assert_eq!(response.status, 200, "matching diagnostic should attach");
        let view: Value = serde_json::from_str(&response.body).expect("artifact view JSON");
        assert_eq!(view["strategy_diagnostics"]["identity_bound"], true);
        assert_eq!(
            view["strategy_diagnostics"]["computation"],
            "client_computed_not_reexecuted_by_api"
        );
        assert_eq!(
            view["strategy_diagnostics"]["report"]["lookahead"]["status"],
            "potential_lookahead_detected"
        );

        let mut mismatched: Value = serde_json::from_str(&request.to_string()).unwrap();
        mismatched["strategy_diagnostics"]["data_fingerprint"] = Value::String("other-data".into());
        let rejected = route(&mismatched.to_string(), &paths);
        assert_eq!(
            rejected.status, 409,
            "mismatched diagnostic must be rejected"
        );
    }

    #[test]
    fn diagnostic_batches_are_bounded_before_artifact_replay() {
        let too_many = json!({
            "outcome": {},
            "market_strategy_diagnostics": vec![Value::Null; MAX_MULTI_MARKET_DIAGNOSTICS + 1]
        });
        assert!(
            parse_request(&too_many).is_err(),
            "oversized batch count is rejected before report verification"
        );

        let oversized_payload = json!({
            "outcome": {},
            "market_strategy_diagnostics": [{"padding": "x".repeat(MAX_DIAGNOSTIC_BYTES)}]
        });
        assert!(
            parse_request(&oversized_payload).is_err(),
            "oversized diagnostic bytes are rejected before report verification"
        );

        let mixed_forms = json!({
            "outcome": {},
            "strategy_diagnostics": {},
            "market_strategy_diagnostics": [{}]
        });
        assert!(
            parse_request(&mixed_forms).is_err(),
            "singular and batch forms cannot be supplied together"
        );

        let too_many_accounts = json!({
            "outcome": {},
            "account_strategy_diagnostics": (0..=MAX_MULTI_ACCOUNT_DIAGNOSTICS)
                .map(|index| json!({"account_id": format!("account-{index}"), "strategy_diagnostics": {}}))
                .collect::<Vec<_>>()
        });
        assert!(
            parse_request(&too_many_accounts).is_err(),
            "account diagnostic batch count is rejected before replay"
        );

        let duplicate_accounts = json!({
            "outcome": {},
            "account_strategy_diagnostics": [
                {"account_id": "same-account", "strategy_diagnostics": {}},
                {"account_id": "same-account", "strategy_diagnostics": {}}
            ]
        });
        assert!(
            parse_request(&duplicate_accounts).is_err(),
            "duplicate account diagnostic identities are rejected before replay"
        );

        let mixed_account_forms = json!({
            "outcome": {},
            "account_strategy_diagnostics": [{
                "account_id": "account-one",
                "strategy_diagnostics": {},
                "market_strategy_diagnostics": [{}]
            }]
        });
        assert!(
            parse_request(&mixed_account_forms).is_err(),
            "one account cannot mix singular and multi-market diagnostic forms"
        );
    }

    #[test]
    fn research_artifact_view_refuses_paths_outside_the_configured_root() {
        let (paths, response) = backtest_fixture();
        assert_eq!(response.status, 200, "backtest fixture must complete");
        let mut outcome: Value = serde_json::from_str(&response.body).expect("outcome JSON");
        let outside = paths
            .artifact_root
            .parent()
            .expect("artifact parent")
            .join("outside.csv");
        std::fs::write(&outside, "ts,equity_raw\n1,1\n").expect("create outside file");
        outcome["artifacts"]["equity"] = Value::String(outside.to_string_lossy().into_owned());
        let rejected = route(&outcome.to_string(), &paths);
        assert_eq!(rejected.status, 400, "path escape must be rejected");
    }
}
