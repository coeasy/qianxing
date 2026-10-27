//! Read-only, verified drilldown data for the interactive research report.

use super::{respond, AppPaths};
use qx_app::{
    AppError, AppErrorCategory, BacktestOutcome, CallerCapability, MultiLegBacktestOutcome,
    MultiMarketBacktestOutcome, MultiMarketDepthBacktestOutcome, ResearchReportRowsV1, RunContext,
    VerificationResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

#[path = "research_artifacts/multi_market_reports.rs"]
mod multi_market_reports;
use multi_market_reports::{build as build_multi_market, build_depth as build_multi_market_depth};

#[path = "research_artifacts/multi_leg_reports.rs"]
mod multi_leg_reports;

#[path = "research_artifacts/account_reports.rs"]
mod account_reports;
#[path = "research_artifacts/depth_reports.rs"]
mod depth_reports;
#[path = "research_artifacts/joint_account_reports.rs"]
mod joint_account_reports;
#[path = "research_artifacts/provider_reports.rs"]
mod provider_reports;

const MAX_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ARTIFACT_ROWS: u64 = 5_000_000;
const MAX_FILL_ROWS: usize = 200;
const MAX_EVENT_ROWS: usize = 300;
const MAX_BAR_POINTS: u64 = 1_000;
const MAX_DIAGNOSTIC_BYTES: usize = 1024 * 1024;
const MAX_MULTI_MARKET_DIAGNOSTICS: usize = 512;
const MAX_MULTI_ACCOUNT_DIAGNOSTICS: usize = qx_app::MAX_MULTI_ACCOUNT_RUNS;
const MAX_MULTI_MARKETS: usize = 32;

/// API adapter that resolves opaque application references inside the configured artifact root.
struct RootedArtifactReader {
    root: PathBuf,
}

impl qx_app::artifact_store::ArtifactReader for RootedArtifactReader {
    fn open(&self, reference: &str) -> Result<qx_app::artifact_store::ArtifactStream, AppError> {
        let rooted = rooted_file(reference, &self.root)?;
        let path = Path::new(&rooted);
        let file =
            File::open(path).map_err(|error| AppError::from_io("打开已授权的研究产物", &error))?;
        let metadata = file
            .metadata()
            .map_err(|error| AppError::from_io("读取已授权研究产物元数据", &error))?;
        if !metadata.is_file() {
            return Err(AppError::new(
                AppErrorCategory::DataUnavailable,
                "研究报告产物不是普通文件",
            ));
        }
        if metadata.len() > MAX_ARTIFACT_BYTES {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                format!(
                    "研究报告产物超过明细接口上限 {} MiB",
                    MAX_ARTIFACT_BYTES / 1024 / 1024
                ),
            ));
        }
        Ok(qx_app::artifact_store::ArtifactStream::new(
            metadata.len(),
            BufReader::new(file),
        ))
    }

    fn equivalent_references(&self, left: &str, right: &str) -> Result<bool, AppError> {
        Ok(rooted_file(left, &self.root)? == rooted_file(right, &self.root)?)
    }
}

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
    #[serde(skip)]
    strategy_ids: Vec<String>,
    bars: BarView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pnl_contribution: Option<PnlContributionView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<DepthView>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    exchange_id: Option<String>,
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

pub(super) fn provider_bar_report_route(body: &str, paths: &AppPaths) -> crate::ApiResponse {
    respond(
        provider_reports::read_bar_report(body, paths).and_then(|document| {
            serde_json::to_string(&document).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("Provider Bar 研究报告序列化失败: {error}"),
                )
            })
        }),
    )
}

pub(super) fn provider_depth_report_route(body: &str, paths: &AppPaths) -> crate::ApiResponse {
    respond(
        provider_reports::read_depth_report(body, paths).and_then(|document| {
            serde_json::to_string(&document).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("Provider Depth 研究报告序列化失败: {error}"),
                )
            })
        }),
    )
}

pub(super) fn multi_account_joint_report_route(body: &str, paths: &AppPaths) -> crate::ApiResponse {
    respond(
        joint_account_reports::read_report(body, paths).and_then(|document| {
            serde_json::to_string(&document).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("联合账户研究报告序列化失败: {error}"),
                )
            })
        }),
    )
}

pub(super) fn multi_leg_report_route(body: &str, paths: &AppPaths) -> crate::ApiResponse {
    respond(
        multi_leg_reports::read_report(body, paths).and_then(|document| {
            serde_json::to_string(&document).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("多腿研究报告序列化失败: {error}"),
                )
            })
        }),
    )
}

fn build(body: &str, paths: &AppPaths) -> Result<ResearchArtifactView, AppError> {
    build_with_preverified_core(body, paths, None)
}

