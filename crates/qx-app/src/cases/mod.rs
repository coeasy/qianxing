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
//! | 项 | `validate_dataset` | `run_backtest` | `verify_run` | `compare_runs` | `run_experiment` | `run_depth_experiment` | `run_multi_leg_experiment` | `run_depth_backtest` | `verify_depth_run` | `validate_trade_control` | `run_multi_account_backtest` | `run_multi_account_depth_backtest` | `run_multi_leg_backtest` | `run_multi_builtin_backtest` | `plan_risk_parity` | `plan_algo_execution` | `plan_capital_allocation` | `run_provider_backtest_with_manifest` | `run_provider_depth_backtest` | `run_multi_leg_backtest_with_strategy` | `run_multi_market_backtest` | `run_multi_market_experiment` | `run_multi_account_joint_backtest` | `run_multi_market_depth_backtest` | `run_multi_market_depth_experiment` | `validate_dataset_manifest` | `paper_available_margin_raw` |
//! | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
//! | 输入 schema | `DatasetSpec` v1 | `BacktestSpec` v1（单标的 Bar，可选冻结 `TradingInstrumentSpec`；可选 `ExecutionCostRules` 绑定费率与延迟，或预加载启用的 `AshareRuleConfig` 绑定 A 股制度（其费率覆盖 maker/taker）；可选 OHLCV 撮合模型 `next_bar_open`/`best_price`/`one_tick_slippage`（后一项要求 `price_tick`）；可选 `BacktestRiskRules` 静态规则集（max_qty/max_notional/no_short，产品派生禁空自动应用）；可选同账户单腿策略组合 portfolio（多个 Rust/Python 自定义 callback 可占槽，其他槽使用内置策略），按优先级共享资金；组合预算不得超账户初始资金，且暂不支持账户级 protective_exit） | `BacktestOutcome` | `CompareRunsSpec` v1 | `RunExperimentSpec` v1（Bar 参数网格；可选 `strategy_kinds` 快速选择单腿内置策略，或本地 Rust/Python StrategyArtifactIdentity + factory 为每个候选新建自定义策略；两种方式互斥；支持训练窗选策略/参数 + OOS；最多 256 候选） | `RunDepthExperimentSpec` v1（`DepthBacktestSpec` + bounded strategy grid；最多 256 候选） | `RunMultiLegExperimentSpec` v1（MultiLegBacktestSpec + fast_window/slow_window/period/threshold_bps/quantity_raw 参数笛卡尔网格；最多 256 候选；本地 Rust/Python 可用 StrategyArtifactIdentity + factory 为每个候选创建自定义策略，不与 strategy_kinds 混用） | `DepthBacktestSpec v1（L1 Tick 或 L2/L3 OrderBook + Rust 内置策略或本地 Rust/Python 自定义回调；同账户 portfolio 槽位支持多个回调与内置策略混合，回调身份排序进入配置指纹；可选 TradingInstrumentSpec；L2 可选 MarketMakingConfig 双边报价/库存偏移/撤单；衍生品要求 OrderPolicy）` | `DepthBacktestOutcome` | `TradeControlSpec` v1（交易控制 + ControlCommand） | `MultiAccountBacktestSpec` v1（2..=32 个独立账户 `BacktestSpec`；Bar 数据可跨市场但须同结算币种；不做跨账户净额） | `MultiAccountDepthBacktestSpec` v1（2..=32 个独立账户 `DepthBacktestSpec`；L1 Tick/L2 OrderBook 可跨市场，同结算币种，不做跨账户净额） | `MultiLegBacktestSpec` v1（两份可错位 BarFrame + 确定性合并行情时间线；同一账户的共享 Ledger、购买力、资金风险准入、撮合与 replay；常量资金费或 Provider 历史费率于 UTC 8 小时边界按已观察标价入账；两个初始资金字段合并为账户初始现金，资金不按腿隔离） | `MultiBuiltinBacktestSpec` v1（双腿对齐 BarFrame、独立 `BacktestSpec` 账户/本金/market spec/保证金/成本/撮合/风控） | `RiskParitySpec` v1（最多 64 资产、冻结对齐收益率、资金与换手上限） | `AlgoExecutionSpec` v1（TWAP 子单数或冻结 VWAP 成交量 profile） | `CapitalAllocationSpec` v1（预算、账户现金与带优先级策略请求） | `ProviderBacktestSpec`（DataRequest + BacktestSpec + dataset identity） | `ProviderDepthBacktestSpec v1（单标的 Quote DataRequest + DepthBacktestSpec；冻结深度帧嵌入来源身份）` | `MultiLegBacktestSpec` v1 + `StrategyArtifactIdentity` (Rust/Python callback) | `MultiMarketBacktestSpec` v1（1..=32 个异步 Bar 市场；同一账户/结算币；每市场 1..=16 个策略槽，支持 Rust/Python 本地 callback 与内置策略混合；显式优先级与预算；现货/合约订单策略、资金与风险约束由统一 Rust Bar 引擎校验） | `RunMultiMarketExperimentSpec` v1（共享账户多市场 Bar 规格 + 目标策略槽 + 有界参数/策略种类网格；最多 256 个候选） | `MultiAccountJointBacktestSpec` v1（OKX/Binance/Bitget，2..=32 个独立交易所账户组、最多 256 个 Bar 标的；账户 Ledger 独立，聚合权益与成交，不跨账户保证金净额或换汇） | `MultiMarketDepthBacktestSpec` v1（2..=32 个异步 L2 OrderBook 现货市场；同一账户、结算币和 Rust 共享 Ledger；每市场一个内置或 Rust/Python 本地策略槽；目前不支持跨市场衍生品保证金） | shared Rust use case; <=256 candidates; deterministic ranking and cancellable RunHandle | `DatasetManifestValidationSpec` v1（宽松诊断 BarFrame 并发布/复核 DatasetManifestV2） | `Ledger` + 账户/币种 + marks + 冻结多市场规格 + FX |
//! | 输出 schema | `DatasetVerdict` | `BacktestOutcome` | `VerificationResult` | `CompareRunsResult` v1 | `RunExperimentResult` v1 | `RunDepthExperimentResult` v1（候选结果/错误 + 收益降序、回撤升序确定性排名） | `RunMultiLegExperimentResult` v1（逐候选结果/错误 + 按组合收益确定性排名 + 实验摘要产物；自定义策略 ID/version/SHA-256 纳入实验指纹） | `DepthBacktestOutcome` v1 | `VerificationResult` | 经验证的 `ControlCommand`，交给现有 Rust control submitter | MultiAccountBacktestResult v1（每账户独立 outcome/error + 成功账户本金加权收益摘要；失败账户隔离） | `MultiAccountDepthBacktestResult` v1（每账户 outcome/error + 成功账户本金加权收益摘要；失败账户隔离） | `MultiLegBacktestOutcome` v1（双腿哈希、组合收益、归因产物路径） | `MultiBuiltinBacktestRun`（两腿结果、ReplayVerifier 验证报告与目标时间表） | `RiskParityResult` v1（目标权重、名义金额、调仓增量与计划哈希） | `AlgoExecutionPlan` v1（parent-linked 子单和调度时间） | `CapitalAllocationResult` v1（确定性分配） | `ProviderBacktestOutcome` v1（清单、校验裁定和回测结果） | `ProviderDepthBacktestOutcome v1（共享深度结果 + Provider/request/retry/source_hash 与冻结输入路径）` | `MultiLegBacktestOutcome` v1 (both leg hashes, combined return, attribution artifact) | `MultiMarketBacktestOutcome` v1（共享账簿哈希、聚合收益/回撤、每市场成交数、可重放产物及 RunManifest 路径） | `RunMultiMarketExperimentResult` v1（候选结果/错误 + 收益降序、回撤升序稳定排名） | MultiAccountJointBacktestOutcome v1（联合权益/成交产物 + 每个交易所账户独立的 MultiMarketBacktestOutcome） | `MultiMarketDepthBacktestOutcome` v1（共享结果哈希、市场/成交/权益摘要、各市场实际撮合参数与假设、回放产物） | shared Rust use case; <=256 candidates; deterministic ranking and cancellable RunHandle | `DatasetManifestValidationResult` v1（DatasetManifestV2、JSON、幂等标志、指纹退化原因） | `i128`（可用保证金 raw 值） |
//! | 运行权限 | `RESEARCH` | `RESEARCH` | 无（纯读产物） | 无（纯计算） | `RESEARCH` | `RESEARCH` | `RESEARCH` | `RESEARCH` | 无（纯读产物） | 必须满足认证授予的 Trading 权限 | RESEARCH | `RESEARCH` | `RESEARCH` | Research（R）；明确传入 `CallerCapability::Research` | `RESEARCH` | `RESEARCH` | RESEARCH | `RESEARCH` | `RESEARCH` | `RESEARCH` | `RESEARCH` | `RESEARCH` | RESEARCH | `RESEARCH` | shared Rust use case; <=256 candidates; deterministic ranking and cancellable RunHandle | `RESEARCH` | `PAPER` |
//! | 幂等键 | `dataset_id` + 内容指纹 | `run_id` + `config_hash` + `data_fingerprint` | `run_id` | 完整输入文档（纯计算） | `experiment_id` + 规格指纹；自定义策略身份另绑定 ID/version/SHA-256；冲突在子运行前拒绝 | `experiment_id` + 完整规格指纹；异规格在候选启动前拒绝 | `experiment_id` + 完整规格指纹；自定义策略身份另绑定 ID/version/SHA-256；不同规格在启动候选前拒绝 | `run_id` + 规格/盘口指纹；产物冲突即拒绝 | `run_id` | `command_id` + `request_id`，由 ControlPlane 拒绝重复项 | batch_id + 按 account_id 排序的规格指纹（忽略本地数据/产物路径）；成员 run_id 唯一，数据内容由子结果指纹标识 | batch_id + 按 account_id 排序的深度规格指纹（忽略本地路径）；成员 run_id 唯一，数据内容由子结果指纹标识 | `run_id` + 规格与双 BarFrame 指纹；异内容拒绝覆盖 | 每腿 run_id + config_hash + data_fingerprint；信号时间表由策略规格与双帧确定性推导 | `portfolio_id` + 完整规格（纯计算） | `parent_id` + 完整规格（纯计算） | portfolio_id + 完整规格（纯计算） | 数据帧及清单内容逐字节相同允许重跑；同路径异内容返回 Conflict | `run_id` + 深度规格/Provider source_hash；同路径异内容 Conflict | `run_id` + spec/data fingerprints + strategy ID/version/source SHA-256 | `run_id` + 规范规格指纹 + 所有 BarFrame 内容指纹；异内容拒绝覆盖 | `experiment_id` + 规范规格/排序市场数据指纹；异内容拒绝覆盖 | `run_id` + 规范规格与 exchange/account 标识及各子市场 BarFrame 指纹；相同身份幂等，异内容拒绝覆盖 | `run_id` + 规范规格与排序后所有 DepthFrame 指纹 + 策略 ID/version/source SHA-256；异内容拒绝覆盖 | shared Rust use case; <=256 candidates; deterministic ranking and cancellable RunHandle | dataset_id + version + 内容指纹 + 输出路径；同内容复核，异内容 Conflict | 账簿与估值输入（纯读） |
//! | 取消行为 | 无取消点（同步纯读） | 同步 run_backtest 不取消；BacktestSpec::start 通过 Rust 引擎逐 Bar 协作取消，取消不返回部分报告或产物 | 无取消点 | 无取消点 | RunExperimentSpec::start 与 start_with_strategy_factory 在候选之间及候选 Bar 内核协作取消，不发布部分实验报告 | `RunDepthExperimentSpec::start` 在候选间与 Tick/快照内核循环协作取消，不发布部分实验报告 | RunMultiLegExperimentSpec::start 与 start_with_strategy_factory 在候选之间和候选双腿撮合循环协作取消，不发布部分实验报告 | 同步 run_depth_backtest 不取消；DepthBacktestSpec::start 与 start_depth_backtest_with_strategy_slots 通过 Rust 引擎逐 Tick/快照协作取消，取消不返回部分报告或产物 | 无取消点（同步纯读） | 无取消点；验证后由原有命令队列和 worker 驱动终态 | 同步入口不取消；两种多账户规格的 start 在账户间与对应 Rust Bar/深度内核内协作取消，不返回部分批次结果；单账户失败隔离，最多 32 个成员 | 同步入口不取消；`MultiAccountDepthBacktestSpec::start` 在账户间及 Tick/OrderBook 内核协作取消，不返回部分批次结果；最多 32 个成员 | MultiLegBacktestSpec::start 在信号与双腿撮合边界协作取消，不发布部分归因产物 | 同步用例，无取消点；逐腿复用标准 BarBacktestEngine 和 ReplayVerifier | 同步有界求解，无取消点（最多 10000 轮） | 同步有限计算，无取消点（最多 10000 个子单） | 无取消点（最多 1024 个账户与 10000 个请求） | 同步 Provider 获取由 adapter deadline 控制；冻结与内核运行同步不可取消 | 同步 Provider 获取与深度内核无取消点，外部 deadline 由 Provider adapter 控制 | Sync run has no cancellation; RunHandle checks between signals and Rust leg matching | `MultiMarketBacktestSpec::start` 与 `start_with_strategy_slots` 在准备阶段与共享 Rust session 每个事件批次协作取消；取消不写部分产物 | 同步入口不取消；`RunMultiMarketExperimentSpec::start` 在候选之间和共享 Rust 市场事件批次检查取消，不发布部分报告 | `MultiAccountJointBacktestSpec::start` 在账户间与共享 Rust Bar 事件批次协作取消，不写部分联合产物 | 同步入口不取消；`MultiMarketDepthBacktestSpec::start` 与 `start_with_strategy_slots` 在市场准备和 Rust 快照循环协作取消，不发布部分产物 | shared Rust use case; <=256 candidates; deterministic ranking and cancellable RunHandle | 同步文件读写，无取消点；失败不覆盖既有清单 | 同步纯读，无取消点 |
//! | 事件/进度 | 无 | RunHandle 提供 run_id、状态、取消、等待和一次性结果读取；无百分比进度 | 无 | 无 | 逐候选结果 + RunHandle 生命周期；无百分比进度 | 逐候选记录结果与错误；RunHandle 提供状态/取消/等待/一次性结果读取，无百分比进度 | 逐候选记载成功与稳定 AppError JSON；RunHandle 提供状态/取消/等待/一次性结果读取，无百分比进度 | RunHandle 提供 run_id、状态、取消、等待和一次性结果读取；无百分比进度；结果绑定 Rust 撮合报告和 ReplayVerifier 重放证据 | 无 | 返回 `Accepted` 审计回执；执行进度/终态复用 API 审计读面 | 每账户完成后记录 outcome 或稳定 AppError JSON；无百分比进度 | 每账户完成后记录 outcome 或稳定 AppError JSON；RunHandle 提供状态/取消/等待/一次性结果，无百分比进度 | RunHandle 提供 run_id、状态、取消、等待和一次性结果读取；每条腿独立重放校验 | 无 | 返回求解轮数；无百分比进度 | 无（返回确定性子单序列） | 无 | 无；返回共享回测产物和来源血缘 | 无百分比进度；返回 Provider lineage 和共享回测摘要 | RunHandle status/cancel/wait/one-shot result; both legs replay-verified | RunHandle 提供状态/取消/等待/一次性结果；逐市场决策进入同一 session EventLog，无百分比进度 | 逐候选结果与错误隔离；RunHandle 提供状态/取消/等待/一次性结果，无百分比进度 | RunHandle 提供状态、取消、等待和一次性结果；联合权益/成交汇总并保留各账户结果 | RunHandle 提供状态、取消、等待、事件与一次性结果；无百分比进度 | shared Rust use case; <=256 candidates; deterministic ranking and cancellable RunHandle | 无 | 无 |
//! | 产物清单 | 无 | run.json / summary.json / equity.csv / fills.csv | 无（只读） | 无（内存结果） | 每候选四份回测产物 + 实验 JSON 摘要 | 各成功候选的四份深度回测产物 + `<experiment_id>.depth-experiment.json` | 各成功候选的多腿归因产物 + `<experiment_id>.multi-leg-experiment.json` | run.json / summary.json / equity.csv / fills.csv | 无（只读） | ControlPlane 哈希审计、原有 EventLog 与 Ledger | 各成功账户各自的 run.json / summary.json / equity.csv / fills.csv；无批次共享产物 | 各成功账户各自的 run.json / summary.json / equity.csv / fills.csv；无批次共享产物 | `<run_id>.multi-leg.json`（两腿重放、成交归因、裸腿待对账） | 每条腿独立 run.json / summary.json / equity.csv / fills.csv；应用层返回组合收益、分组归因、成本与裸腿闭合事实，CLI 适配层负责历史摘要/证据文件兼容落盘 | 无（返回确定性计划，不提交订单） | 无（仅返回规划结果，不提交订单） | 无（计划结果，不下单） | 冻结 BarFrame + DatasetManifestV2 sidecar + run.json/summary.json/equity.csv/fills.csv | 冻结 Provider DepthFrame + 标准深度回测产物 | `<run_id>.multi-leg.json` with strategy identity, hashes, attribution and pending reconciliation | `<run_id>.multi-market.json`（市场与自定义策略身份指纹、共享权益序列、成交/事件/账簿事实和 replay 结论） | 不可变实验摘要与各候选共享多市场回测产物 | `<run_id>.joint-portfolio.json`、`<run_id>.joint-equity.json`、`<run_id>.joint-fills.json`；各账户另保留共享市场内核产物 | `<run_id>.multi-market-depth.json`（市场身份、共享账簿/事件、逐市场成交与 Replay 校验） | shared Rust use case; <=256 candidates; deterministic ranking and cancellable RunHandle | 指定 output_path 的 DatasetManifestV2；相同既有清单只读复核 | 无 |
//! | 错误类别 | 八类见 `AppErrorCategory` | 同左 | 同左 | 同左 | 同左（候选错误隔离记录） | AppErrorCategory 八类；候选错误隔离，整体规格/存储错误由用例返回 | AppErrorCategory 八类；候选失败隔离，整体规格/存储错误由用例返回 | 同左 | 同左 | 同左（认证拒绝、不可执行命令与订单载荷拒绝在入队前返回） | AppErrorCategory 八类；账户级数据/运行错误隔离返回，批次格式/权限/聚合溢出错误整体返回 | AppErrorCategory 八类；规格/权限/聚合溢出整体失败，账户级数据与回测错误隔离返回 | 同左（数据/策略输入、幂等冲突、撮合不变量） | AppErrorCategory 八类；双帧标的/时间戳不对齐为 InvalidInput，样本不足为 FidelityInsufficient | 八类；无历史/不收敛为 FidelityInsufficient | 八类；非法规格、profile、越界与溢出为 InvalidInput | 八类；身份、额度、所有权及重复键错误为 InvalidInput | AppErrorCategory 八类；数据、冻结冲突、校验和回测沿用共享用例错误 | AppErrorCategory 八类；Provider 来源、深度数据或冻结冲突返回错误 | Eight AppError categories; invalid identity/spec/intent and conflicting run identity fail closed | AppError 八类；规格、数据、资金风险与 replay 错误沿用共享内核并带 correlation id | AppErrorCategory 八类；规格/网格/数据错误为 InvalidInput，异身份产物为 Conflict | AppErrorCategory 八类；交易所/venue/结算币不匹配为 InvalidInput，异身份产物为 Conflict | 八类 `AppErrorCategory`；输入、资金与回放错误由共享 Rust 内核返回；当前衍生品规格明确拒绝 | shared Rust use case; <=256 candidates; deterministic ranking and cancellable RunHandle | AppError 八类；输入为 InvalidInput/DataUnavailable，写入为 StorageFailure，清单占位/漂移为 Conflict | AppError 八类；权限拒绝和领域估值错误带 correlation id |
//! | 能力等级 | R | R | R | R | R | R | R | R | R | P/L | R | `R` | R | R | R | R | R | R | R | `R` | `R` | `R` | R | `R` | shared Rust use case; <=256 candidates; deterministic ranking and cancellable RunHandle | R | P |

