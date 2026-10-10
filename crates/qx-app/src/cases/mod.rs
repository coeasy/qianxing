//! 应用用例（T2-1/T2-2）。每一个用例的**边界**都长这三段：
//!
//! 1. `context.require(...)` —— 能力闸（路线图 §5 R/P/O/L）。
//! 2. `guard_panics(...)` —— panic 闸（T2-0），把下游的 panic 翻成 `InternalInvariant`。
//! 3. `attach(error, correlation_id)` —— 给**每一条**出口挂上对齐 id，不许漏。
//!
//! 第 3 段刻意做成边界处的一次收口，而不是在用例体里逐处 `.map_err(|e| e.with_correlation_id(..))`：
//! 逐处挂的话，每加一条 `?` 就多一次"记得挂"的机会，漏掉的那条就是一条不可定位的错误——
//! 而不可定位的错误恰恰是 G6 要消灭的东西。
//!
//! ## 用例登记面（每个用例必须定义的九项）
//!
//! 路线图 §2 要求每个用例定义九项。它们逐条落在下面，`maturity/app_use_cases.yaml` 是同一份
//! 内容的机读登记面，由 `qx_app_check` 与本模块的文档逐条对账——**改了这里不改登记面会红**。
//!
//! | 项 | `validate_dataset` | `run_backtest` | `verify_run` | `compare_runs` | `run_experiment` | `run_depth_experiment` | `run_multi_leg_experiment` | `run_depth_backtest` | `verify_depth_run` | `validate_trade_control` | `run_multi_leg_backtest` |
//! |---|---|---|---|---|---|---|---|---|---|---|
//! | 输入 schema | `DatasetSpec` v1 | `BacktestSpec` v1（单腿 Bar） | `BacktestOutcome` | `CompareRunsSpec` v1 | `RunExperimentSpec` v1（Bar 参数网格） | `RunDepthExperimentSpec` v1（`DepthBacktestSpec` + bounded strategy grid；最多 256 候选） | `RunMultiLegExperimentSpec` v1（MultiLegBacktestSpec + fast_window/slow_window/period/threshold_bps/quantity_raw 参数笛卡尔网格；最多 256 候选） | `DepthBacktestSpec v1（L1 Tick 或 L2/L3 OrderBook + Rust 内置策略 + 可选 TradingInstrumentSpec；衍生品要求 OrderPolicy）` | `DepthBacktestOutcome` | `TradeControlSpec` v1（交易控制 + ControlCommand） | `MultiLegBacktestSpec` v1（两份对齐 BarFrame + 双腿策略、market spec、共享/腿级费率、资金费与撮合模型） |
//! | 输出 schema | `DatasetVerdict` | `BacktestOutcome` | `VerificationResult` | `CompareRunsResult` v1 | `RunExperimentResult` v1 | `RunDepthExperimentResult` v1（候选结果/错误 + 收益降序、回撤升序确定性排名） | `RunMultiLegExperimentResult` v1（逐候选结果/错误 + 按组合收益确定性排名 + 实验摘要产物） | `DepthBacktestOutcome` v1 | `VerificationResult` | 经验证的 `ControlCommand`，交给现有 Rust control submitter | `MultiLegBacktestOutcome` v1（双腿哈希、组合收益、归因产物路径） |
//! | 运行权限 | `RESEARCH` | `RESEARCH` | 无（纯读产物） | 无（纯计算） | `RESEARCH` | `RESEARCH` | `RESEARCH` | `RESEARCH` | 无（纯读产物） | 必须满足认证授予的 Trading 权限 | `RESEARCH` |
//! | 幂等键 | `dataset_id` + 内容指纹 | `run_id` + `config_hash` + `data_fingerprint` | `run_id` | 完整输入文档（纯计算） | `experiment_id` + 规格指纹；冲突在子运行前拒绝 | `experiment_id` + 完整规格指纹；异规格在候选启动前拒绝 | `experiment_id` + 完整规格指纹；不同规格在启动候选前拒绝 | `run_id` + 规格/盘口指纹；产物冲突即拒绝 | `run_id` | `command_id` + `request_id`，由 ControlPlane 拒绝重复项 | `run_id` + 规格与双 BarFrame 指纹；异内容拒绝覆盖 |
//! | 取消行为 | 无取消点（同步纯读） | 同步 run_backtest 不取消；BacktestSpec::start 通过 Rust 引擎逐 Bar 协作取消，取消不返回部分报告或产物 | 无取消点 | 无取消点 | RunExperimentSpec::start 在候选之间及候选 Bar 内核协作取消，不写部分实验报告 | `RunDepthExperimentSpec::start` 在候选间与 Tick/快照内核循环协作取消，不发布部分实验报告 | RunMultiLegExperimentSpec::start 在候选之间和候选双腿撮合循环协作取消，不发布部分实验报告 | 同步 run_depth_backtest 不取消；DepthBacktestSpec::start 通过 Rust 引擎逐 Tick/快照协作取消，取消不返回部分报告或产物 | 无取消点（同步纯读） | 无取消点；验证后由原有命令队列和 worker 驱动终态 | MultiLegBacktestSpec::start 在信号与双腿撮合边界协作取消，不发布部分归因产物 |
//! | 事件/进度 | 无 | RunHandle 提供 run_id、状态、取消、等待和一次性结果读取；无百分比进度 | 无 | 无 | 逐候选结果 + RunHandle 生命周期；无百分比进度 | 逐候选记录结果与错误；RunHandle 提供状态/取消/等待/一次性结果读取，无百分比进度 | 逐候选记载成功与稳定 AppError JSON；RunHandle 提供状态/取消/等待/一次性结果读取，无百分比进度 | RunHandle 提供 run_id、状态、取消、等待和一次性结果读取；无百分比进度；结果绑定 Rust 撮合报告和 ReplayVerifier 重放证据 | 无 | 返回 `Accepted` 审计回执；执行进度/终态复用 API 审计读面 | RunHandle 提供 run_id、状态、取消、等待和一次性结果读取；每条腿独立重放校验 |
//! | 产物清单 | 无 | run.json / summary.json / equity.csv / fills.csv | 无（只读） | 无（内存结果） | 每候选四份回测产物 + 实验 JSON 摘要 | 各成功候选的四份深度回测产物 + `<experiment_id>.depth-experiment.json` | 各成功候选的多腿归因产物 + `<experiment_id>.multi-leg-experiment.json` | run.json / summary.json / equity.csv / fills.csv | 无（只读） | ControlPlane 哈希审计、原有 EventLog 与 Ledger | `<run_id>.multi-leg.json`（两腿重放、成交归因、裸腿待对账） |
//! | 错误类别 | 八类见 `AppErrorCategory` | 同左 | 同左 | 同左 | 同左（候选错误隔离记录） | AppErrorCategory 八类；候选错误隔离，整体规格/存储错误由用例返回 | AppErrorCategory 八类；候选失败隔离，整体规格/存储错误由用例返回 | 同左 | 同左 | 同左（认证拒绝、不可执行命令与订单载荷拒绝在入队前返回） | 同左（数据/策略输入、幂等冲突、撮合不变量） |
//! | 能力等级 | R | R | R | R | R | R | R | R | R | P/L | R |

