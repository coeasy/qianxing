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
//! qx-api ─┼─→ qx-app ─→ qx-core / qx-data / qx-guanxing / qx-datastruct / qx-spec / qx-strategy / qx-risk / qx-xingban / qx-zhenlu
//! qx-python ┘
//! ```
//!
#![recursion_limit = "256"]

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
//! - 用例：[`validate_dataset`]、[`validate_dataset_manifest`]、[`run_backtest`]、[`run_multi_account_backtest`]、[`run_multi_account_depth_backtest`]、[`run_multi_account_joint_backtest`]、[`run_depth_backtest`]、[`run_multi_leg_backtest`]、[`run_experiment`]、[`run_multi_leg_experiment`]、[`plan_risk_parity`]、[`screen_bar_frames`]、[`verify_run`]、[`verify_depth_run`]、[`compare_runs`]、[`validate_trade_control`]；策略边界预检：[`validate_strategy_execution_preflight_v1`]；报告投影：[`read_run_report_v1`]、[`read_depth_run_report_v1`]、[`read_event_report_v1_with_reader`]、[`verify_event_replay_v1`]、[`verify_multi_account_research_facts_v1`]；多市场报告复核与身份：[`verify_multi_market_pnl_contributions_v1`]、[`verify_multi_market_bar_input_v1`]、[`verify_multi_market_depth_input_v1`]、[`validate_multi_market_quality_manifest_set_v1`]、[`multi_market_spec_fingerprint_v1`]、[`multi_market_depth_spec_fingerprint_v1`]、[`multi_market_input_set_identity_v1`]、[`multi_market_depth_input_set_identity_v1`]
//! - 输入/输出：[`DatasetSpec`]、[`DatasetVerdict`]、[`DatasetManifestValidationSpec`]、[`DatasetManifestValidationResult`]、[`BacktestSpec`]、[`BacktestOutcome`]、
//!   [`VerificationResult`]、[`CompareRunsSpec`]、[`CompareRunsResult`]
//! - 调用上下文与能力档：[`RunContext`]、[`CallerCapability`]
//! - 错误：[`AppError`]、[`AppErrorCategory`]、[`AppAction`]、[`AppRetry`]
//!
//! Paper/Live 使用 `validate_trade_control` 校验授权能力、工作流和订单命令；API facade 随后将
//! 同一 `ControlCommand` 交给现有 ControlPlane 与 worker，不在 SDK 或应用层另建执行路径。
//! [`report_read::ResearchRunReportV1`] 提供 Bar/深度共用的版本化核心读模型（复核、摘要及有界权益/成交样本），[`report_read::EventReportPageV1`] 将单市场 EventLog 的有界读取、重放和成交身份核对放在同一应用层；[`verify_multi_account_research_facts_v1`] 复算多账户 Bar/Depth 的成功与失败计数、成交总数、本金加权收益、父 summary 和子身份指纹；[`verify_multi_market_pnl_contributions_v1`] 复核多市场逐标的权益贡献与共享权益守恒，[`verify_multi_market_bar_input_v1`] 复核多市场 BarFrame 和可选 DatasetManifestV2，[`verify_multi_market_depth_input_v1`] 复核多市场 DepthFrame 和可选 DatasetManifestV2，[`verify_multi_market_bar_sidecars_v1`] 复核多市场权益指标和 Filled sidecar，[`verify_event_replay_v1`] 复核持久化事件流、EventLog digest 和 replay ledger，[`multi_market_spec_fingerprint_v1`] 与 [`multi_market_depth_spec_fingerprint_v1`] 统一多市场 Bar/深度报告的规格与策略身份指纹，[`multi_market_input_set_identity_v1`] 与 [`multi_market_depth_input_set_identity_v1`] 统一生产与报告复核的数据指纹和 RunManifest 输入组件。报告文件授权与有界读取留在 API adapter；Bar/DepthFrame、质量、重放及多账户汇总事实的业务复核由 qx-app 持有。Bar JSONL 与 `verify_event_replay_v1` 已使用增量重放器，不再同时构造完整 EventLog；多市场事件读取只保留成交总数与序列摘要，分页所需订单归属通过有 SHA-256 复核的二次扫描构建，并受 300 行页大小及 1 MiB 行长上限约束。账户交互投影及其它报告类型仍由 `qx-api` 处理，完整报告读用例尚未覆盖所有产物类型。`strategy_preflight` 将完整 Python 文件策略的参数、搜索空间、市场要求、warmup 与 Bar 文件身份统一交给 Rust 校验。`qx-api` 将远程 Run 状态与终态结果持久化；服务重启后可查询已完成结果，崩溃前仍运行的任务会明确恢复为失败态（worker 本身不跨进程续跑）。
//! 多市场 v1 内嵌 EventLog 与 v2 流式 EventLog 由同一应用事实 owner 复核分页、成交数和订单-标的映射；
//! 旧 Depth 报告的成交 sidecar 继续按原始事件行比较，保持已保存 artifact 的兼容语义。
//! Bar、Tick/OrderBook、多腿回测以及 Bar/深度/多腿参数实验的 `start` 入口返回可协作取消的
//! [`RunHandle`]；候选运行复用对应的 Rust 回测用例与内核。

