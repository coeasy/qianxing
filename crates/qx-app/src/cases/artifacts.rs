//! 产物读写：四份核心产物和可选事件追溯 sidecar 的落点、格式与**读回**（T2-2）。
//!
//! 产物是应用层对外的**第二张脸**（第一张是 [`crate::spec`] 里的 JSON 类型）。它必须满足
//! 两件事：三个入口写出**逐字节相同**的四份核心文件；事件追溯 sidecar 与核心文件一起发布，任何产物都能被独立读回来复核
//! （[`crate::cases::verify_run`] 就是那个读回者）。
//!
//! ## 四份核心产物
//!
//! | 文件 | 内容 | 复核时看什么 |
//! |---|---|---|
//! | `<run_id>.run.json` | `qx_core::RunManifest` 的 JSON | `result_hash` / `data_fingerprint` / `config_hash` |
//! | `<run_id>.summary.json` | [`RunSummary`] | 与 manifest 的同一组哈希逐字相等 |
//! | `<run_id>.equity.csv` | `ts,equity_raw,position_raw` | 行数 == `equity_points`；末行权益 == `final_equity_raw` |
//! | `<run_id>.fills.csv` | fills CSV v2：原始字段与精确数量、价格、多币种费用 | 行数 == `fills` |
//!
//! Bar 与深度回测还会发布 JSONL 事件 sidecar：完整重放事件流，摘要中的 digest 和事件数可用于校验；
//! `BacktestArtifacts.event_trace` 暴露其路径。它与内容寻址 RunManifest 同身份发布。
//!
//! `RunManifest` 直接复用 `qx-core` 的那一份——**不**另造一份同形清单：它是全仓对"这次运行
//! 是谁、拿什么跑的、结果是什么"的唯一说法，应用层再写一份等于给同一件事发两张身份证。
//!
//! ## 为什么 `summary` 不是 manifest 的重复
//!
//! manifest 是**身份**（谁跑的、什么输入、什么哈希），summary 是**读数**（成交几笔、收益几个
//! 基点、末值多少、用了哪套费率与风控）。把读数塞进 manifest 会让身份文件随读数漂移，
//! 而身份文件一变，离线复算就没法拿它当锚点。两张脸，两件事。
//!
//! 同一条尺子也裁掉了一个**看起来该有**的字段：`RunSummary` 里**没有** `input_event_hash`。
//! 引擎对自己手里那段切片的自哈希（`BacktestReport::input_data_hash`）是**身份**——它已经
//! 落在 `RunManifest.input_event_hash` 里。在 summary 再抄一份，读者就会以为那是"这次运行的
//! 输入事件摘要"（一个读数），而它实际是引擎自哈希；更糟的是，那个表达式的形状与本仓一条
//! 判据（`event_backtest_evidence_check` 的 `self_hash_fallback == 0`）正面冲突——那条判据守的
//! 正是"引擎自哈希不得冒充输入身份"。少抄一份，两件事同时解决。

use crate::artifact_store::sink;
use crate::error::{AppError, AppErrorCategory};
use crate::spec::{BacktestArtifacts, BacktestSpec};
use qx_core::{Fill, RunManifest};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// `summary` 的 schema 版本。改字段必须抬它——读回侧按它判断"这份产物是哪个世代"。
pub(super) const SUMMARY_SCHEMA_VERSION: u32 = 1;

/// 本用例固定用的数据档。写成常量而不是到处字面量：档位是产物里必须交代的一格，
/// 而"交代"与"实际"必须是同一个词。
pub(super) const DATA_TIER: &str = "bar";