pub(crate) mod artifacts;
mod compare_runs;
mod experiment_grid;
pub(crate) mod guard;
mod run_backtest;
mod run_depth_backtest;
mod run_depth_experiment;
mod run_experiment;
mod run_multi_leg_backtest;
mod run_multi_leg_experiment;
mod validate_dataset;
mod validate_trade_control;
mod verify_depth_run;
mod verify_run;

pub use compare_runs::compare_runs;
pub use compare_runs::{CompareRunsResult, CompareRunsSpec, ComparedRun, ComparedRunResult};
pub use run_backtest::run_backtest;
pub use run_depth_backtest::run_depth_backtest;
pub use run_depth_experiment::run_depth_experiment;
pub use run_depth_experiment::{
    DepthExperimentCandidate, DepthExperimentRankedCandidate, RunDepthExperimentResult,
    RunDepthExperimentSpec, RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION,
};
pub use run_experiment::run_experiment;
pub use run_experiment::{
    ExperimentCandidateResult, ExperimentParameterSpace, RunExperimentResult, RunExperimentSpec,
    MAX_EXPERIMENT_CANDIDATES, RUN_EXPERIMENT_SCHEMA_VERSION,
};
pub use run_multi_leg_backtest::run_multi_leg_backtest;
pub use run_multi_leg_experiment::run_multi_leg_experiment;
pub use run_multi_leg_experiment::{
    MultiLegExperimentCandidate, MultiLegExperimentRankedCandidate, RunMultiLegExperimentResult,
    RunMultiLegExperimentSpec, RUN_MULTI_LEG_EXPERIMENT_SCHEMA_VERSION,
};
pub use validate_dataset::validate_dataset;
pub use validate_trade_control::validate_trade_control;
pub use validate_trade_control::{TradeControlSpec, TradeWorkflow, TRADE_CONTROL_SCHEMA_VERSION};
pub use verify_depth_run::verify_depth_run;
pub use verify_run::verify_run;

use crate::error::AppError;

/// 把用例的 correlation id 挂到任何一条返回的错误上。
///
/// 已经带 id 的错误原样放行——`guard_panics` 翻出来的那条自带 id，覆盖它会把
/// "哪个边界捕获的"这条信息抹掉。
pub(super) fn attach(error: AppError, correlation_id: &str) -> AppError {
    if error.correlation_id().is_some() {
        error
    } else {
        error.with_correlation_id(correlation_id)
    }
}
