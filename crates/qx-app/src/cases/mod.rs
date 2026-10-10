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
//! | 项 | `validate_dataset` | `run_backtest` | `verify_run` | `compare_runs` | `run_experiment` | `run_depth_backtest` | `verify_depth_run` |
//! |---|---|---|---|---|---|---|
//! | 输入 schema | `DatasetSpec` v1 | `BacktestSpec` v1（单腿 Bar） | `BacktestOutcome` | `CompareRunsSpec` v1 | `RunExperimentSpec` v1（Bar 参数网格） | `DepthBacktestSpec` v1（内置策略 + L1/L2） | `DepthBacktestOutcome` |
//! | 输出 schema | `DatasetVerdict` | `BacktestOutcome` | `VerificationResult` | `CompareRunsResult` v1 | `RunExperimentResult` v1 | `DepthBacktestOutcome` v1 | `VerificationResult` |
//! | 运行权限 | `RESEARCH` | `RESEARCH` | 无（纯读产物） | 无（纯计算） | `RESEARCH` | `RESEARCH` | 无（纯读产物） |
//! | 幂等键 | `dataset_id` + 内容指纹 | `run_id` + `config_hash` + `data_fingerprint` | `run_id` | 完整输入文档（纯计算） | `experiment_id` + 规格指纹；冲突在子运行前拒绝 | `run_id` + 规格/盘口指纹；产物冲突即拒绝 | `run_id` |
//! | 取消行为 | 无取消点（同步纯读） | 无取消点（同步） | 无取消点 | 无取消点（有界输入） | 无取消点；候选上限 256 | 无取消点（同步；RunHandle 生命周期待接入） | 无取消点（同步纯读） |
//! | 事件/进度 | 无 | 无 | 无 | 无 | 结果逐候选记录成功或稳定错误 | 最终结果含撮合/重放证据；无实时进度 | 无 |
//! | 产物清单 | 无 | run.json / summary.json / equity.csv / fills.csv | 无（只读） | 无（内存结果） | 每候选四份回测产物 + 实验 JSON 摘要 | run.json / summary.json / equity.csv / fills.csv | 无（只读） |
//! | 错误类别 | 八类见 `AppErrorCategory` | 同左 | 同左 | 同左 | 同左（候选错误隔离记录） | 同左 | 同左 |
//! | 能力等级 | R | R | R | R | R | R | R |

pub(crate) mod artifacts;
mod compare_runs;
pub(crate) mod guard;
mod run_backtest;
mod run_depth_backtest;
mod run_experiment;
mod validate_dataset;
mod verify_depth_run;
mod verify_run;

pub use compare_runs::compare_runs;
pub use compare_runs::{CompareRunsResult, CompareRunsSpec, ComparedRun, ComparedRunResult};
pub use run_backtest::run_backtest;
pub use run_depth_backtest::run_depth_backtest;
pub use run_experiment::run_experiment;
pub use run_experiment::{
    ExperimentCandidateResult, ExperimentParameterSpace, RunExperimentResult, RunExperimentSpec,
    MAX_EXPERIMENT_CANDIDATES, RUN_EXPERIMENT_SCHEMA_VERSION,
};
pub use validate_dataset::validate_dataset;
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