/// 一次运行的读数（`<run_id>.summary.json`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RunSummary {
    pub schema_version: u32,
    pub run_id: String,
    pub instrument: String,
    pub account_id: String,
    pub currency: String,
    pub strategy_id: String,
    pub strategy_version: String,
    /// 读到的 Bar 根数。
    pub bars: u64,
    pub fills: u64,
    pub equity_points: u64,
    /// 结果哈希（与 `RunManifest.result_hash` 同源）。
    pub result_hash: String,
    /// 合成数据身份（`barframe:<内容哈希>`）。
    pub data_fingerprint: String,
    /// Declared input provenance for report readers; absent on legacy v1 summaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    /// RunManifest input digest and location, used to verify the declared source frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_data_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_manifest: Option<String>,
    /// SHA-256 checked references to the completed summary/equity/fills/manifest set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_record: Option<String>,
    /// Storage locations are resolved at the application boundary; RunRecord V2 itself is pathless.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_locations: Option<serde_json::Value>,
    /// 规格摘要（与 `RunManifest.config_hash` 同源）。
    pub config_hash: String,
    /// Explicit, machine-readable execution assumptions for research evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matching_kernel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_rules: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_costs: Option<serde_json::Value>,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub final_equity_raw: i128,
    pub fees_raw: i128,
    pub turnover_raw: i128,
    /// Currency-aware, scale-preserving costs and turnover; absent on legacy summaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_exact: Option<serde_json::Value>,
    /// 事件日志重放吞下的事件条数。
    pub replay_events: u64,
    /// Structured replay facts shared with report readers; absent in legacy v1 summaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<RunReplaySummary>,
    /// 重放重建出的账簿条数——必须等于 `ledger_entries`，否则这一轮不该落盘。
    pub replay_ledger_entries: u64,
    /// 本轮运行账簿的条数（与上一格是同一把尺子的两端）。
    pub ledger_entries: u64,
    pub clock_start: u64,
    pub clock_end: u64,
    pub seed: u64,
    pub risk_rule_set_version: String,
    pub fee_model: String,
    pub fill_model: String,
    /// Optional structured audit details; absent when reading legacy v1 artifacts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_model_details: Option<qx_xingban::FillModelDescriptor>,
    pub data_tier: String,
    /// Optional grouped order rejections; absent in old v1 summaries with no integrity block.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejection_facts: Vec<(String, u64)>,
    /// Absent for runs without protective exits (preserves the v1 wire shape).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protective_exit_fills: Vec<ProtectiveExitSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RunReplaySummary {
    pub log_digest: String,
    pub events: u64,
    pub ledger_entries: u64,
    pub run_ledger_entries: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ProtectiveExitSummary {
    pub reason: String,
    pub trigger_ts: u64,
    pub fill_ts: u64,
    pub order_id: u64,
    pub qty_raw: i128,
    pub price_raw: i128,
}

/// 四份产物的落点。相对 `output_dir` 按进程当前目录解析（与仓内其余入口同口径）。
pub(super) fn artifact_paths(spec: &BacktestSpec) -> BacktestArtifacts {
    let root = PathBuf::from(&spec.output_dir);
    let named = |suffix: &str| {
        root.join(format!("{}.{suffix}", spec.run_id))
            .to_string_lossy()
            .into_owned()
    };
    BacktestArtifacts {
        run_manifest: named("run.json"),
        summary: named("summary.json"),
        equity: named("equity.csv"),
        fills: named("fills.csv"),
        event_trace: Some(named("events.jsonl")),
    }
}

/// 建产物目录。已存在不算错（幂等重跑必须能过）。
pub(super) fn ensure_output_dir(spec: &BacktestSpec) -> Result<(), AppError> {
    std::fs::create_dir_all(&spec.output_dir)
        .map_err(|error| AppError::from_io(&format!("创建产物目录 {}", spec.output_dir), &error))
}

/// Persist a content-addressed run manifest under `<root>/runs`.
/// Existing identical manifests are idempotent; a conflicting manifest at the same identity is
/// rejected. The path and digest convention are shared by CLI, HTTP and SDK callers.
pub(crate) fn persist_run_manifest(
    root: &Path,
    manifest: &RunManifest,
) -> Result<PathBuf, AppError> {
    let runs_root = root.join("runs");
    std::fs::create_dir_all(&runs_root)
        .map_err(|error| AppError::from_io("创建回测 RunManifest 目录", &error))?;
    let safe_run_id = manifest
        .run_id
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.') {
                value
            } else {
                '_'
            }
        })
        .collect::<String>();
    let path = runs_root.join(format!("{safe_run_id}-{:016x}.run.json", manifest.digest()));
    let payload = manifest.to_json().map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("RunManifest 序列化失败: {error}"),
        )
    })?;
    match std::fs::read_to_string(&path) {
        Ok(existing) => {
            let restored = RunManifest::from_json(&existing).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("已有回测 RunManifest 无效 {}: {error}", path.display()),
                )
            })?;
            if restored != *manifest {
                return Err(AppError::new(
                    AppErrorCategory::Conflict,
                    format!(
                        "同一回测 RunManifest 路径已存在不同内容: {}",
                        path.display()
                    ),
                ));
            }
            return Ok(path);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取已有回测 RunManifest", &error)),
    }
    // 落盘委托全仓唯一发布出口（`artifact_store::sink`）：此前这里是
    // 「读判等 → 自造临时文件 → sync → hard_link」的三十行长逻辑。sink 对
    // Idempotent 已含「内容相同即幂等、不同即冲突」，此处只多一层**语义级**
    // 比对：字节不同但 `RunManifest` 解析相等时仍算同一事实（format 漂移不算冲突）。
    match sink::publish(&path, &payload, sink::PublishMode::Idempotent) {
        Ok(()) => {}
        // sink 已按字节判过等；此处补一层**语义级**比对：字节不同但
        // `RunManifest` 解析相等（format 漂移）时仍算同一事实。
        Err(sink::PublishError::Conflict(_)) => {
            let existing = std::fs::read_to_string(&path).map_err(|_| {
                AppError::new(
                    AppErrorCategory::Conflict,
                    format!(
                        "同一回测 RunManifest 路径已存在不同内容: {}",
                        path.display()
                    ),
                )
            })?;
            let restored = RunManifest::from_json(&existing).map_err(|parse_error| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!(
                        "并发生成的回测 RunManifest 无效 {}: {parse_error}",
                        path.display()
                    ),
                )
            })?;
            if restored != *manifest {
                return Err(AppError::new(
                    AppErrorCategory::Conflict,
                    format!(
                        "同一回测 RunManifest 路径已存在不同内容: {}",
                        path.display()
                    ),
                ));
            }
        }
        Err(sink::PublishError::Io(error)) => {
            return Err(AppError::from_io("发布回测 RunManifest", &error));
        }
    }
    Ok(path)
}

