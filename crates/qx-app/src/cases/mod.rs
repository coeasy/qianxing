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
//! | 项 | `validate_dataset` | `run_backtest` | `verify_run` | `compare_runs` | `run_experiment` | `run_depth_experiment` | `run_multi_leg_experiment` | `run_depth_backtest` | `verify_depth_run` | `validate_trade_control` | `run_multi_account_backtest` | `run_multi_leg_backtest` | `plan_risk_parity` | `plan_algo_execution` | `plan_capital_allocation` | `run_provider_backtest_with_manifest` | `run_provider_depth_backtest`  | `run_multi_leg_backtest_with_strategy` |
//! | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | ---  | --- |
//! | 输入 schema | `DatasetSpec` v1 | `BacktestSpec` v1（单标的 Bar，可选冻结 `TradingInstrumentSpec`；可选同账户单腿策略组合 portfolio（多个 Rust/Python 自定义 callback 可占槽，其他槽使用内置策略），按优先级共享资金；组合预算不得超账户初始资金，且暂不支持账户级 protective_exit） | `BacktestOutcome` | `CompareRunsSpec` v1 | `RunExperimentSpec` v1（Bar 参数网格；可选 `strategy_kinds` 快速选择单腿内置策略；支持训练窗选策略/参数 + OOS；最多 256 候选） | `RunDepthExperimentSpec` v1（`DepthBacktestSpec` + bounded strategy grid；最多 256 候选） | `RunMultiLegExperimentSpec` v1（MultiLegBacktestSpec + fast_window/slow_window/period/threshold_bps/quantity_raw 参数笛卡尔网格；最多 256 候选；本地 Rust/Python 可用 StrategyArtifactIdentity + factory 为每个候选创建自定义策略，不与 strategy_kinds 混用） | `DepthBacktestSpec v1（L1 Tick 或 L2/L3 OrderBook + Rust 内置策略或本地 Rust/Python 自定义回调；同账户 portfolio 槽位支持多个回调与内置策略混合，回调身份排序进入配置指纹；可选 TradingInstrumentSpec；L2 可选 MarketMakingConfig 双边报价/库存偏移/撤单；衍生品要求 OrderPolicy）` | `DepthBacktestOutcome` | `TradeControlSpec` v1（交易控制 + ControlCommand） | MultiAccountBacktestSpec v1（2..=32 个独立账户的 BacktestSpec；Bar 数据可跨市场，但同一结算币种；不做跨账户净额） | `MultiLegBacktestSpec` v1（两份对齐 BarFrame + 双腿策略、market spec、共享/腿级费率、资金费与撮合模型） | `RiskParitySpec` v1（最多 64 资产、冻结对齐收益率、资金与换手上限） | `AlgoExecutionSpec` v1（TWAP 子单数或冻结 VWAP 成交量 profile） | `CapitalAllocationSpec` v1（预算、账户现金与带优先级策略请求） | `ProviderBacktestSpec`（DataRequest + BacktestSpec + dataset identity） | `ProviderDepthBacktestSpec v1（单标的 Quote DataRequest + DepthBacktestSpec；冻结深度帧嵌入来源身份）`  | `MultiLegBacktestSpec` v1 + `StrategyArtifactIdentity` (Rust/Python callback) |
//! | 输出 schema | `DatasetVerdict` | `BacktestOutcome` | `VerificationResult` | `CompareRunsResult` v1 | `RunExperimentResult` v1 | `RunDepthExperimentResult` v1（候选结果/错误 + 收益降序、回撤升序确定性排名） | `RunMultiLegExperimentResult` v1（逐候选结果/错误 + 按组合收益确定性排名 + 实验摘要产物；自定义策略 ID/version/SHA-256 纳入实验指纹） | `DepthBacktestOutcome` v1 | `VerificationResult` | 经验证的 `ControlCommand`，交给现有 Rust control submitter | MultiAccountBacktestResult v1（每账户独立 outcome/error + 成功账户本金加权收益摘要；失败账户隔离） | `MultiLegBacktestOutcome` v1（双腿哈希、组合收益、归因产物路径） | `RiskParityResult` v1（目标权重、名义金额、调仓增量与计划哈希） | `AlgoExecutionPlan` v1（parent-linked 子单和调度时间） | `CapitalAllocationResult` v1（确定性分配） | `ProviderBacktestOutcome` v1（清单、校验裁定和回测结果） | `ProviderDepthBacktestOutcome v1（共享深度结果 + Provider/request/retry/source_hash 与冻结输入路径）`  | `MultiLegBacktestOutcome` v1 (both leg hashes, combined return, attribution artifact) |
//! | 运行权限 | `RESEARCH` | `RESEARCH` | 无（纯读产物） | 无（纯计算） | `RESEARCH` | `RESEARCH` | `RESEARCH` | `RESEARCH` | 无（纯读产物） | 必须满足认证授予的 Trading 权限 | RESEARCH | `RESEARCH` | `RESEARCH` | `RESEARCH` | RESEARCH | `RESEARCH` | `RESEARCH`  | `RESEARCH` |
//! | 幂等键 | `dataset_id` + 内容指纹 | `run_id` + `config_hash` + `data_fingerprint` | `run_id` | 完整输入文档（纯计算） | `experiment_id` + 规格指纹；冲突在子运行前拒绝 | `experiment_id` + 完整规格指纹；异规格在候选启动前拒绝 | `experiment_id` + 完整规格指纹；自定义策略身份另绑定 ID/version/SHA-256；不同规格在启动候选前拒绝 | `run_id` + 规格/盘口指纹；产物冲突即拒绝 | `run_id` | `command_id` + `request_id`，由 ControlPlane 拒绝重复项 | batch_id + 按 account_id 排序的规格指纹（忽略本地数据/产物路径）；成员 run_id 唯一，数据内容由子结果指纹标识 | `run_id` + 规格与双 BarFrame 指纹；异内容拒绝覆盖 | `portfolio_id` + 完整规格（纯计算） | `parent_id` + 完整规格（纯计算） | portfolio_id + 完整规格（纯计算） | 数据帧及清单内容逐字节相同允许重跑；同路径异内容返回 Conflict | `run_id` + 深度规格/Provider source_hash；同路径异内容 Conflict  | `run_id` + spec/data fingerprints + strategy ID/version/source SHA-256 |
//! | 取消行为 | 无取消点（同步纯读） | 同步 run_backtest 不取消；BacktestSpec::start 通过 Rust 引擎逐 Bar 协作取消，取消不返回部分报告或产物 | 无取消点 | 无取消点 | RunExperimentSpec::start 在候选之间及候选 Bar 内核协作取消，不写部分实验报告 | `RunDepthExperimentSpec::start` 在候选间与 Tick/快照内核循环协作取消，不发布部分实验报告 | RunMultiLegExperimentSpec::start 与 start_with_strategy_factory 在候选之间和候选双腿撮合循环协作取消，不发布部分实验报告 | 同步 run_depth_backtest 不取消；DepthBacktestSpec::start 与 start_depth_backtest_with_strategy_slots 通过 Rust 引擎逐 Tick/快照协作取消，取消不返回部分报告或产物 | 无取消点（同步纯读） | 无取消点；验证后由原有命令队列和 worker 驱动终态 | 同步入口不取消；MultiAccountBacktestSpec::start 在账户间与当前 Rust Bar 引擎内协作取消，不返回部分批次结果；单账户失败隔离，最多 32 个成员 | MultiLegBacktestSpec::start 在信号与双腿撮合边界协作取消，不发布部分归因产物 | 同步有界求解，无取消点（最多 10000 轮） | 同步有限计算，无取消点（最多 10000 个子单） | 无取消点（最多 1024 个账户与 10000 个请求） | 同步 Provider 获取由 adapter deadline 控制；冻结与内核运行同步不可取消 | 同步 Provider 获取与深度内核无取消点，外部 deadline 由 Provider adapter 控制  | Sync run has no cancellation; RunHandle checks between signals and Rust leg matching |
//! | 事件/进度 | 无 | RunHandle 提供 run_id、状态、取消、等待和一次性结果读取；无百分比进度 | 无 | 无 | 逐候选结果 + RunHandle 生命周期；无百分比进度 | 逐候选记录结果与错误；RunHandle 提供状态/取消/等待/一次性结果读取，无百分比进度 | 逐候选记载成功与稳定 AppError JSON；RunHandle 提供状态/取消/等待/一次性结果读取，无百分比进度 | RunHandle 提供 run_id、状态、取消、等待和一次性结果读取；无百分比进度；结果绑定 Rust 撮合报告和 ReplayVerifier 重放证据 | 无 | 返回 `Accepted` 审计回执；执行进度/终态复用 API 审计读面 | 每账户完成后记录 outcome 或稳定 AppError JSON；无百分比进度 | RunHandle 提供 run_id、状态、取消、等待和一次性结果读取；每条腿独立重放校验 | 返回求解轮数；无百分比进度 | 无（返回确定性子单序列） | 无 | 无；返回共享回测产物和来源血缘 | 无百分比进度；返回 Provider lineage 和共享回测摘要  | RunHandle status/cancel/wait/one-shot result; both legs replay-verified |
//! | 产物清单 | 无 | run.json / summary.json / equity.csv / fills.csv | 无（只读） | 无（内存结果） | 每候选四份回测产物 + 实验 JSON 摘要 | 各成功候选的四份深度回测产物 + `<experiment_id>.depth-experiment.json` | 各成功候选的多腿归因产物 + `<experiment_id>.multi-leg-experiment.json` | run.json / summary.json / equity.csv / fills.csv | 无（只读） | ControlPlane 哈希审计、原有 EventLog 与 Ledger | 各成功账户各自的 run.json / summary.json / equity.csv / fills.csv；无批次共享产物 | `<run_id>.multi-leg.json`（两腿重放、成交归因、裸腿待对账） | 无（返回确定性计划，不提交订单） | 无（仅返回规划结果，不提交订单） | 无（计划结果，不下单） | 冻结 BarFrame + DatasetManifestV2 sidecar + run.json/summary.json/equity.csv/fills.csv | 冻结 Provider DepthFrame + 标准深度回测产物  | `<run_id>.multi-leg.json` with strategy identity, hashes, attribution and pending reconciliation |
//! | 错误类别 | 八类见 `AppErrorCategory` | 同左 | 同左 | 同左 | 同左（候选错误隔离记录） | AppErrorCategory 八类；候选错误隔离，整体规格/存储错误由用例返回 | AppErrorCategory 八类；候选失败隔离，整体规格/存储错误由用例返回 | 同左 | 同左 | 同左（认证拒绝、不可执行命令与订单载荷拒绝在入队前返回） | AppErrorCategory 八类；账户级数据/运行错误隔离返回，批次格式/权限/聚合溢出错误整体返回 | 同左（数据/策略输入、幂等冲突、撮合不变量） | 八类；无历史/不收敛为 FidelityInsufficient | 八类；非法规格、profile、越界与溢出为 InvalidInput | 八类；身份、额度、所有权及重复键错误为 InvalidInput | AppErrorCategory 八类；数据、冻结冲突、校验和回测沿用共享用例错误 | AppErrorCategory 八类；Provider 来源、深度数据或冻结冲突返回错误  | Eight AppError categories; invalid identity/spec/intent and conflicting run identity fail closed |
//! | 能力等级 | R | R | R | R | R | R | R | R | R | P/L | R | R | R | R | R | R | R  | `R` |