pub(crate) mod artifacts;
mod ashare_binding;
mod compare_runs;
mod experiment_fold_search;
mod experiment_grid;
mod experiment_search;
mod experiment_spec_artifact;
mod experiment_split;
pub(crate) mod guard;
pub(crate) mod kernels;
mod paper_available_margin_raw;
mod plan_algo_execution;
mod plan_capital_allocation;
mod plan_risk_parity;
pub(crate) mod risk_gate;
mod run_backtest;
mod run_depth_backtest;
mod run_depth_experiment;
mod run_experiment;
mod run_multi_account_backtest;
mod run_multi_account_depth_backtest;
mod run_multi_account_joint_backtest;
mod run_multi_builtin_backtest;
mod run_multi_leg_backtest;
mod run_multi_leg_backtest_with_funding_provider;
mod run_multi_leg_backtest_with_strategy;
mod run_multi_leg_experiment;
mod run_multi_market_backtest;
mod run_multi_market_depth_backtest;
mod run_multi_market_depth_experiment;
mod run_multi_market_experiment;
mod run_multi_market_manifest;
mod run_provider_backtest_with_manifest;
mod run_provider_depth_backtest;
mod validate_dataset;
mod validate_dataset_manifest;
mod validate_depth_dataset_manifest;
mod validate_trade_control;
mod verify_depth_run;
mod verify_run;

