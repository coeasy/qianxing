//! 产物读写：四份产物的落点、格式与**读回**（T2-2）。
//!
//! 产物是应用层对外的**第二张脸**（第一张是 [`crate::spec`] 里的 JSON 类型）。它必须满足
//! 两件事：三个入口写出**逐字节相同**的四份文件；任何一份都能被独立读回来复核
//! （[`crate::cases::verify_run`] 就是那个读回者）。
//!
//! ## 四份产物
//!
//! | 文件 | 内容 | 复核时看什么 |
//! |---|---|---|
//! | `<run_id>.run.json` | `qx_core::RunManifest` 的 JSON | `result_hash` / `data_fingerprint` / `config_hash` |
//! | `<run_id>.summary.json` | [`RunSummary`] | 与 manifest 的同一组哈希逐字相等 |
//! | `<run_id>.equity.csv` | `ts,equity_raw` | 行数 == `equity_points`；末行 == `final_equity_raw` |
//! | `<run_id>.fills.csv` | `ts,order_id,qty_raw,price_raw,fee_raw` | 行数 == `fills` |
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

use crate::error::{AppError, AppErrorCategory};
use crate::spec::{BacktestArtifacts, BacktestSpec};
use qx_core::Fill;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    /// 规格摘要（与 `RunManifest.config_hash` 同源）。
    pub config_hash: String,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub final_equity_raw: i128,
    pub fees_raw: i128,
    pub turnover_raw: i128,
    /// 事件日志重放吞下的事件条数。
    pub replay_events: u64,
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
    pub data_tier: String,
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
    }
}

/// 建产物目录。已存在不算错（幂等重跑必须能过）。
pub(super) fn ensure_output_dir(spec: &BacktestSpec) -> Result<(), AppError> {
    std::fs::create_dir_all(&spec.output_dir)
        .map_err(|error| AppError::from_io(&format!("创建产物目录 {}", spec.output_dir), &error))
}

/// 写一份文本产物。四份产物共用它，所以"写失败怎么归类"只有一处口径。
pub(super) fn write_text(path: &str, body: &str) -> Result<(), AppError> {
    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);
    let serial = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = format!("{path}.tmp-{}-{serial}", std::process::id());
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| AppError::from_io(&format!("创建临时产物 {temporary}"), &error))?;
        file.write_all(body.as_bytes())
            .map_err(|error| AppError::from_io(&format!("写入临时产物 {temporary}"), &error))?;
        file.sync_all()
            .map_err(|error| AppError::from_io(&format!("同步临时产物 {temporary}"), &error))?;
        drop(file);
        std::fs::rename(&temporary, path)
            .map_err(|error| AppError::from_io(&format!("发布产物 {path}"), &error))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// 读一份文本产物。读不到时由 [`AppError::from_io`] 归类（不在盘 → `DataUnavailable`）。
pub(super) fn read_text(path: &str) -> Result<String, AppError> {
    std::fs::read_to_string(path)
        .map_err(|error| AppError::from_io(&format!("读取产物 {path}"), &error))
}

/// 已存在的那份 manifest（幂等/冲突判定用）。**不存在是正常路径**，所以返回 `Ok(None)`。
pub(super) fn existing_manifest(path: &str) -> Result<Option<String>, AppError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AppError::from_io(&format!("读取既有产物 {path}"), &error)),
    }
}

/// `ts,equity_raw` 两列。时间与权益逐行成对，行数取两者较短的——多出来的那一格没有配对的
/// 时间戳，印出来只会让读回侧对不上账。
pub(super) fn equity_csv(ts: &[u64], equity: &[i128]) -> String {
    let mut out = String::from("ts,equity_raw\n");
    for (stamp, value) in ts.iter().zip(equity.iter()) {
        out.push_str(&format!("{stamp},{value}\n"));
    }
    out
}

/// `ts,order_id,qty_raw,price_raw,fee_raw` 五列。
///
/// **刻意没有 side 列**：`qx_core::Fill` 不带方向，方向在订单上；给这一格编一个默认值
/// 就是"看起来完整的来源未知"，正是本仓明令禁止的形状。
pub(super) fn fills_csv(fills: &[Fill]) -> String {
    let mut out = String::from("ts,order_id,qty_raw,price_raw,fee_raw\n");
    for fill in fills {
        out.push_str(&format!(
            "{},{},{},{},{}\n",
            fill.ts,
            fill.order_id,
            fill.qty.raw(),
            fill.price.raw(),
            fill.fee.raw()
        ));
    }
    out
}

/// CSV 的**数据行**（跳过表头，丢掉空行）。
pub(super) fn csv_rows(body: &str) -> Vec<&str> {
    body.lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .collect()
}

/// `equity.csv` 末行的第二列。读不出来就是 `None`——调用方据此报"复核不通过"，
/// **不**拿 0 当兜底。
pub(super) fn last_equity_raw(body: &str) -> Option<i128> {
    let last = csv_rows(body).pop()?;
    last.split(',').nth(1)?.trim().parse::<i128>().ok()
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