pub(crate) mod artifacts;
mod compare_runs;
mod experiment_grid;
mod experiment_split;
pub(crate) mod guard;
mod plan_algo_execution;
mod plan_capital_allocation;
mod plan_risk_parity;
mod run_backtest;
mod run_depth_backtest;
mod run_depth_experiment;
mod run_experiment;
mod run_multi_account_backtest;
mod run_multi_leg_backtest;
mod run_multi_leg_backtest_with_strategy;
mod run_multi_leg_experiment;
mod run_provider_backtest_with_manifest;
mod run_provider_depth_backtest;
mod validate_dataset;
mod validate_trade_control;
mod verify_depth_run;
mod verify_run;

pub use compare_runs::compare_runs;
pub use compare_runs::{CompareRunsResult, CompareRunsSpec, ComparedRun, ComparedRunResult};
pub use experiment_split::{
    ExperimentBaselineReport, ExperimentFoldReport, ExperimentOutOfSampleReport,
    ExperimentSplitPlan,
};
pub use plan_algo_execution::plan_algo_execution;
pub use plan_capital_allocation::plan_capital_allocation;
pub use plan_risk_parity::plan_risk_parity;
pub use run_backtest::run_backtest;
pub use run_backtest::StrategyArtifactIdentity;
pub use run_depth_backtest::run_depth_backtest;
pub use run_depth_backtest::{
    run_depth_backtest_with_strategy, run_depth_backtest_with_strategy_slots,
    start_depth_backtest_with_strategy, start_depth_backtest_with_strategy_slots,
};
pub use run_depth_experiment::run_depth_experiment;
pub use run_depth_experiment::{
    DepthExperimentCandidate, DepthExperimentRankedCandidate, RunDepthExperimentResult,
    RunDepthExperimentSpec, RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION,
};
pub use run_experiment::run_experiment;
pub use run_experiment::{
    ExperimentCandidateResult, ExperimentParameterSpace, RunExperimentResult, RunExperimentSpec,
    EXPERIMENT_BASELINE_NAMES, MAX_EXPERIMENT_CANDIDATES, RUN_EXPERIMENT_SCHEMA_VERSION,
};
pub use run_multi_account_backtest::run_multi_account_backtest;
pub use run_multi_leg_backtest::run_multi_leg_backtest;
pub use run_multi_leg_backtest_with_strategy::run_multi_leg_backtest_with_strategy;
pub use run_multi_leg_experiment::run_multi_leg_experiment;
pub use run_multi_leg_experiment::{
    MultiLegExperimentCandidate, MultiLegExperimentRankedCandidate, RunMultiLegExperimentResult,
    RunMultiLegExperimentSpec, RUN_MULTI_LEG_EXPERIMENT_SCHEMA_VERSION,
};
pub use run_provider_backtest_with_manifest::run_provider_backtest_with_manifest;
pub use run_provider_backtest_with_manifest::{ProviderBacktestOutcome, ProviderBacktestSpec};
pub use run_provider_depth_backtest::run_provider_depth_backtest;
pub use run_provider_depth_backtest::{
    ProviderDepthBacktestOutcome, ProviderDepthBacktestSpec,
    PROVIDER_DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
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