fn build_with_preverified_core(
    body: &str,
    paths: &AppPaths,
    mut preverified_core: Option<qx_app::ResearchRunReportV1>,
) -> Result<ResearchArtifactView, AppError> {
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
        joint_account_options,
    ) = parse_request(&request)?;
    let account_strategy_diagnostics = request
        .get("account_strategy_diagnostics")
        .filter(|value| !value.is_null());
    if let Some(view) = provider_reports::build(&request, &outcome_value, paths)? {
        return Ok(view);
    }
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
    if joint_account_reports::is_joint_account_outcome(&outcome_value) {
        if strategy_diagnostics.is_some()
            || market_strategy_diagnostics.is_some()
            || account_strategy_diagnostics.is_some()
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "多交易所联合账户报告暂不接受外部策略诊断附件",
            ));
        }
        if event_page.is_some() || account_event_offsets.is_some() || event_instrument.is_some() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "联合账户不支持合并事件游标；请使用 joint_account_event_pages 分别指定交易所账户页",
            ));
        }
        return joint_account_reports::build(
            &outcome_value,
            paths,
            &joint_account_options.unwrap_or_default(),
        );
    }
    if joint_account_options.is_some() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "joint_account_event_pages 仅适用于多交易所联合账户报告",
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
        return depth_reports::build(outcome_value, paths, event_page, preverified_core.take());
    }
    if outcome_value.get("batch_id").is_some() && outcome_value.get("runs").is_some() {
        if strategy_diagnostics.is_some() || market_strategy_diagnostics.is_some() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "多账户报告须通过 account_strategy_diagnostics 按账户身份绑定策略诊断",
            ));
        }
        return account_reports::build(
            outcome_value,
            paths,
            event_page,
            account_event_offsets.as_ref(),
            event_instrument.as_deref(),
            account_strategy_diagnostics,
        );
    }
    if outcome_value.get("attribution_artifact").is_some() {
        return multi_leg_reports::build(
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
    let artifact_reader = RootedArtifactReader { root: root.clone() };
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

    let declared_summary =
        read_json_with_reader(&artifact_reader, &outcome.artifacts.summary, "研究摘要")?;
    ensure_declared_artifact_paths(&declared_summary, &root)?;
    let core = if let Some(core) = preverified_core {
        if core.run_id != outcome.run_id || core.instrument != outcome.instrument {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                "预复核 Bar 报告与子运行身份不一致",
            ));
        }
        core
    } else {
        qx_app::read_run_report_v1_with_reader(
            &outcome,
            &RunContext::new(CallerCapability::Research, outcome.run_id.clone()),
            &artifact_reader,
        )?
    };
    if core.summary != declared_summary {
        return Err(AppError::new(
            AppErrorCategory::StorageFailure,
            "研究摘要在路径校验与公共报告读取之间发生变化",
        ));
    }
    let summary = core.summary;
    let mut verification = core.verification;
    if !verification.verified {
        if let Some(mismatch) = verification.mismatches.iter().find(|mismatch| {
            mismatch.starts_with("RunRecord SHA-256 复核失败")
                || mismatch.starts_with("RunManifest 身份复核失败")
        }) {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                format!("研究报告产物完整性复核失败: {mismatch}"),
            ));
        }
        return Ok(ResearchArtifactView {
            schema_version: 1,
            run_kind: None,
            run_id: outcome.run_id,
            instrument: outcome.instrument,
            verification,
            summary,
            equity: into_csv_view(core.equity),
            fills: into_csv_view(core.fills),
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
    let strategy_diagnostics = strategy_diagnostics
        .map(|report| bind_strategy_diagnostics(report, &summary))
        .transpose()?;
    let equity = into_csv_view(core.equity);
    let (mut events, event_fills) = event_view(
        &artifact_reader,
        event_trace,
        &summary,
        &outcome.artifacts.run_manifest,
        event_page,
    )?;
    let replay_checked_fills =
        validated_fill_view(&artifact_reader, &outcome.artifacts.fills, event_fills)?;
    let fills = into_csv_view(core.fills);
    if fills.total != replay_checked_fills.total || fills.rows != replay_checked_fills.rows {
        return Err(AppError::new(
            AppErrorCategory::StorageFailure,
            "公共报告成交样本与 replay-verified 成交产物不一致",
        ));
    }
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

fn invalid_report(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::Conflict, message)
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
    let mut run_record_path = None;
    for field in ["run_manifest", "run_record"] {
        let Some(path) = summary.get(field).and_then(Value::as_str) else {
            continue;
        };
        let rooted = rooted_file(path, artifact_root).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("摘要声明的 {field} 必须位于服务配置的产物根目录内: {error}"),
            )
        })?;
        check_artifact_size(Path::new(&rooted))?;
        if field == "run_record" {
            run_record_path = Some(rooted);
        }
    }
    if let Some(record_path) = run_record_path {
        let record = read_json(Path::new(&record_path))?;
        if record.get("schema_version").and_then(Value::as_u64) == Some(2) {
            let locations = summary
                .get("artifact_locations")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    AppError::new(
                        AppErrorCategory::InvalidInput,
                        "RunRecord V2 摘要缺少 artifact_locations",
                    )
                })?;
            for name in ["summary", "equity", "fills", "run_manifest"] {
                let path = locations.get(name).and_then(Value::as_str).ok_or_else(|| {
                    AppError::new(
                        AppErrorCategory::InvalidInput,
                        format!("RunRecord V2 摘要缺少 {name} 存储定位"),
                    )
                })?;
                let rooted = rooted_file(path, artifact_root).map_err(|error| {
                    AppError::new(
                        AppErrorCategory::InvalidInput,
                        format!(
                            "RunRecord V2 的 {name} 工件必须位于服务配置的产物根目录内: {error}"
                        ),
                    )
                })?;
                check_artifact_size(Path::new(&rooted))?;
            }
            return Ok(());
        }
        let Some(references) = record.get("artifact_refs").and_then(Value::as_array) else {
            // qx-app's verifier will report a malformed or legacy record. There are no
            // referenced paths to dereference before that verdict.
            return Ok(());
        };
        for reference in references {
            let Some(path) = reference.get("path").and_then(Value::as_str) else {
                continue;
            };
            let rooted = rooted_file(path, artifact_root).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("RunRecord 引用产物必须位于服务配置的产物根目录内: {error}"),
                )
            })?;
            check_artifact_size(Path::new(&rooted))?;
        }
    }
    Ok(())
}

