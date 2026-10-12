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
//! qx-api ─┼─→ qx-app ─→ qx-core / qx-guanxing / qx-datastruct / qx-spec / qx-strategy / qx-xingban / qx-zhenlu
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
//! - 用例：[`validate_dataset`]、[`run_backtest`]、[`run_depth_backtest`]、[`run_multi_leg_backtest`]、[`run_experiment`]、[`run_multi_leg_experiment`]、[`plan_risk_parity`]、[`verify_run`]、[`verify_depth_run`]、[`compare_runs`]、[`validate_trade_control`]
//! - 输入/输出：[`DatasetSpec`]、[`DatasetVerdict`]、[`BacktestSpec`]、[`BacktestOutcome`]、
//!   [`VerificationResult`]、[`CompareRunsSpec`]、[`CompareRunsResult`]
//! - 调用上下文与能力档：[`RunContext`]、[`CallerCapability`]
//! - 错误：[`AppError`]、[`AppErrorCategory`]、[`AppAction`]、[`AppRetry`]
//!
//! Paper/Live 使用 `validate_trade_control` 校验授权能力、工作流和订单命令；API facade 随后将
//! 同一 `ControlCommand` 交给现有 ControlPlane 与 worker，不在 SDK 或应用层另建执行路径。
//! 本 crate 尚未提供 `ReadRunArtifacts`/`BuildReport`（T2-4）。`qx-api` 将远程 Run 状态与终态结果持久化；服务重启后可查询已完成结果，崩溃前仍运行的任务会明确恢复为失败态（worker 本身不跨进程续跑）。
//! Bar、Tick/OrderBook、多腿回测以及 Bar/深度/多腿参数实验的 `start` 入口返回可协作取消的
//! [`RunHandle`]；候选运行复用对应的 Rust 回测用例与内核。

pub mod cases;
pub mod context;
pub mod error;
pub mod multi_leg;
mod run_handle;
pub mod spec;
pub mod trade_facts;

#[cfg(test)]
mod tests;

pub use cases::{
    compare_runs, plan_algo_execution, plan_capital_allocation, plan_risk_parity, run_backtest,
    run_depth_backtest, run_depth_experiment, run_experiment, run_multi_leg_backtest,
    run_multi_leg_experiment, validate_dataset, validate_trade_control, verify_depth_run,
    verify_run, CompareRunsResult, CompareRunsSpec, ComparedRun, ComparedRunResult,
    DepthExperimentCandidate, DepthExperimentRankedCandidate, ExperimentCandidateResult,
    ExperimentParameterSpace, MultiLegExperimentCandidate, MultiLegExperimentRankedCandidate,
    RunDepthExperimentResult, RunDepthExperimentSpec, RunExperimentResult, RunExperimentSpec,
    RunMultiLegExperimentResult, RunMultiLegExperimentSpec, TradeControlSpec, TradeWorkflow,
    EXPERIMENT_BASELINE_NAMES, MAX_EXPERIMENT_CANDIDATES, RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION,
    RUN_EXPERIMENT_SCHEMA_VERSION, RUN_MULTI_LEG_EXPERIMENT_SCHEMA_VERSION,
    TRADE_CONTROL_SCHEMA_VERSION,
};
pub use cases::{
    ExperimentBaselineReport, ExperimentFoldReport, ExperimentOutOfSampleReport,
    ExperimentSplitPlan,
};
pub use context::{CallerCapability, RunContext};
pub use error::{AppAction, AppError, AppErrorCategory, AppRetry};
pub use multi_leg::{
    multi_leg_bar_close_raw, multi_leg_cost_bps, multi_leg_group_attributions,
    multi_leg_group_totals, multi_leg_integrity_json, multi_leg_integrity_summary,
    multi_leg_leg_buckets, multi_leg_leg_integrity, multi_leg_leg_margin,
    multi_leg_pending_reconcile_json, multi_leg_realized_standing_at, MultiLegAttribution,
    MultiLegLegFacts, MultiLegLegIntegrity, MultiLegPendingReconcile, MultiLegSignalBucket,
    MULTI_LEG_FUNDING_INTERVAL_MS,
};
pub use qx_core::MakerTakerFeeSchedule;
pub use run_handle::{RunEvent, RunEventKind, RunHandle, RunStatus, DEFAULT_WAIT_TIMEOUT};
pub use spec::{
    AlgoExecutionMethod, AlgoExecutionPlan, AlgoExecutionSpec, BacktestArtifacts, BacktestOutcome,
    BacktestSpec, BuiltinStrategySpec, CapitalAllocationResult, CapitalAllocationSpec, DatasetSpec,
    DatasetVerdict, DepthBacktestOutcome, DepthBacktestSpec, MultiLegBacktestOutcome,
    MultiLegBacktestSpec, MultiLegFillModel, ProtectiveExitSpec, RiskParityAssetSpec,
    RiskParityResult, RiskParitySpec, VerificationResult, ALGO_EXECUTION_SPEC_SCHEMA_VERSION,
    BACKTEST_SPEC_SCHEMA_VERSION, DATASET_SPEC_SCHEMA_VERSION, DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
    MIN_BACKTEST_BARS, MULTI_LEG_BACKTEST_SPEC_SCHEMA_VERSION, RISK_PARITY_RESULT_SCHEMA_VERSION,
    RISK_PARITY_SPEC_SCHEMA_VERSION,
};
pub use trade_facts::{rejection_count, rejection_facts, rejection_facts_line, spread_group_id};
