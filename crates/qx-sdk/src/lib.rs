//! qx-sdk：牵星面向第三方的稳定 Rust 门面（方案 §6.1）。
//!
//! ## 它解决什么问题
//!
//! `qx-app` 是应用层，它的公开面按**用例**组织、为**三个门面**（CLI / HTTP / Python）服务。
//! 第三方嵌入方要的却是另一件事：一个**稳定、有 semver 承诺、不需要理解内部 crate 布局**的
//! 入口。此前这条边界不存在——集成方只能自己挑 `qx-app` 加若干领域 crate 拼，拼错了没人拦。
//!
//! 本 crate 就是那条边界。它**不重写任何业务逻辑**：`qx-app` 的用例、规格、错误、运行句柄
//! 在这里被原样重导出，另加一个 [`Client`] 把调用方身份收成一个值。
//!
//! ```text
//! 第三方 ─→ qx-sdk ─→ qx-app ─→ 领域/端口
//! ```
//!
//! ## 与 `qx-app` 的关系（如实说清）
//!
//! - 门面是 `qx-app` 公开面的**超集**：`qx-app` 重导出的每一项在这里都够得着，
//!   所以集成方**不需要**再单独依赖 `qx-app`。
//! - 门面公开多源 Provider 注册/查询/结果类型与原始血缘类型，也公开策略注册表类型与执行
//!   扩展端口：它们是构造 `qx-app` 输入、进行确定性 Provider failover、接入共享 Rust
//!   TWAP/VWAP 状态机所需的稳定边界，不在门面里复制策略、风险或订单逻辑。
//! - TWAP/VWAP 父单状态可从稳定门面创建、序列化，并通过 `ExecutionGateway` 推进；
//!   持久化、单飞调度和 Paper/Live worker 由运行时负责；RemoteApiClient 可调用服务端 Paper 生命周期，但不代表 SDK 自托管 worker。
//! - `qx-app` 的 API 变动会**同时**是门面的变动——门面刻意不做版本隔离层，那会变成第二份
//!   语义。门面的 semver 承诺就是"跟着 `qx-app` 一起动，但**变动必须是有意的**"。
//!
//! ## §6.1 能力清单：哪些有、哪些没有
//!
//! 方案 §6.1 点名了十一项。**八项在这里有实物**：`Client/Engine`（[`Client`]）、
//! capabilities（[`ProviderCapability`]）、spec 构造校验（各 `*Spec`）、sync run（`run_*`）、
//! async run（各 `*Spec::start` 返回 [`RunHandle`]）、`RunHandle`（[`RunHandle`] /
//! [`RunStatus`]）、cancel/wait/result（[`RunHandle::cancel`] / [`RunHandle::wait`] /
//! [`RunHandle::try_take_result`]）、report/verify（[`verify_run`] / [`verify_depth_run`] /
//! [`compare_runs`]）、Provider/Venue 扩展端口（[`DataProvider`] / [`VenuePort`]）、
//! 统一错误（[`AppError`] 一族）。
//!
//! **事件读面**：[`RemoteApiClient::get_live_events`] 按服务端序号游标读取 `qx-api` 的实时事件，
//! 远端游标沿用服务端 snapshot-recovery；本地 [`RunHandle::events_after`] 从拥有运行的 Rust
//! worker 读取有序生命周期事件。它目前不等同于持久化的订单/成交事件日志。
//!
//! 另外两点如实记下，免得被读成"已经有了"：
//! - **"异步"是句柄式不是 `Future` 式**：`qx-app` 的引擎是线程 + 通道，没有 executor。
//! - **发布验收未做**：`cargo package` 与临时 registry 消费、路径依赖转版本依赖都还没跑。
//!
//! ## 上手与迁移
//!
//! 最小上手见 `examples/quickstart.rs`（它由 `cargo test --workspace` 真编译，
//! 不是文档里的假想代码）；从"直接用 `qx-app`"迁到本门面见 `docs/qx-sdk-migration.md`。

pub mod client;
pub mod remote;