/// 写一份文本产物。四份产物共用它，所以"写失败怎么归类"只有一处口径。
///
/// 委托全仓唯一发布出口（`artifact_store::sink`，`Overwrite` 档：这些是每跑一次
/// 就整份重写的可再生展示件，语义与原先的「临时文件 + rename」完全一致）。
pub(super) fn write_text(path: &str, body: &str) -> Result<(), AppError> {
    let path = Path::new(path);
    sink::publish(path, body, sink::PublishMode::Overwrite).map_err(|error| match error {
        sink::PublishError::Io(error) => AppError::from_io("发布产物", &error),
        // Overwrite 档不冲突；真出现就是原语被改坏，按冲突报比按存储故障报诚实。
        sink::PublishError::Conflict(message) => AppError::new(AppErrorCategory::Conflict, message),
    })
}

/// 读一份文本产物。读不到时由 [`AppError::from_io`] 归类（不在盘 → `DataUnavailable`）。
pub(super) fn read_text(path: &str) -> Result<String, AppError> {
    std::fs::read_to_string(path)
        .map_err(|error| AppError::from_io(&format!("读取产物 {path}"), &error))
}

/// Check the schema-registered manifest and RunRecord references for a modern run summary.
///
/// This is shared by Bar and depth verification so every application caller receives the
/// same artifact-integrity verdict. The API/report layer still roots and size-bounds paths
/// before invoking the verifier; this helper owns identity and digest semantics.
pub(super) fn verify_declared_artifact_identity(
    summary: &serde_json::Value,
    checks: &mut Vec<String>,
    mismatches: &mut Vec<String>,
    reader: &dyn crate::artifact_store::ArtifactReader,
) {
    let version = summary
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    if summary
        .get("run_manifest")
        .and_then(serde_json::Value::as_str)
        .is_some()
        && summary
            .get("input_data_hash")
            .and_then(serde_json::Value::as_str)
            .is_some()
    {
        match crate::artifact_store::verify_declared_run_manifest_with_reader(summary, reader) {
            Ok(()) => checks.push("RunManifest 声明身份已复核".into()),
            Err(error) => mismatches.push(format!("RunManifest 身份复核失败: {error}")),
        }
    }
    if summary
        .get("run_record")
        .and_then(serde_json::Value::as_str)
        .is_some()
        || version >= 5
    {
        match crate::artifact_store::verify_declared_run_record_with_reader(summary, reader) {
            Ok(()) => checks.push("RunRecord 与四份核心产物 SHA-256 已复核".into()),
            Err(error) => mismatches.push(format!("RunRecord SHA-256 复核失败: {error}")),
        }
    }
}

/// 已存在的那份 manifest（幂等/冲突判定用）。**不存在是正常路径**，所以返回 `Ok(None)`。
pub(super) fn existing_manifest(path: &str) -> Result<Option<String>, AppError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AppError::from_io(&format!("读取既有产物 {path}"), &error)),
    }
}

/// `ts,equity_raw,position_raw` 三列。时间、权益与持仓逐行对齐；行数取三者较短的，
/// 多出来的采样值没有对应坐标，不能伪装成完整的研究曲线。
pub(super) fn equity_csv(ts: &[u64], equity: &[i128], positions: &[i128]) -> String {
    let mut out = String::from("ts,equity_raw,position_raw\n");
    for ((stamp, value), position) in ts.iter().zip(equity.iter()).zip(positions.iter()) {
        out.push_str(&format!("{stamp},{value},{position}\n"));
    }
    out
}

