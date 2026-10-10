//! qx-app：牵星应用层（QX-DEV-PLAN-2026-10-10 阶段 2 / 方案 §2.1、§15.2、§15.5）。
//!
//! 本 crate 是「**用例**」的那一层。它回答的是"一次研究/交易请求，从调用方进来到结果出去，
//! 中间该发生什么"；它**不**回答"撮合怎么算"（那是 `qx-xingban`）、"指标怎么算"（那是
//! `qx-core`/`qx-xingban`）、"风险怎么裁"（那是 `qx-risk`/`qx-zhenlu`）。
//! 应用层只做四件事：**校验输入、装配领域件、把失败翻译成调用方看得懂的类别、落产物**。
//!
//! ## 依赖方向（唯一一条硬约束）
//!
//! ```text
//! qx-cli ─┐
//! qx-api ─┼─→ qx-app ─→ qx-core / qx-data / qx-datastruct / qx-spec / qx-strategy / qx-xingban / qx-zhenlu
//! qx-python ┘
//! ```
//!
//! 只许**门面依赖应用层**，不许应用层依赖门面。`tools/check_architecture.py` 的
//! `layer_dependency_check` 把这条写成禁止边（qx-app → qx-cli/qx-api/qx-python/qx-adapter/
//! qx-execution/qx-runtime/qx-storage 任一出现即红），`qx_app_check` 另按 Cargo.toml 的实际
//! 依赖集再核一遍——两条判据分别守"没加进来"与"加了会被发现"。
//!
//! ## 为什么公共面是 JSON 字符串而不是 Rust 结构体
//!
//! 三个门面里只有 CLI 说 Rust。Python 走 PyO3、HTTP 走字节流，它们拿不到 Rust 类型；
//! 若公共面是结构体，Python 与 HTTP 就得各自再装配一次——那正是 §15.2「所有业务入口调用
//! 同一 `qx-app` use case」要消灭的形状。所以用例的进出都是 [`spec`] 里那几份**带版本、
//! 严格（`deny_unknown_fields`）**的 JSON：三个入口交出同一份字节，换回同一份字节。
//!
//! ## 公共面
//!
//! - 用例：[`validate_dataset`]、[`run_backtest`]、[`verify_run`]、[`compare_runs`]
//! - 输入/输出：[`DatasetSpec`]、[`DatasetVerdict`]、[`BacktestSpec`]、[`BacktestOutcome`]、
//!   [`VerificationResult`]、[`CompareRunsSpec`]、[`CompareRunsResult`]
//! - 调用上下文与能力档：[`RunContext`]、[`CallerCapability`]
//! - 错误：[`AppError`]、[`AppErrorCategory`]、[`AppAction`]、[`AppRetry`]
//!
//! 现在**没有**的东西（刻意留着，别按名字猜它存在）：`RunHandle` 与异步/可取消的长任务
//! （方案 §2.2 的 `-> RunHandle` 形态属 T2-5/T3-3）、Tick/L2 档（T2-3）、
//! `ReadRunArtifacts`/`BuildReport`（T2-4）。本 crate 今天只交付
//! 「数据验证 → Bar 回测 → 产物 → 复核」这条垂直切片，且它是**同步**的。

pub mod cases;
pub mod context;
pub mod error;
pub mod spec;

#[cfg(test)]
mod tests;

pub use cases::{
    compare_runs, run_backtest, validate_dataset, verify_run, CompareRunsResult, CompareRunsSpec,
    ComparedRun, ComparedRunResult,
};
pub use context::{CallerCapability, RunContext};
pub use error::{AppAction, AppError, AppErrorCategory, AppRetry};
pub use spec::{
    BacktestArtifacts, BacktestOutcome, BacktestSpec, BuiltinStrategySpec, DatasetSpec,
    DatasetVerdict, VerificationResult, BACKTEST_SPEC_SCHEMA_VERSION, DATASET_SPEC_SCHEMA_VERSION,
    MIN_BACKTEST_BARS,
};