pub use client::Client;
pub use qx_execution::{
    plan_twap, plan_vwap, AlgoChildSlice, AlgoExecutionError, AlgoExecutionState,
    AlgoExecutionStatus, AlgoExecutionStep, AlgoParentOrder, EventAppender, ExecutionEvent,
    ExecutionEventEnvelope, ExecutionEventPort, ExecutionGateway, Fill, InstrumentId, LedgerProbe,
    Order, OrderPolicy, OrderStatus, OrderStore, OrderTrace, PortExecutionResult, Price, Quantity,
    RiskPort, RiskVerdict, Side, TradingInstrumentSpec, TradingProduct, VenueCapabilities,
    VenueMarketScope, VenueOrderType, VenuePort, VenueRouterPort, VwapVolumeBucket,
    MAX_ALGO_CHILDREN, PAPER_VENUE_CAPABILITIES,
};
pub use remote::{RemoteApiClient, RemoteApiError, RemoteResponse, RemoteTransport};

// —— §6.1「统一错误」——
pub use qx_app::{builtin_strategy_catalog, BuiltinStrategyInfo};
pub use qx_app::{AppAction, AppError, AppErrorCategory, AppRetry};

// —— §6.1「capabilities」与多源数据 Provider 端口 ——
// DataProvider 的公开签名直接使用 RawRecord，因此同时重导出来源身份和记录类型；
// 独立消费者只依赖 qx-sdk 即可注册数据源、做确定性主备选择并保留血缘。
pub use qx_app::{
    ProviderBacktestOutcome, ProviderBacktestSpec, ProviderDepthBacktestOutcome,
    ProviderDepthBacktestSpec, FUNDING_RATE_JSON_CONTENT_TYPE,
};
pub use qx_provider::{
    DataKind, DataProvider, DataQuery, DataRequest, DataRequirements, DataSourceId,
    DatasetManifestV2, MarketDataTier, ProviderBarFrame, ProviderCapability, ProviderError,
    ProviderErrorClass, ProviderPayload, ProviderRegistry, ProviderResult, RawRecord,
    RegistryError, PROVIDER_RESULT_SCHEMA_VERSION,
};

// —— §6.1「spec 构造校验」——
// `BuiltinStrategyKind` 来自 qx-strategy：它是 `BuiltinStrategySpec::new` 的参数类型，
// 而 qx-app 没有重导出它（见 Cargo.toml 里的说明）。
pub use qx_app::{
    screen_bar_frames, AlgoExecutionMethod, AlgoExecutionPlan, AlgoExecutionSpec,
    BacktestDatasetInput, BacktestSpec, BuiltinStrategySpec, CapitalAllocationAccount,
    CapitalAllocationRequest, CapitalAllocationResult, CapitalAllocationSpec, CompareRunsSpec,
    DatasetSpec, DepthBacktestSpec, MakerTakerFeeSchedule, MultiAccountBacktestSpec,
    MultiAccountDepthBacktestSpec, MultiLegBacktestSpec, MultiLegDatasetInput, MultiLegFillModel,
    MultiMarketBacktestMarket, MultiMarketBacktestSpec, MultiMarketDepthBacktestMarket,
    MultiMarketDepthBacktestOutcome, MultiMarketDepthBacktestSpec, PortfolioBacktestConfig,
    PortfolioStrategySpec, RiskParityAssetSpec, RiskParityResult, RiskParitySpec,
    RunDepthExperimentSpec, RunExperimentSpec, RunMultiLegExperimentSpec,
    RunMultiMarketDepthExperimentSpec, RunMultiMarketExperimentSpec, StrategyArtifactIdentity,
    TradeControlSpec, TradeWorkflow, ALGO_EXECUTION_SPEC_SCHEMA_VERSION,
    RISK_PARITY_RESULT_SCHEMA_VERSION, RISK_PARITY_SPEC_SCHEMA_VERSION,
};
pub use qx_strategy::{
    BuiltinStrategyKind, MarketEvent, Strategy, StrategyContext, StrategyDecision,
    StrategyOrderIntent, STRATEGY_API_VERSION,
};