pub mod account_activity;
pub mod account_query_preflight;
pub mod artifact_store;
pub mod ashare_readout;
pub mod ashare_screen;
pub mod backtest_input_identity;
mod bar_fill_model;
pub mod cases;
pub mod ccxt_backtest_snapshot;
pub mod context;
pub mod dataset_bundle;
pub mod error;
pub mod event_replay;
pub mod execution_capability_preflight;
pub mod execution_cost;
pub mod fast_backtest;
pub mod multi_account_joint_report;
pub mod multi_account_report;
pub mod multi_leg;
pub mod multi_market_depth_research_report;
pub mod multi_market_report;
pub mod multi_market_research_report;
pub mod report_analysis;
pub mod report_read;
mod run_handle;
pub mod spec;
pub mod strategy_backtest_identity;
mod strategy_backtest_workflow;
pub mod strategy_catalog;
mod strategy_engine;
pub mod strategy_order;
pub mod strategy_preflight;
pub mod trade_facts;

#[cfg(test)]
mod tests;

pub use account_activity::{
    read_account_activity_report_v1, AccountActivityCompletenessV1, AccountActivityEventPageV1,
    AccountActivityIdentityV1, AccountActivityReportReadOptionsV1, AccountActivityReportV1,
    AccountActivityRowsV1, ACCOUNT_ACTIVITY_REPORT_SCHEMA_VERSION_V1,
    DEFAULT_ACCOUNT_ACTIVITY_EVENT_PAGE_V1, MAX_ACCOUNT_ACTIVITY_EVENT_PAGE_V1,
    MAX_ACCOUNT_ACTIVITY_SNAPSHOT_ROWS_V1,
};
pub use account_query_preflight::{
    evaluate_account_query_capability_preflight_v1, AccountQueryCapabilityPreflightDecisionV1,
    AccountQueryCapabilityPreflightSpecV1, ACCOUNT_QUERY_PREFLIGHT_SCHEMA_VERSION,
};
pub use artifact_store::{
    verify_multi_account_backtest_result, verify_multi_account_backtest_result_with_reader,
};
pub(crate) use ashare_readout::ashare_analysis_json;
pub use ashare_screen::screen_bar_frames;
pub use bar_fill_model::{
    bar_fill_model, fill_model_failure, fill_model_problem, BarFillModelBinding,
};
pub use cases::kernels::{BAR_MATCHING_KERNEL, ORDER_BOOK_MATCHING_KERNEL, TICK_MATCHING_KERNEL};
pub use cases::{
    bind_ashare_backtest_rules, compare_runs, paper_available_margin_raw, plan_algo_execution,
    plan_capital_allocation, plan_risk_parity, prepare_multi_leg_experiment_artifacts,
    prepare_quick_research_spec_v1, run_backtest, run_depth_backtest,
    run_depth_backtest_with_strategy, run_depth_backtest_with_strategy_slots, run_depth_experiment,
    run_experiment, run_multi_account_backtest, run_multi_account_depth_backtest,
    run_multi_account_joint_backtest, run_multi_builtin_backtest,
    run_multi_builtin_backtest_with_inputs, run_multi_leg_backtest,
    run_multi_leg_backtest_with_strategy, run_multi_leg_experiment, run_multi_market_backtest,
    run_multi_market_depth_backtest, run_multi_market_depth_experiment,
    run_multi_market_experiment, run_provider_backtest_with_manifest, run_provider_depth_backtest,
    start_depth_backtest_with_strategy, start_depth_backtest_with_strategy_slots, validate_dataset,
    validate_dataset_manifest, validate_depth_dataset_manifest, validate_multi_builtin_frames,
    validate_trade_control, verify_depth_run, verify_multi_leg_report_quality_evidence_bytes_v1,
    verify_multi_leg_report_quality_evidence_v1, verify_run, AshareBacktestBinding,
    CompareRunsResult, CompareRunsSpec, ComparedRun, ComparedRunResult,
    DatasetManifestValidationResult, DepthDatasetManifestValidationResult,
    DepthExperimentCandidate, DepthExperimentRankedCandidate, ExecutionCostSensitivityFoldDelta,
    ExecutionCostSensitivityNeighborResult, ExecutionCostSensitivityNeighborhoodResult,
    ExecutionCostSensitivityResult, ExecutionCostSensitivityScenario, ExperimentCandidatePruning,
    ExperimentCandidateResult, ExperimentCrossFoldRobustness, ExperimentEarlyPruningPlan,
    ExperimentFoldSearchReport, ExperimentNeighborhoodAssessment, ExperimentNeighborhoodReport,
    ExperimentParameterRange, ExperimentParameterSpace, ExperimentSearchAlgorithm,
    ExperimentSearchObjective, ExperimentSearchPlan, ExperimentSearchReport, ExperimentSearchTrial,
    MultiBuiltinBacktestInputs, MultiLegExperimentCandidate, MultiLegExperimentRankedCandidate,
    MultiMarketDepthExperimentCandidate, MultiMarketDepthExperimentRankedCandidate,
    MultiMarketExperimentCandidate, MultiMarketExperimentFoldSearchReport,
    MultiMarketExperimentRankedCandidate, MultiMarketExperimentSearchReport,
    PreparedMultiLegExperimentArtifacts, ProviderBacktestOutcome, ProviderBacktestSpec,
    ProviderDepthBacktestOutcome, ProviderDepthBacktestSpec, RunDepthExperimentResult,
    RunDepthExperimentSpec, RunExperimentResult, RunExperimentSpec, RunMultiLegExperimentResult,
    RunMultiLegExperimentSpec, RunMultiMarketDepthExperimentResult,
    RunMultiMarketDepthExperimentSpec, RunMultiMarketExperimentResult,
    RunMultiMarketExperimentSpec, StrategyArtifactIdentity, TradeControlSpec, TradeWorkflow,
    EXPERIMENT_BASELINE_NAMES, FUNDING_RATE_JSON_CONTENT_TYPE, FUNDING_RATE_JSON_CONTENT_TYPE_V2,
    MAX_EXPERIMENT_CANDIDATES, MAX_MULTI_MARKET_DEPTH_EXPERIMENT_CANDIDATES,
    MAX_MULTI_MARKET_EXPERIMENT_CANDIDATES, PROVIDER_DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
    RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION, RUN_EXPERIMENT_SCHEMA_VERSION,
    RUN_MULTI_LEG_EXPERIMENT_SCHEMA_VERSION, RUN_MULTI_MARKET_DEPTH_EXPERIMENT_SCHEMA_VERSION,
    RUN_MULTI_MARKET_EXPERIMENT_SCHEMA_VERSION, TRADE_CONTROL_SCHEMA_VERSION,
};
pub use cases::{
    ExperimentBaselineReport, ExperimentFoldReport, ExperimentOutOfSampleReport,
    ExperimentSplitPlan,
};
pub use context::{CallerCapability, RunContext, APP_DEFAULT_CODE_COMMIT};
pub use error::{AppAction, AppError, AppErrorCategory, AppRetry};
pub use event_replay::{verify_event_replay_v1, VerifiedEventReplayV1};
pub use execution_capability_preflight::{
    evaluate_cancel_execution_capability_preflight_v1, evaluate_execution_capability_preflight_v1,
    ExecutionCapabilityPreflightDecisionV1, ExecutionCapabilityPreflightSpecV1, ExecutionProfile,
    EXECUTION_CAPABILITY_PREFLIGHT_SCHEMA_VERSION,
};
pub use execution_cost::ExecutionCostBinding;
pub use multi_account_joint_report::{
    read_multi_account_joint_report_v1, read_multi_account_joint_report_v1_with_options,
    read_multi_account_joint_report_v1_with_reader,
    read_multi_account_joint_report_v1_with_reader_and_options, MultiAccountJointAccountReportV1,
    MultiAccountJointEquityPointV1, MultiAccountJointEquitySeriesV1,
    MultiAccountJointEventPageRequestV1, MultiAccountJointFillSampleV1,
    MultiAccountJointReportReadOptionsV1, MultiAccountJointResearchReportV1,
    MultiAccountJointResearchSummaryV1, MAX_MULTI_ACCOUNT_JOINT_REPORT_EQUITY_SAMPLE,
    MAX_MULTI_ACCOUNT_JOINT_REPORT_EVENT_PAGE, MAX_MULTI_ACCOUNT_JOINT_REPORT_EVENT_SAMPLE,
    MAX_MULTI_ACCOUNT_JOINT_REPORT_FILL_SAMPLE, MAX_MULTI_ACCOUNT_JOINT_REPORT_ROWS,
    MULTI_ACCOUNT_JOINT_RESEARCH_REPORT_SCHEMA_VERSION_V1,
};
pub use multi_account_report::{
    read_multi_account_report_v1, read_multi_account_report_v1_with_reader,
    verify_multi_account_research_facts_v1, MultiAccountResearchAccountReportV1,
    MultiAccountResearchFactsV1, MultiAccountResearchMemberV1, MultiAccountResearchReportV1,
    MultiAccountResearchStatusV1, MultiAccountResearchSummaryV1,
    MULTI_ACCOUNT_RESEARCH_REPORT_SCHEMA_VERSION_V1,
};
pub use multi_leg::{
    aggregate_multi_leg_quality_report, multi_builtin_composition_summary_json,
    multi_leg_apply_funding, multi_leg_bar_close_raw, multi_leg_combined_equity_metrics,
    multi_leg_combined_exact_equity_metrics, multi_leg_combined_return_bps, multi_leg_cost_bps,
    multi_leg_funding_cost, multi_leg_group_attributions, multi_leg_group_totals,
    multi_leg_initial_cash_raw_v1, multi_leg_integrity_json, multi_leg_integrity_summary,
    multi_leg_leg_buckets, multi_leg_leg_cash_reserve, multi_leg_leg_integrity,
    multi_leg_leg_margin, multi_leg_leg_pnl_exact, multi_leg_leg_return_bps,
    multi_leg_leg_return_bps_exact, multi_leg_leg_return_bps_exact_v2,
    multi_leg_pending_reconcile_json, multi_leg_pending_turnover_raw_v1,
    multi_leg_quality_fingerprint, multi_leg_realized_standing_at,
    multi_leg_shared_account_ledger_summary_exact_v2, multi_leg_shared_account_ledger_summary_v1,
    read_multi_leg_event_report_page_v1, read_multi_leg_event_report_page_v1_with_readers,
    read_multi_leg_report_facts_v1, read_multi_leg_report_facts_v1_with_reader,
    read_multi_leg_report_facts_v1_with_readers,
    read_multi_leg_report_read_model_v1_with_page_and_readers,
    read_multi_leg_report_read_model_v1_with_readers, read_multi_leg_research_report_v1,
    read_multi_leg_research_report_v1_with_readers, verify_multi_leg_cost_facts_v1,
    verify_multi_leg_equity_report_exact_v1,
    verify_multi_leg_equity_report_exact_with_principal_v2, verify_multi_leg_equity_report_v1,
    verify_multi_leg_market_fill_report_v1, verify_multi_leg_research_report_facts_v1,
    verify_multi_leg_shared_account_ledger_exact_v2, verify_multi_leg_shared_account_ledger_v1,
    MultiBuiltinCompositionSummary, MultiLegAttribution, MultiLegCombinedEquity, MultiLegCostFacts,
    MultiLegCostVerificationFacts, MultiLegEquityReportFactsV1, MultiLegEventPageRequestV1,
    MultiLegEventReportPageV1, MultiLegEventReportReadRequestV1, MultiLegLegFacts,
    MultiLegLegIntegrity, MultiLegLegReportFactsV1, MultiLegLegReturnFacts,
    MultiLegMarketFillReportFactsV1, MultiLegPendingReconcile, MultiLegReportCostFactsV1,
    MultiLegReportRowsV1, MultiLegResearchReportCostSummaryV1,
    MultiLegResearchReportEquitySummaryV1, MultiLegResearchReportExecutionV1,
    MultiLegResearchReportFactsV1, MultiLegResearchReportInputV1,
    MultiLegResearchReportLegSummaryV1, MultiLegResearchReportMarketV1,
    MultiLegResearchReportReadModelV1, MultiLegResearchReportReadRequestV1,
    MultiLegResearchReportSummaryV1, MultiLegResearchReportV1, MultiLegSignalBucket,
    MAX_MULTI_LEG_EQUITY_REPORT_ROWS, MAX_MULTI_LEG_EVENT_REPORT_PAGE_ROWS,
    MAX_MULTI_LEG_FILL_REPORT_ROWS, MAX_MULTI_LEG_REPORT_ROWS,
    MULTI_LEG_EVENT_REPORT_SCHEMA_VERSION_V1, MULTI_LEG_FUNDING_INTERVAL_MS,
    MULTI_LEG_RESEARCH_REPORT_SCHEMA_VERSION_V1,
};
pub use multi_market_depth_research_report::{
    read_multi_market_depth_research_report_v1,
    read_multi_market_depth_research_report_v1_with_readers, MultiMarketDepthResearchEventPageV1,
    MultiMarketDepthResearchMarketV1, MultiMarketDepthResearchReportReadRequestV1,
    MultiMarketDepthResearchReportV1, MAX_MULTI_MARKET_DEPTH_REPORT_EQUITY_ROWS,
    MAX_MULTI_MARKET_DEPTH_REPORT_FILL_ROWS, MAX_MULTI_MARKET_DEPTH_REPORT_LEVELS_PER_SIDE,
    MAX_MULTI_MARKET_DEPTH_REPORT_MARKETS, MAX_MULTI_MARKET_DEPTH_REPORT_SNAPSHOT_ROWS,
    MULTI_MARKET_DEPTH_RESEARCH_REPORT_SCHEMA_VERSION_V1,
};
pub use multi_market_report::{
    multi_market_depth_input_set_identity_v1, multi_market_depth_spec_fingerprint_v1,
    multi_market_input_set_identity_v1, multi_market_spec_fingerprint_v1,
    validate_multi_market_quality_manifest_set_v1, verify_multi_market_bar_input_v1,
    verify_multi_market_bar_sidecars_from_event_report_v1,
    verify_multi_market_bar_sidecars_from_event_report_with_exact_v1,
    verify_multi_market_bar_sidecars_v1, verify_multi_market_bar_sidecars_with_exact_v1,
    verify_multi_market_depth_input_v1, verify_multi_market_event_fill_sidecar_v1,
    verify_multi_market_legacy_event_fill_sidecar_v1, verify_multi_market_pnl_contributions_v1,
    verify_multi_market_pnl_contributions_with_exact_v1, MultiMarketDepthInputIdentityV1,
    MultiMarketInputIdentityV1, MultiMarketInputSetIdentityV1, VerifiedMultiMarketBarInputV1,
    VerifiedMultiMarketBarSidecarsV1, VerifiedMultiMarketDepthInputV1,
    VerifiedMultiMarketPnlContributions, VerifiedPnlContributionSample,
    MAX_MULTI_MARKET_PNL_SAMPLES,
};
pub use multi_market_research_report::{
    read_multi_market_research_report_v1, read_multi_market_research_report_v1_with_readers,
    MultiMarketResearchEventPageV1, MultiMarketResearchMarketV1,
    MultiMarketResearchReportReadRequestV1, MultiMarketResearchReportV1,
    MAX_MULTI_MARKET_REPORT_BAR_ROWS, MAX_MULTI_MARKET_REPORT_EQUITY_ROWS,
    MAX_MULTI_MARKET_REPORT_FILL_ROWS, MAX_MULTI_MARKET_REPORT_SCHEDULE_PREVIEW_ROWS,
    MULTI_MARKET_RESEARCH_REPORT_SCHEMA_VERSION_V1,
};
pub use qx_core::MakerTakerFeeSchedule;
pub use report_read::{
    multi_market_bar_context, read_depth_run_report_v1, read_depth_run_report_v1_with_reader,
    read_event_report_v1_with_reader, read_multi_market_depth_event_report_page_v1,
    read_multi_market_depth_event_report_page_v1_with_reader,
    read_multi_market_depth_event_report_read_model_v1_with_reader,
    read_multi_market_event_report_page_v1, read_multi_market_event_report_page_v1_with_reader,
    read_multi_market_event_report_read_model_v1_with_reader,
    read_multi_market_event_report_v1_with_reader, read_multi_market_legacy_event_report_v1,
    read_provider_backtest_report_v1, read_provider_backtest_report_v1_with_reader,
    read_provider_depth_backtest_report_v1, read_provider_depth_backtest_report_v1_with_readers,
    read_run_report_v1, read_run_report_v1_with_reader, EventFillCountV1, EventFillIdentityV1,
    EventReportPageV1, MultiMarketDepthEventReportReadModelV1,
    MultiMarketDepthEventReportReadRequestV1, MultiMarketEventReportPageRequestV1,
    MultiMarketEventReportPageV1, MultiMarketEventReportReadModelV1,
    MultiMarketEventReportReadRequestV1, ProviderBacktestReportV1, ProviderDepthBacktestReportV1,
    ResearchReportRowsV1, ResearchRunReportV1, VerifiedMultiMarketEventReportV1,
    EVENT_REPORT_PAGE_SCHEMA_VERSION, MAX_EVENT_REPORT_PAGE_ROWS,
    MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1, PROVIDER_BACKTEST_REPORT_SCHEMA_VERSION_V1,
    PROVIDER_DEPTH_BACKTEST_REPORT_SCHEMA_VERSION_V1, RESEARCH_RUN_REPORT_SCHEMA_VERSION,
};
pub use run_handle::{
    RunEvent, RunEventKind, RunHandle, RunObservation, RunStatus, DEFAULT_WAIT_TIMEOUT,
};
pub use spec::market_spec::{
    backtest_margin_model_from_market, backtest_market_spec_binding, market_spec_from_value,
    market_spec_source_label, BacktestMarketSpecBinding, CCXT_MARKET_SPEC_VERSION,
    DEFAULT_INSTRUMENT_SPEC_VERSION, PRODUCT_MARKET_SPEC_VERSION,
};
pub use spec::multi_market::{
    MultiMarketBacktestMarket, MultiMarketBacktestOutcome, MultiMarketBacktestSpec,
    MAX_MULTI_MARKETS, MAX_STRATEGIES_PER_MARKET, MIN_MULTI_MARKETS,
    MULTI_MARKET_BACKTEST_EXACT_SCHEMA_VERSION, MULTI_MARKET_BACKTEST_SCHEMA_VERSION,
};
pub use spec::{
    AlgoExecutionMethod, AlgoExecutionPlan, AlgoExecutionSpec, BacktestArtifacts,
    BacktestDatasetInput, BacktestFillModel, BacktestMarginConfig, BacktestMarginTier,
    BacktestOutcome, BacktestRiskRules, BacktestSpec, BuiltinStrategySpec,
    CapitalAllocationAccount, CapitalAllocationRequest, CapitalAllocationResult,
    CapitalAllocationSpec, DatasetManifestValidationSpec, DatasetSpec, DatasetVerdict,
    DepthBacktestOutcome, DepthBacktestSpec, DepthDatasetManifestValidationSpec,
    JointAccountBacktestResult, JointBacktestAccount, MultiAccountBacktestResult,
    MultiAccountBacktestSpec, MultiAccountBacktestSummary, MultiAccountDepthBacktestResult,
    MultiAccountDepthBacktestSpec, MultiAccountDepthBacktestSummary, MultiAccountDepthRunResult,
    MultiAccountJointBacktestOutcome, MultiAccountJointBacktestSpec, MultiAccountRunResult,
    MultiBuiltinAttribution, MultiBuiltinBacktestRun, MultiBuiltinBacktestSpec, MultiBuiltinLegRun,
    MultiLegBacktestOutcome, MultiLegBacktestSpec, MultiLegDatasetInput, MultiLegFillModel,
    MultiMarketDepthBacktestMarket, MultiMarketDepthBacktestOutcome, MultiMarketDepthBacktestSpec,
    PortfolioBacktestConfig, PortfolioStrategySpec, ProtectiveExitSpec, RiskParityAssetSpec,
    RiskParityResult, RiskParitySpec, VerificationResult, ALGO_EXECUTION_PLAN_SCHEMA_VERSION,
    ALGO_EXECUTION_SPEC_SCHEMA_VERSION, BACKTEST_SPEC_EXACT_SCHEMA_VERSION,
    BACKTEST_SPEC_SCHEMA_VERSION, CAPITAL_ALLOCATION_RESULT_SCHEMA_VERSION,
    DATASET_MANIFEST_VALIDATION_SCHEMA_VERSION, DATASET_SPEC_SCHEMA_VERSION,
    DEPTH_BACKTEST_SPEC_SCHEMA_VERSION, DEPTH_DATASET_MANIFEST_VALIDATION_SCHEMA_VERSION,
    MAX_JOINT_ACCOUNTS, MAX_JOINT_MARKETS, MAX_MULTI_ACCOUNT_RUNS, MAX_MULTI_MARKET_DEPTH_MARKETS,
    MIN_BACKTEST_BARS, MIN_MULTI_MARKET_DEPTH_MARKETS,
    MULTI_ACCOUNT_BACKTEST_RESULT_SCHEMA_VERSION, MULTI_ACCOUNT_BACKTEST_SCHEMA_VERSION,
    MULTI_ACCOUNT_DEPTH_BACKTEST_RESULT_SCHEMA_VERSION,
    MULTI_ACCOUNT_JOINT_BACKTEST_SCHEMA_VERSION, MULTI_BUILTIN_BACKTEST_SCHEMA_VERSION,
    MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION, MULTI_LEG_BACKTEST_SPEC_SCHEMA_VERSION,
    MULTI_MARKET_DEPTH_BACKTEST_EXACT_SCHEMA_VERSION, MULTI_MARKET_DEPTH_BACKTEST_SCHEMA_VERSION,
    RISK_PARITY_RESULT_SCHEMA_VERSION, RISK_PARITY_SPEC_SCHEMA_VERSION,
};
pub use strategy_backtest_identity::{
    build_backtest_run_manifest, build_strategy_backtest_run_manifest, strategy_backtest_run_id,
    BacktestRunManifestInput, StrategyBacktestRunIdentityInput, StrategyBacktestRunManifestInput,
};
pub use strategy_backtest_workflow::{
    run_and_persist_strategy_backtest, StrategyBacktestExecution, StrategyBacktestWorkflowInput,
    StrategyBacktestWorkflowResult,
};
pub use strategy_catalog::{builtin_strategy_catalog, BuiltinStrategyInfo};
pub use strategy_engine::{
    assemble_strategy_backtest_spec, build_strategy_context, initialize_native_builtin_strategy,
    initialize_native_builtin_strategy_for_backtest, run_bar_backtest_with_outcome,
    run_bar_backtest_with_strategy, run_native_builtin_strategy_backtest,
    run_native_builtin_strategy_backtest_with_outcome, NativeStrategyBacktestContextBindings,
    StrategyBacktestBindings, StrategyBacktestOutcome, StrategyContextBindings,
};
pub use strategy_order::{
    build_strategy_order_from_intent, build_strategy_target_order,
    normalize_strategy_order_quantity, StrategyOrderBindings, StrategyOrderBuild,
    StrategyOrderInput, StrategyTargetOrderInput,
};
pub use strategy_preflight::{
    validate_strategy_execution_preflight_v1, StrategyExecutionPreflightResultV1,
    StrategyExecutionPreflightSpecV1, StrategyPreflightMarketV1, StrategyPreflightPurpose,
    STRATEGY_EXECUTION_PREFLIGHT_SCHEMA_VERSION,
};
pub use trade_facts::{rejection_count, rejection_facts, rejection_facts_line, spread_group_id};

/// Run an L2 multi-market portfolio with caller-authored strategies in selected slots.
pub fn run_multi_market_depth_backtest_with_strategy_slots(
    spec: &MultiMarketDepthBacktestSpec,
    strategies: Vec<(StrategyArtifactIdentity, Box<dyn qx_strategy::Strategy>)>,
    context: &RunContext,
) -> Result<MultiMarketDepthBacktestOutcome, AppError> {
    cases::run_multi_market_depth_backtest_with_strategy_slots(spec, strategies, context)
}