fn empty_depth(source_status: &'static str) -> DepthView {
    DepthView {
        source_status,
        total: 0,
        truncated: false,
        rows: Vec::new(),
    }
}

fn depth_fill_view(
    artifact_reader: &dyn qx_app::artifact_store::ArtifactReader,
    reference: &str,
) -> Result<CsvView, AppError> {
    let (headers, total) = csv_shape(artifact_reader, reference)?;
    for field in ["ts", "order_id", "qty_raw", "price_raw", "fee_raw"] {
        column(&headers, &[field])?;
    }
    let mut artifact = open_bounded_artifact(artifact_reader, reference)?;
    let mut bytes_seen = 0_u64;
    let _header = read_artifact_line(artifact.reader.as_mut(), reference, &mut bytes_seen)?
        .ok_or_else(|| malformed_csv("深度成交 CSV 为空"))?;
    let required = ["ts", "order_id", "qty_raw", "price_raw", "fee_raw"]
        .iter()
        .map(|field| column(&headers, &[*field]))
        .collect::<Result<Vec<_>, _>>()?;
    let mut rows = Vec::with_capacity(MAX_FILL_ROWS.min(total as usize));
    let mut seen = 0_u64;
    loop {
        let Some(line) = read_artifact_line(artifact.reader.as_mut(), reference, &mut bytes_seen)?
        else {
            break;
        };
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

type ParsedResearchRequest<'a> = (
    &'a Value,
    Option<&'a Value>,
    Option<&'a Value>,
    Option<EventPage>,
    Option<BTreeMap<String, u64>>,
    Option<String>,
    Option<qx_app::MultiAccountJointReportReadOptionsV1>,
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
                    | "joint_account_event_pages"
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
        let joint_account_options = object
            .get("joint_account_event_pages")
            .map(|pages| {
                serde_json::from_value(serde_json::json!({
                    "joint_account_event_pages": pages
                }))
                .map_err(|error| {
                    AppError::new(
                        AppErrorCategory::InvalidInput,
                        format!("joint_account_event_pages 结构无效: {error}"),
                    )
                })
            })
            .transpose()?;
        Ok((
            outcome,
            strategy_diagnostics,
            market_strategy_diagnostics,
            event_page,
            account_event_offsets,
            event_instrument,
            joint_account_options,
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
        Ok((request, None, None, None, None, None, None))
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
    if object.len() > qx_app::MAX_MULTI_ACCOUNT_RUNS {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!(
                "account_event_offsets 最多支持 {} 个账户",
                qx_app::MAX_MULTI_ACCOUNT_RUNS
            ),
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

fn into_csv_view(rows: ResearchReportRowsV1) -> CsvView {
    CsvView {
        total: rows.total,
        truncated: rows.truncated,
        rows: rows.rows,
    }
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

fn read_json_with_reader(
    reader: &dyn qx_app::artifact_store::ArtifactReader,
    reference: &str,
    label: &str,
) -> Result<Value, AppError> {
    read_json_with_reader_limit(reader, reference, label, MAX_ARTIFACT_BYTES)
}

fn report_artifact_too_large(reference: &str, byte_limit: u64) -> AppError {
    AppError::new(
        AppErrorCategory::Timeout,
        format!("研究产物 {reference} 超过读取大小上限 {byte_limit} bytes"),
    )
}

fn read_json_with_reader_limit(
    reader: &dyn qx_app::artifact_store::ArtifactReader,
    reference: &str,
    label: &str,
    byte_limit: u64,
) -> Result<Value, AppError> {
    let artifact = reader.open(reference)?;
    if artifact.byte_len > byte_limit {
        return Err(report_artifact_too_large(reference, byte_limit));
    }

    // Parse directly from the bounded stream. Reading the full UTF-8 body first doubled peak
    // memory for large multi-market artifacts because serde_json then built a second tree.
    let mut bounded = artifact.reader.take(byte_limit.saturating_add(1));
    let parsed = {
        let mut deserializer = serde_json::Deserializer::from_reader(&mut bounded);
        match Value::deserialize(&mut deserializer) {
            Ok(value) => deserializer.end().map(|()| value),
            Err(error) => Err(error),
        }
    };
    let bytes_read = byte_limit.saturating_add(1).saturating_sub(bounded.limit());
    if bytes_read > byte_limit {
        return Err(report_artifact_too_large(reference, byte_limit));
    }

    parsed.map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("{label} {reference} 格式损坏: {error}"),
        )
    })
}

fn event_view(
    reader: &dyn qx_app::artifact_store::ArtifactReader,
    reference: &str,
    summary: &Value,
    manifest_reference: &str,
    requested_page: Option<EventPage>,
) -> Result<(EventView, BTreeMap<qx_app::EventFillIdentityV1, usize>), AppError> {
    let report = qx_app::read_event_report_v1_with_reader(
        reference,
        summary,
        manifest_reference,
        requested_page.map(|page| (page.offset, page.limit)),
        reader,
    )?;
    let fill_counts = report
        .fill_counts
        .into_iter()
        .map(|fill| (fill.identity, fill.count))
        .collect();
    Ok((
        EventView {
            total: report.total,
            offset: report.offset,
            truncated: report.truncated,
            rows: report.rows,
        },
        fill_counts,
    ))
}

fn open_bounded_artifact(
    reader: &dyn qx_app::artifact_store::ArtifactReader,
    reference: &str,
) -> Result<qx_app::artifact_store::ArtifactStream, AppError> {
    let artifact = reader.open(reference)?;
    if artifact.byte_len > MAX_ARTIFACT_BYTES {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            format!(
                "研究报告产物超过明细接口上限 {} MiB",
                MAX_ARTIFACT_BYTES / 1024 / 1024
            ),
        ));
    }
    Ok(artifact)
}

fn read_artifact_line(
    reader: &mut dyn BufRead,
    reference: &str,
    bytes_seen: &mut u64,
) -> Result<Option<String>, AppError> {
    let Some((line, line_bytes)) = qx_app::artifact_store::read_bounded_line(reader, reference)?
    else {
        return Ok(None);
    };
    *bytes_seen = bytes_seen.saturating_add(line_bytes as u64);
    if *bytes_seen > MAX_ARTIFACT_BYTES {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            format!(
                "研究报告产物超过明细接口上限 {} MiB",
                MAX_ARTIFACT_BYTES / 1024 / 1024
            ),
        ));
    }
    Ok(Some(line))
}