// —— §6.1「sync run」——
pub use qx_app::{
    compare_runs, plan_algo_execution, plan_capital_allocation, plan_risk_parity, run_backtest,
    run_depth_backtest, run_depth_experiment, run_experiment, run_multi_account_backtest,
    run_multi_account_depth_backtest, run_multi_leg_backtest, run_multi_leg_backtest_with_strategy,
    run_multi_leg_experiment, run_multi_market_backtest, run_multi_market_depth_backtest,
    run_multi_market_depth_backtest_with_strategy_slots, run_multi_market_depth_experiment,
    run_multi_market_experiment, validate_dataset, validate_trade_control,
};

// —— §6.1「report/verify」——
pub use qx_app::{verify_depth_run, verify_run};

// —— §6.1「RunHandle / cancel / wait / result」——
// `DEFAULT_WAIT_TIMEOUT` 是 `wait()` 的有限缺省（方案 §9）：门面把它摆出来，
// 调用方才能判断"等多久算正常"。
pub use qx_app::{RunEvent, RunEventKind, RunHandle, RunStatus, DEFAULT_WAIT_TIMEOUT};

// —— 调用上下文与能力档 ——
pub use qx_app::{CallerCapability, RunContext};

// —— 结果与产物类型 ——
pub use qx_app::{
    BacktestArtifacts, BacktestOutcome, CompareRunsResult, ComparedRun, ComparedRunResult,
    DatasetVerdict, DepthBacktestOutcome, DepthExperimentCandidate, DepthExperimentRankedCandidate,
    ExperimentBaselineReport, ExperimentCandidateResult, ExperimentFoldReport,
    ExperimentOutOfSampleReport, ExperimentParameterSpace, ExperimentSplitPlan,
    MultiAccountBacktestResult, MultiAccountBacktestSummary, MultiAccountDepthBacktestResult,
    MultiAccountDepthBacktestSummary, MultiAccountDepthRunResult, MultiAccountRunResult,
    MultiLegBacktestOutcome, MultiLegExperimentCandidate, MultiLegExperimentRankedCandidate,
    MultiMarketBacktestOutcome, MultiMarketDepthExperimentCandidate,
    MultiMarketDepthExperimentRankedCandidate, MultiMarketExperimentCandidate,
    MultiMarketExperimentRankedCandidate, RunDepthExperimentResult, RunExperimentResult,
    RunMultiLegExperimentResult, RunMultiMarketDepthExperimentResult,
    RunMultiMarketExperimentResult, VerificationResult,
};

// —— 多腿归因/完整性读数（qx-app 的公开读面）——
pub use qx_app::{
    multi_leg_bar_close_raw, multi_leg_cost_bps, multi_leg_group_attributions,
    multi_leg_group_totals, multi_leg_integrity_json, multi_leg_integrity_summary,
    multi_leg_leg_buckets, multi_leg_leg_integrity, multi_leg_leg_margin,
    multi_leg_pending_reconcile_json, multi_leg_realized_standing_at, MultiLegAttribution,
    MultiLegLegFacts, MultiLegLegIntegrity, MultiLegPendingReconcile, MultiLegSignalBucket,
};

// —— 拒绝事实读面（风控拒绝的可观测面）——
pub use qx_app::{rejection_count, rejection_facts, rejection_facts_line, spread_group_id};

// —— 契约版本与硬上限（调用方要按它们做兼容判断）——
pub use qx_app::{
    BACKTEST_SPEC_SCHEMA_VERSION, DATASET_SPEC_SCHEMA_VERSION, DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
    MAX_EXPERIMENT_CANDIDATES, MAX_MULTI_ACCOUNT_RUNS, MAX_MULTI_MARKET_EXPERIMENT_CANDIDATES,
    MIN_BACKTEST_BARS, MULTI_ACCOUNT_BACKTEST_SCHEMA_VERSION,
    MULTI_LEG_BACKTEST_SPEC_SCHEMA_VERSION, MULTI_LEG_FUNDING_INTERVAL_MS,
    RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION, RUN_EXPERIMENT_SCHEMA_VERSION,
    RUN_MULTI_LEG_EXPERIMENT_SCHEMA_VERSION, RUN_MULTI_MARKET_EXPERIMENT_SCHEMA_VERSION,
    TRADE_CONTROL_SCHEMA_VERSION,
};

// —— §6.1「Provider/Venue extension ports」——
// 第三方 venue 通过 VenuePort 接入；数据源则经 ProviderRegistry 选择、获取和校验。