/// Scale-aware equity CSV for runs whose marked curve cannot be represented by the legacy raw
/// column. The original columns remain available and retain their ordering and semantics.
pub(super) fn equity_csv_with_exact(
    ts: &[u64],
    equity: &[i128],
    equity_exact: &[qx_core::ExactDecimal],
    positions: &[i128],
) -> Result<String, AppError> {
    if ts.len() != equity.len() || ts.len() != equity_exact.len() || ts.len() != positions.len() {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            "权益 CSV 的时间、raw、exact 与持仓曲线长度不一致",
        ));
    }
    let mut out = String::from("ts,equity_raw,equity_exact,position_raw\n");
    for (((stamp, value), exact), position) in ts
        .iter()
        .zip(equity.iter())
        .zip(equity_exact.iter())
        .zip(positions.iter())
    {
        out.push_str(&format!("{stamp},{value},{exact},{position}\n"));
    }
    Ok(out)
}

/// **刻意没有 side 列**：`qx_core::Fill` 不带方向，方向在订单上；给这一格编一个默认值
/// 就是"看起来完整的来源未知"，正是本仓明令禁止的形状。v2 的序列化由 artifact_store 共用。
pub(super) fn fills_csv(fills: &[Fill]) -> Result<String, AppError> {
    crate::artifact_store::serialize_backtest_fills_csv(fills)
        .map_err(|message| AppError::new(AppErrorCategory::InternalInvariant, message))
}

/// Complete, replayable event trace stored beside the four stable display artifacts.
/// The trace is a sidecar so the established `BacktestArtifacts` API remains unchanged.
pub(super) fn event_trace_jsonl(events: &[qx_core::Event]) -> Result<String, AppError> {
    let mut out = String::new();
    for event in events {
        let line = serde_json::to_string(event).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("序列化回测事件失败: {error}"),
            )
        })?;
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

/// CSV 的**数据行**（跳过表头，丢掉空行）。
pub(super) fn csv_rows(body: &str) -> Vec<&str> {
    body.lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .collect()
}

/// `equity.csv` 的**权益列号**：按表头定位 `equity_raw`。
///
/// 两代形状共存——旧 Bar 链是 `ts,equity_raw`，新 Bar 链增加 `position_raw`，深度链是
/// `index,ts,equity_raw,position_raw`。硬编码第 1 列会在四列形状上静默读到时间戳，
/// 于是"权益末值"变成一个 5 位的时间值，复核给出**假阴性**（明明一致却说错）。
/// 按列名取列；表头里没有这一格（再老的产物）时退回第 1 列，保留旧行为。
fn equity_column(body: &str) -> usize {
    let header = body.lines().next().unwrap_or_default();
    header
        .split(',')
        .position(|cell| cell.trim() == "equity_raw")
        .unwrap_or(1)
}

/// `equity.csv` 权益列的**整列**（按行序）。任意一行读不出来就是 `None`。
///
/// **不**跳过坏行凑一个"看起来完整"的序列——切分评估按 Bar 序号对齐，中间少一格会让
/// 训练窗与测试窗错位，而错位不会自己出声。
pub(super) fn equity_series(body: &str) -> Option<Vec<i128>> {
    let column = equity_column(body);
    csv_rows(body)
        .into_iter()
        .map(|row| row.split(',').nth(column)?.trim().parse::<i128>().ok())
        .collect()
}

/// 复核失败的裁决：产物缺一份，就没法"四份互相印证"。
pub(super) fn unverified(run_id: &str, reason: String) -> crate::spec::VerificationResult {
    crate::spec::VerificationResult {
        run_id: run_id.to_string(),
        result_hash: String::new(),
        data_fingerprint: String::new(),
        verified: false,
        checks: Vec::new(),
        mismatches: vec![reason],
    }
}

/// Preserve a hard integrity failure as a typed-category-aware mismatch so API surfaces do not
/// downgrade tampered content-addressed artifacts to an ordinary incomplete report.
pub(super) fn unverified_artifact_read(
    run_id: &str,
    label: &str,
    error: &AppError,
) -> crate::spec::VerificationResult {
    let reason = if error.category() == AppErrorCategory::Conflict {
        format!("RunRecord SHA-256 复核失败（{label}）: {error}")
    } else {
        format!("产物缺失或不完整（{label}）: {error}")
    };
    unverified(run_id, reason)
}

/// `RunSummary` 的构造留在用例里（它需要 `BacktestReport` 的很多格），这里只做**序列化**：
/// 读回侧与写出侧共用同一份结构体，所以"写出去的键"与"读回来的键"不可能漂移。
pub(super) fn summary_json(summary: &RunSummary) -> Result<String, AppError> {
    serde_json::to_string(summary).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("RunSummary 序列化失败: {error}"),
        )
    })
}

/// 反序列化回 `RunSummary`。产物被改坏时是 `StorageFailure`——**产物是存储面**，
/// 它坏了不是调用方的输入错。
pub(super) fn summary_from_json(body: &str) -> Result<RunSummary, AppError> {
    serde_json::from_str(body).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("summary 产物无法解析: {error}"),
        )
    })
}