fn validated_fill_view(
    reader: &dyn qx_app::artifact_store::ArtifactReader,
    reference: &str,
    mut expected: BTreeMap<qx_app::EventFillIdentityV1, usize>,
) -> Result<CsvView, AppError> {
    let (headers, total) = csv_shape(reader, reference)?;
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
    let mut artifact = open_bounded_artifact(reader, reference)?;
    let mut bytes_seen = 0_u64;
    let _header = read_artifact_line(artifact.reader.as_mut(), reference, &mut bytes_seen)?
        .ok_or_else(|| malformed_csv("CSV 产物为空"))?;
    let mut rows = Vec::with_capacity(MAX_FILL_ROWS.min(total as usize));
    let mut seen = 0_u64;
    loop {
        let Some(line) = read_artifact_line(artifact.reader.as_mut(), reference, &mut bytes_seen)?
        else {
            break;
        };
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
        let key = qx_app::EventFillIdentityV1 {
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
        seen = seen.saturating_add(1);
        if rows.len() < MAX_FILL_ROWS {
            rows.push(csv_object(&headers, values)?);
        }
    }
    if seen != total || !expected.is_empty() {
        return Err(malformed_csv(
            "fills.csv 读取期间行数变化或 replay-verified Filled 事件未全部写入",
        ));
    }
    Ok(CsvView {
        total,
        truncated: rows.len() < total as usize,
        rows,
    })
}

fn csv_shape(
    artifact_reader: &dyn qx_app::artifact_store::ArtifactReader,
    reference: &str,
) -> Result<(Vec<String>, u64), AppError> {
    let mut artifact = open_bounded_artifact(artifact_reader, reference)?;
    let mut bytes_seen = 0_u64;
    let header = read_artifact_line(artifact.reader.as_mut(), reference, &mut bytes_seen)?
        .ok_or_else(|| malformed_csv("CSV 产物为空"))?;
    let headers = csv_record(header.trim_end())?;
    if headers.is_empty() || headers.iter().any(|header| header.is_empty()) {
        return Err(malformed_csv("CSV 表头为空或存在空字段"));
    }
    let mut total = 0u64;
    loop {
        let Some(line) = read_artifact_line(artifact.reader.as_mut(), reference, &mut bytes_seen)?
        else {
            break;
        };
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

fn malformed_csv(message: &str) -> AppError {
    AppError::new(AppErrorCategory::StorageFailure, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_surface::{self, AppPaths};
    use std::io::Cursor;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct InlineArtifactReader {
        bytes: Vec<u8>,
        reported_len: u64,
    }

    impl qx_app::artifact_store::ArtifactReader for InlineArtifactReader {
        fn open(
            &self,
            _reference: &str,
        ) -> Result<qx_app::artifact_store::ArtifactStream, AppError> {
            Ok(qx_app::artifact_store::ArtifactStream::new(
                self.reported_len,
                Cursor::new(self.bytes.clone()),
            ))
        }
    }

    #[test]
    fn bounded_json_reader_parses_without_materializing_a_text_copy() {
        let reader = InlineArtifactReader {
            bytes: br#"{"ok":true,"count":3}"#.to_vec(),
            reported_len: 21,
        };
        let parsed = read_json_with_reader_limit(&reader, "inline.json", "fixture", 64)
            .expect("parse bounded JSON stream");
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["count"], 3);
    }

    #[test]
    fn bounded_json_reader_rejects_actual_stream_growth_and_invalid_json() {
        let grown = InlineArtifactReader {
            bytes: b"{}     ".to_vec(),
            reported_len: 2,
        };
        assert!(
            read_json_with_reader_limit(&grown, "grown.json", "fixture", 4)
                .expect_err("actual bytes beyond limit must fail")
                .to_string()
                .contains("超过读取大小上限")
        );

        let malformed = InlineArtifactReader {
            bytes: b"{broken}".to_vec(),
            reported_len: 8,
        };
        assert!(
            read_json_with_reader_limit(&malformed, "broken.json", "fixture", 64)
                .expect_err("malformed JSON must fail")
                .to_string()
                .contains("格式损坏")
        );
    }

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

    fn multi_account_joint_fixture() -> (AppPaths, crate::ApiResponse) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join(format!(
                "qx-api-joint-report-{}-{unique}",
                std::process::id()
            ));
        let data_root = workspace.join("data");
        let artifact_root = workspace.join("artifacts");
        std::fs::create_dir_all(&data_root).expect("create joint report fixture data root");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy/qianxing.bar-frame.example.json");
        let template = std::fs::read_to_string(fixture).expect("read joint report BarFrame");
        let members = [("okx", "BTC-USDT.OKX"), ("bitget", "PEPEUSDT.BITGET")]
            .into_iter()
            .enumerate()
            .map(|(index, (exchange_id, instrument))| {
                let bars_path = data_root.join(format!("venue-{index}.json"));
                std::fs::write(&bars_path, template.replace("BTCUSDT.BINANCE", instrument))
                    .expect("write venue BarFrame");
                json!({
                    "exchange_id": exchange_id,
                    "account_id": "primary",
                    "initial_cash_raw": 100_000_000_000_000_i128,
                    "markets": [{
                        "instrument": instrument,
                        "bars_path": bars_path,
                        "priority": 0,
                        "market_spec": null,
                        "fee_schedule": null,
                        "strategies": [{
                            "priority": 0,
                            "reserved_capital_raw": 20_000_000_000_000_i128,
                            "strategy": {
                                "kind": "sma_cross",
                                "strategy_id": format!("joint-api-{index}"),
                                "quantity_raw": qx_core::SCALE,
                                "fast_window": 2,
                                "slow_window": 3
                            }
                        }]
                    }]
                })
            })
            .collect::<Vec<_>>();
        let paths = AppPaths {
            data_root,
            artifact_root,
        };
        let spec = json!({
            "schema_version": 1,
            "run_id": format!("api-joint-report-{unique}"),
            "settlement_currency": "USDT",
            "seed": 19,
            "output_dir": "client-path-is-overridden",
            "accounts": members
        });
        let response = app_surface::multi_account_joint(&spec.to_string(), &paths);
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
    fn multi_leg_versioned_report_http_matches_qx_app_and_enforces_roots() {
        let (paths, outcome) = multi_leg_fixture();
        assert_eq!(outcome.status, 200, "multi-leg fixture must complete");
        let outcome_value: Value = serde_json::from_str(&outcome.body).expect("outcome JSON");
        let request = json!({"schema_version": 1, "outcome": outcome_value});
        let response = crate::app_surface::route_app_run(
            "POST",
            "/app/read-multi-leg-report",
            "",
            &request.to_string(),
            &paths,
            &crate::app_surface::app_runs::AppRunRegistry::default(),
            None,
        )
        .expect("multi-leg report route is registered");
        assert_eq!(
            response.status, 200,
            "versioned multi-leg report must use rooted qx-app reader: {}",
            response.body
        );
        let actual: Value = serde_json::from_str(&response.body).expect("versioned report JSON");
        let request_dto: qx_app::MultiLegResearchReportReadRequestV1 =
            serde_json::from_value(request.clone()).expect("multi-leg report request DTO");
        let expected = qx_app::read_multi_leg_research_report_v1(
            &request_dto,
            &RunContext::new(
                CallerCapability::Research,
                request_dto.outcome.run_id.clone(),
            ),
        )
        .expect("native qx-app multi-leg report reader");
        assert_eq!(
            actual,
            serde_json::to_value(expected).expect("serialize qx-app report"),
            "HTTP returns the same complete versioned DTO as qx-app"
        );

        let mut escaped = request;
        escaped["outcome"]["attribution_artifact"] = json!(paths
            .artifact_root
            .parent()
            .unwrap()
            .join("outside-attribution.json"));
        let rejected = crate::app_surface::route_app_run(
            "POST",
            "/app/read-multi-leg-report",
            "",
            &escaped.to_string(),
            &paths,
            &crate::app_surface::app_runs::AppRunRegistry::default(),
            None,
        )
        .expect("multi-leg report route is registered");
        assert_ne!(
            rejected.status, 200,
            "attribution artifacts outside artifact root must be rejected"
        );
    }

    #[test]
    fn verified_multi_leg_report_replays_shared_account_and_exposes_both_leg_timelines() {
        let (paths, outcome) = multi_leg_fixture();
        assert_eq!(outcome.status, 200, "multi-leg fixture must complete");
        let outcome_value: Value =
            serde_json::from_str(&outcome.body).expect("multi-leg outcome JSON");
        let typed_outcome =
            MultiLegBacktestOutcome::from_json(&outcome.body).expect("typed multi-leg outcome");
        let shared_facts = qx_app::read_multi_leg_report_facts_v1(
            &typed_outcome,
            &RunContext::new(CallerCapability::Research, typed_outcome.run_id.clone()),
        )
        .expect("qx-app reads the same persisted multi-leg report facts");
        assert_eq!(shared_facts.schema_version, 1);
        assert_eq!(
            shared_facts.total_fills,
            typed_outcome.primary_fills + typed_outcome.reference_fills
        );
        assert_eq!(shared_facts.event_hash.len(), 16);
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
    fn multi_leg_report_rejects_artifact_strategy_identity_drift_against_run_manifest() {
        let (paths, outcome) = multi_leg_fixture();
        assert_eq!(outcome.status, 200, "multi-leg fixture must complete");
        let outcome_value: Value = serde_json::from_str(&outcome.body).expect("outcome JSON");
        let artifact_path = outcome_value["attribution_artifact"]
            .as_str()
            .expect("multi-leg artifact path");
        let mut artifact: Value =
            serde_json::from_slice(&std::fs::read(artifact_path).expect("read multi-leg artifact"))
                .expect("parse multi-leg artifact");
        artifact["strategy_version"] = json!("tampered-strategy-version");
        std::fs::write(
            artifact_path,
            serde_json::to_vec(&artifact).expect("encode tampered artifact"),
        )
        .expect("write tampered artifact");

        let response = route(&json!({"outcome": outcome_value}).to_string(), &paths);
        assert_eq!(
            response.status, 409,
            "multi-leg strategy identity drift must fail RunManifest verification"
        );
    }

    #[test]
    fn multi_leg_report_rejects_tampered_per_leg_return_facts() {
        let (paths, outcome) = multi_leg_fixture();
        assert_eq!(outcome.status, 200, "multi-leg fixture must complete");
        let outcome_value: Value = serde_json::from_str(&outcome.body).expect("outcome JSON");
        let artifact_path = outcome_value["attribution_artifact"]
            .as_str()
            .expect("multi-leg artifact path");
        let mut artifact: Value =
            serde_json::from_slice(&std::fs::read(artifact_path).expect("read multi-leg artifact"))
                .expect("parse multi-leg artifact");
        let declared_return = artifact["primary_return_bps"]
            .as_i64()
            .expect("primary return bps");
        artifact["primary_return_bps"] = json!(declared_return + 1);
        std::fs::write(
            artifact_path,
            serde_json::to_vec(&artifact).expect("encode tampered artifact"),
        )
        .expect("write tampered artifact");

        let response = route(&json!({"outcome": outcome_value}).to_string(), &paths);
        assert_eq!(response.status, 409);
        assert!(response.body.contains("逐腿收益"), "{}", response.body);
    }

    #[test]
    fn multi_leg_report_rejects_effective_market_rule_drift_against_run_manifest() {
        let (paths, outcome) = multi_leg_fixture();
        assert_eq!(outcome.status, 200, "multi-leg fixture must complete");
        let outcome_value: Value = serde_json::from_str(&outcome.body).expect("outcome JSON");
        let artifact_path = outcome_value["attribution_artifact"]
            .as_str()
            .expect("multi-leg artifact path");
        let mut artifact: Value =
            serde_json::from_slice(&std::fs::read(artifact_path).expect("read artifact"))
                .expect("parse artifact");
        artifact["effective_market_specs"]["primary"]["price_tick"] = json!(7);
        std::fs::write(
            artifact_path,
            serde_json::to_vec(&artifact).expect("encode tampered artifact"),
        )
        .expect("write tampered artifact");

        let response = route(&json!({"outcome": outcome_value}).to_string(), &paths);
        assert_eq!(
            response.status, 409,
            "effective market rules must remain bound to the canonical RunManifest"
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
                .is_some_and(|rows| !rows.is_empty() && rows.len() <= 1_000)
                && market["pnl_contribution"]["currency"] == "USDT"
        }));
        assert!(view["summary"]["execution_assumptions"]["markets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|market| market["funding_events"]["rows"].as_array().unwrap().len() <= 100));
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

        let first_manifest_path = paths.data_root.join("depth-0.manifest.json");
        let original_manifest =
            std::fs::read(&first_manifest_path).expect("read original depth quality manifest");
        let mut changed_manifest: Value =
            serde_json::from_slice(&original_manifest).expect("parse depth quality manifest");
        changed_manifest["quality_report"]["row_count"] = json!(3);
        std::fs::write(
            &first_manifest_path,
            serde_json::to_vec(&changed_manifest).unwrap(),
        )
        .expect("tamper depth quality manifest");
        let rejected_quality = route(&outcome.body, &paths);
        assert_ne!(
            rejected_quality.status, 200,
            "changed DatasetManifestV2 must fail depth quality verification"
        );
        std::fs::write(&first_manifest_path, original_manifest)
            .expect("restore depth quality manifest");

        let artifact_path = outcome_value["artifact_path"]
            .as_str()
            .expect("artifact path");
        let artifact: Value = read_json(Path::new(artifact_path)).expect("artifact JSON");
        let event_trace_path = artifact["event_trace_path"]
            .as_str()
            .expect("external event trace path");
        let event_trace = std::fs::read_to_string(event_trace_path).expect("read event trace");
        let mut lines = event_trace.lines();
        let mut event: Value =
            serde_json::from_str(lines.next().expect("first event")).expect("first event JSON");
        event["ts"] = json!(event["ts"].as_u64().unwrap_or_default() + 1);
        let mut altered_trace = serde_json::to_string(&event).expect("serialize altered event");
        for line in lines {
            altered_trace.push('\n');
            altered_trace.push_str(line);
        }
        std::fs::write(event_trace_path, altered_trace).expect("tamper event log");
        let rejected = route(&outcome.body, &paths);
        assert_ne!(
            rejected.status, 200,
            "changed shared event stream must fail digest/replay verification"
        );
    }

    #[test]
    fn verified_joint_multi_account_report_uses_rooted_shared_reader() {
        let (paths, outcome) = multi_account_joint_fixture();
        assert_eq!(outcome.status, 200, "joint account fixture must complete");
        let versioned = crate::app_surface::route_app_run(
            "POST",
            "/app/read-multi-account-joint-report",
            "",
            &outcome.body,
            &paths,
            &crate::app_surface::app_runs::AppRunRegistry::default(),
            None,
        )
        .expect("joint report route is registered");
        assert_eq!(
            versioned.status, 200,
            "versioned joint report must use the rooted qx-app reader: {}",
            versioned.body
        );
        let versioned_value: Value =
            serde_json::from_str(&versioned.body).expect("versioned joint report JSON");
        let outcome_dto: qx_app::MultiAccountJointBacktestOutcome =
            serde_json::from_str(&outcome.body).expect("joint outcome DTO");
        let expected = qx_app::read_multi_account_joint_report_v1(
            &outcome_dto,
            &RunContext::new(CallerCapability::Research, &outcome_dto.run_id),
        )
        .expect("native verified joint report");
        assert_eq!(
            versioned_value,
            serde_json::to_value(expected).expect("serialize native joint report"),
            "HTTP returns the same core DTO as qx-app"
        );

        let mut escaped: Value = serde_json::from_str(&outcome.body).expect("joint outcome JSON");
        escaped["artifact_path"] =
            json!(paths.artifact_root.parent().unwrap().join("outside.json"));
        let rejected = crate::app_surface::route_app_run(
            "POST",
            "/app/read-multi-account-joint-report",
            "",
            &escaped.to_string(),
            &paths,
            &crate::app_surface::app_runs::AppRunRegistry::default(),
            None,
        )
        .expect("joint report route is registered");
        assert_ne!(
            rejected.status, 200,
            "artifact-root escapes must be rejected"
        );

        let response = route(&outcome.body, &paths);
        assert_eq!(
            response.status, 200,
            "joint report must use rooted artifact verification: {}",
            response.body
        );
        let view: Value = serde_json::from_str(&response.body).expect("joint report JSON");
        assert_eq!(view["schema_version"], 13);
        assert_eq!(view["run_kind"], "multi_account_joint");
        assert_eq!(view["verification"]["verified"], true);
        assert_eq!(view["summary"]["accounts_total"], 2);
        assert_eq!(view["summary"]["accounts_succeeded"], 2);
        assert_eq!(view["accounts"].as_array().unwrap().len(), 2);
        let exchange_ids = view["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|account| account["exchange_id"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            exchange_ids,
            std::collections::BTreeSet::from(["bitget", "okx"])
        );
        assert!(view["accounts"].as_array().unwrap().iter().all(|account| {
            account["account_id"] == "primary"
                && account["status"] == "succeeded"
                && account["report"]["verification"]["verified"] == true
                && account["report"]["events"]["rows"]
                    .as_array()
                    .is_some_and(|rows| !rows.is_empty())
        }));
        assert_eq!(
            view["events"]["total"], 0,
            "the API does not invent one global causal order across independent venue logs"
        );
        assert_eq!(view["event_trace_available"], false);

        let original: Value = serde_json::from_str(&outcome.body).expect("joint outcome JSON");
        let first_account_page = route(
            &json!({
                "outcome": original.clone(),
                "joint_account_event_pages": {
                    "okx": {"primary": {"offset": 0, "limit": 1}}
                }
            })
            .to_string(),
            &paths,
        );
        assert_eq!(
            first_account_page.status, 200,
            "per-account event page should verify"
        );
        let first_page_view: Value =
            serde_json::from_str(&first_account_page.body).expect("joint first page JSON");
        assert_eq!(first_page_view["schema_version"], 13);
        let okx_first_page = first_page_view["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|account| account["exchange_id"] == "okx")
            .unwrap();
        assert_eq!(okx_first_page["report"]["events"]["offset"], 0);
        assert_eq!(okx_first_page["report"]["events"]["limit"], 1);
        assert_eq!(
            okx_first_page["report"]["events"]["rows"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(okx_first_page["report"]["events"]["next_offset"], 1);

        let second_account_page = route(
            &json!({
                "outcome": original.clone(),
                "joint_account_event_pages": {
                    "okx": {"primary": {"offset": 1, "limit": 1}}
                }
            })
            .to_string(),
            &paths,
        );
        assert_eq!(second_account_page.status, 200);
        let second_page_view: Value =
            serde_json::from_str(&second_account_page.body).expect("joint second page JSON");
        let okx_second_page = second_page_view["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|account| account["exchange_id"] == "okx")
            .unwrap();
        assert_eq!(okx_second_page["report"]["events"]["offset"], 1);
        assert_eq!(okx_second_page["report"]["events"]["previous_offset"], 0);
        assert_ne!(
            okx_first_page["report"]["events"]["rows"], okx_second_page["report"]["events"]["rows"],
            "adjacent pages must not repeat the same source rows"
        );

        let unknown_joint_account_page = route(
            &json!({
                "outcome": original.clone(),
                "joint_account_event_pages": {
                    "unknown-exchange": {"primary": {"offset": 0, "limit": 1}}
                }
            })
            .to_string(),
            &paths,
        );
        assert_eq!(unknown_joint_account_page.status, 400);

        let paged = route(
            &json!({
                "outcome": original.clone(),
                "event_page": {"offset": 0, "limit": 1}
            })
            .to_string(),
            &paths,
        );
        assert_eq!(
            paged.status, 400,
            "unsupported global joint event paging is explicit"
        );

        let equity_path = PathBuf::from(
            original["equity_path"]
                .as_str()
                .expect("joint aggregate equity path"),
        );
        let equity_original = std::fs::read_to_string(&equity_path).expect("read joint equity");
        let mut equity: Value = serde_json::from_str(&equity_original).expect("joint equity JSON");
        equity[0][1] = json!(0);
        std::fs::write(
            &equity_path,
            serde_json::to_vec(&equity).expect("encode altered equity"),
        )
        .expect("tamper joint equity");
        let rejected = route(&outcome.body, &paths);
        std::fs::write(&equity_path, equity_original).expect("restore joint equity");
        assert_ne!(
            rejected.status, 200,
            "tampered aggregate equity must be rejected"
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
        let mut tampered_parent = batch.clone();
        tampered_parent["run_manifest"]["config_hash"] = json!("0000000000000000");
        let rejected_parent = route(&tampered_parent.to_string(), &paths);
        assert_ne!(
            rejected_parent.status, 200,
            "multi-account Bar report must reject a tampered parent RunManifest"
        );

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

        let mut all_failed: Value = serde_json::from_str(&outcome.body).expect("depth result");
        all_failed["run_manifest"] = Value::Null;
        all_failed["summary"] = json!({
            "accounts_total": 2,
            "accounts_succeeded": 0,
            "accounts_failed": 2,
            "successful_initial_cash_raw": 0,
            "successful_fills": 0,
            "weighted_return_bps": null,
            "complete": false
        });
        for run in all_failed["runs"].as_array_mut().expect("depth accounts") {
            run["outcome"] = Value::Null;
            run["error_json"] = Value::String(json!({"category": "data_unavailable"}).to_string());
        }
        let failed_response = route(&all_failed.to_string(), &paths);
        assert_eq!(
            failed_response.status, 200,
            "an all-failed v2 depth batch retains its result_kind discriminator"
        );
        let failed_view: Value =
            serde_json::from_str(&failed_response.body).expect("all-failed depth view");
        assert!(failed_view["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|account| account["status"] == "failed"));
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
            "same-count fill tampering must be rejected against replay events: {}",
            rejected_fill.body
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

    #[test]
    fn research_artifact_view_roots_run_record_references_before_rust_readback() {
        let (paths, response) = backtest_fixture();
        assert_eq!(response.status, 200, "backtest fixture must complete");
        let outcome: Value = serde_json::from_str(&response.body).expect("outcome JSON");
        let summary_path = PathBuf::from(
            outcome["artifacts"]["summary"]
                .as_str()
                .expect("summary path"),
        );
        let mut summary: Value = read_json(&summary_path).expect("summary JSON");
        let outside = paths
            .artifact_root
            .parent()
            .expect("artifact parent")
            .join("outside-record-reference.csv");
        std::fs::write(&outside, "outside").expect("create outside file");
        summary["artifact_locations"]["equity"] =
            Value::String(outside.to_string_lossy().into_owned());

        ensure_declared_artifact_paths(&summary, &paths.artifact_root)
            .expect_err("RunRecord V2 locations must not escape the configured artifact root");
    }
}