pub use ashare_binding::{bind_ashare_backtest_rules, AshareBacktestBinding};
pub use compare_runs::compare_runs;
pub use compare_runs::{CompareRunsResult, CompareRunsSpec, ComparedRun, ComparedRunResult};
pub use experiment_search::{
    ExperimentCandidatePruning, ExperimentEarlyPruningPlan, ExperimentFoldSearchReport,
    ExperimentNeighborhoodAssessment, ExperimentNeighborhoodReport, ExperimentParameterRange,
    ExperimentSearchAlgorithm, ExperimentSearchObjective, ExperimentSearchPlan,
    ExperimentSearchReport, ExperimentSearchTrial,
};
pub use experiment_split::{
    ExperimentBaselineReport, ExperimentCrossFoldRobustness, ExperimentFoldReport,
    ExperimentOutOfSampleReport, ExperimentSplitPlan, EXPERIMENT_BASELINE_NAMES,
};
pub use paper_available_margin_raw::paper_available_margin_raw;
pub use plan_algo_execution::plan_algo_execution;
pub use plan_capital_allocation::plan_capital_allocation;
pub use plan_risk_parity::plan_risk_parity;
pub(crate) use risk_gate::backtest_risk_gate;
pub use run_backtest::run_backtest;
pub use run_backtest::StrategyArtifactIdentity;
pub(crate) use run_backtest::{bind_execution, bind_margin};
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
    ExecutionCostSensitivityFoldDelta, ExecutionCostSensitivityNeighborResult,
    ExecutionCostSensitivityNeighborhoodResult, ExecutionCostSensitivityResult,
    ExecutionCostSensitivityScenario, ExperimentCandidateResult, ExperimentParameterSpace,
    RunExperimentResult, RunExperimentSpec, MAX_EXPERIMENT_CANDIDATES,
    RUN_EXPERIMENT_SCHEMA_VERSION,
};
pub use run_multi_account_backtest::run_multi_account_backtest;
pub use run_multi_account_depth_backtest::run_multi_account_depth_backtest;
pub use run_multi_account_joint_backtest::run_multi_account_joint_backtest;
pub use run_multi_builtin_backtest::{
    run_multi_builtin_backtest, run_multi_builtin_backtest_with_inputs,
    validate_multi_builtin_frames, MultiBuiltinBacktestInputs,
};
pub use run_multi_leg_backtest::run_multi_leg_backtest;
pub use run_multi_leg_backtest_with_funding_provider::FUNDING_RATE_JSON_CONTENT_TYPE;
pub use run_multi_leg_backtest_with_strategy::run_multi_leg_backtest_with_strategy;
pub use run_multi_leg_experiment::run_multi_leg_experiment;
pub use run_multi_leg_experiment::{
    MultiLegExperimentCandidate, MultiLegExperimentRankedCandidate, RunMultiLegExperimentResult,
    RunMultiLegExperimentSpec, RUN_MULTI_LEG_EXPERIMENT_SCHEMA_VERSION,
};
pub use run_multi_market_backtest::run_multi_market_backtest;
pub use run_multi_market_depth_backtest::run_multi_market_depth_backtest;
pub(crate) use run_multi_market_depth_backtest::run_multi_market_depth_backtest_with_strategy_slots;
pub use run_multi_market_depth_experiment::run_multi_market_depth_experiment;
pub use run_multi_market_depth_experiment::{
    MultiMarketDepthExperimentCandidate, MultiMarketDepthExperimentRankedCandidate,
    RunMultiMarketDepthExperimentResult, RunMultiMarketDepthExperimentSpec,
    MAX_MULTI_MARKET_DEPTH_EXPERIMENT_CANDIDATES, RUN_MULTI_MARKET_DEPTH_EXPERIMENT_SCHEMA_VERSION,
};
pub use run_multi_market_experiment::run_multi_market_experiment;
pub use run_multi_market_experiment::{
    MultiMarketExperimentCandidate, MultiMarketExperimentRankedCandidate,
    RunMultiMarketExperimentResult, RunMultiMarketExperimentSpec,
    MAX_MULTI_MARKET_EXPERIMENT_CANDIDATES, RUN_MULTI_MARKET_EXPERIMENT_SCHEMA_VERSION,
};
pub use run_provider_backtest_with_manifest::run_provider_backtest_with_manifest;
pub use run_provider_backtest_with_manifest::{ProviderBacktestOutcome, ProviderBacktestSpec};
pub use run_provider_depth_backtest::run_provider_depth_backtest;
pub use run_provider_depth_backtest::{
    ProviderDepthBacktestOutcome, ProviderDepthBacktestSpec,
    PROVIDER_DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
};
pub use validate_dataset::validate_dataset;
pub use validate_dataset_manifest::validate_dataset_manifest;
pub use validate_dataset_manifest::DatasetManifestValidationResult;
pub use validate_depth_dataset_manifest::{
    validate_depth_dataset_manifest, DepthDatasetManifestValidationResult,
};
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

/// Derive the initial strategy buying-power value from seeded cash through the shared
/// `ValuationResult.available` relation. Event-time refreshes replace this seed before decisions.
pub(crate) fn strategy_available_margin_raw(
    cash: &std::collections::BTreeMap<String, i128>,
    currency: &str,
) -> Option<i128> {
    let equity = *cash.get(currency)?;
    qx_core::ValuationResult::new(
        qx_core::Money::from_raw(equity),
        qx_core::Money::ZERO,
        qx_core::Money::ZERO,
        currency,
    )
    .ok()
    .map(|valuation| valuation.available.raw())
}
