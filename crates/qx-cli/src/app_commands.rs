//! `qx-cli app` 的派发与处理器（T2-2；退出门 G1「三入口同一 use case」/ G2「CLI 真调应用接口」）。
//!
//! 这个模块**没有业务**。它做的三件事是：读一份 spec JSON、把它连同调用上下文交给 `qx-app`
//! 的用例、把结果 JSON 打到 stdout。业务全在 `qx-app` 里——这正是 G2 要的形状：CLI 不再自己
//! 装配引擎；网格实验也只展开参数并复用 `run_backtest` 与 `compare_runs`。
//!
//! ## 两个入口面为什么逐字节可比
//!
//! 成功时 stdout **只有** `to_json()` 那一份文档；失败时 stderr 一行 `[qx-cli · CLI] {AppError JSON}`
//! 并以 2 退出（stdout 保持干净，只装成功结果）。Python SDK 与 HTTP API 交出/换回的是同一份
//! **文档字节**，各自的外框（异常载荷 / 响应体）不算文档的一部分——所以「同一 use case 三个入口
//! 结果哈希相同、错误 code/context 相同」是一条可以直接断言的事实，而不是一句承诺。
//!
//! `RunContext::with_code_commit` 在这里第一次被**生产**代码调用：CLI 把自己的构建身份
//! （`build_identity::BUILD_REVISION`，来自 build.rs 注入的 `QX_GIT_COMMIT`）带进用例，
//! 于是产物里的 `code_commit` 是"跑这次回测的那份二进制"，不是一个包版本号。

use crate::app_args::AppCommand;
use crate::build_identity::BUILD_REVISION;
use qx_app::{
    compare_runs, plan_algo_execution, plan_risk_parity, read_depth_run_report_v1,
    read_multi_account_joint_report_v1_with_options, read_multi_account_report_v1,
    read_multi_leg_event_report_page_v1, read_multi_leg_report_facts_v1,
    read_multi_leg_research_report_v1, read_multi_market_depth_event_report_page_v1,
    read_multi_market_depth_research_report_v1, read_multi_market_event_report_page_v1,
    read_multi_market_research_report_v1, read_provider_backtest_report_v1,
    read_provider_depth_backtest_report_v1, read_run_report_v1, run_backtest, run_depth_backtest,
    run_depth_experiment, run_experiment, run_multi_account_backtest,
    run_multi_account_depth_backtest, run_multi_leg_backtest, run_multi_leg_experiment,
    run_multi_market_backtest, run_multi_market_depth_backtest, run_multi_market_experiment,
    run_provider_backtest_with_manifest, run_provider_depth_backtest, validate_dataset,
    verify_depth_run, verify_run, AlgoExecutionSpec, AppError, AppErrorCategory, BacktestOutcome,
    BacktestSpec, CallerCapability, CompareRunsSpec, DatasetSpec, DepthBacktestOutcome,
    DepthBacktestSpec, MultiAccountBacktestSpec, MultiAccountDepthBacktestSpec,
    MultiAccountJointBacktestOutcome, MultiAccountJointBacktestSpec,
    MultiAccountJointReportReadOptionsV1, MultiLegBacktestOutcome, MultiLegBacktestSpec,
    MultiMarketBacktestSpec, MultiMarketDepthBacktestSpec, ProviderBacktestOutcome,
    ProviderBacktestSpec, ProviderDepthBacktestOutcome, ProviderDepthBacktestSpec, RiskParitySpec,
    RunContext, RunDepthExperimentSpec, RunExperimentSpec, RunMultiLegExperimentSpec,
    RunMultiMarketExperimentSpec,
};
use qx_runtime::RuntimeConfig;
use std::path::Path;

mod report_readers;
mod use_cases;

pub(crate) fn dispatch(action: Option<AppCommand>) {
    let Some(action) = action else {
        eprintln!(
            "[qx-cli · CLI] app 需要一个子命令：app <validate-dataset|backtest|provider-backtest|provider-depth-backtest|verify|read-report|read-depth-report|read-provider-report|read-provider-depth-report|read-multi-account-report|read-multi-account-joint-report|read-multi-leg-report|read-multi-leg-research-report|read-multi-leg-event-page|read-multi-market-event-page|read-multi-market-depth-event-page|read-multi-market-depth-research-report|compare-runs|plan-risk-parity|plan-algo-execution|run-experiment|run-depth-experiment|run-multi-leg-experiment|run-multi-market-experiment|depth-backtest|verify-depth|multi-leg-backtest|multi-market-backtest|multi-account-backtest|multi-account-depth-backtest> <spec.json>\n  \
             validate-dataset <DatasetSpec.json>  校验一份数据集能否支撑 Bar 回测\n  \
             backtest <BacktestSpec.json>         跑一次 Bar 回测并落四份产物\n  \
             provider-backtest <RuntimeConfig.json> <ProviderBacktestSpec.json>  Run a shared Rust Provider backtest\n  \
             provider-depth-backtest <RuntimeConfig.json> <ProviderDepthBacktestSpec.json>  Run a shared Rust Provider depth backtest\n  \
             verify <BacktestOutcome.json>        复核一轮产物（输入取 `app backtest` 的 stdout）\n  \
             read-report <BacktestOutcome.json>   读取共享的 Bar 版本化报告 DTO\n  \
             read-depth-report <DepthBacktestOutcome.json>  读取共享的 Tick/OrderBook 版本化报告 DTO\n  \
             read-multi-account-report <MultiAccountBacktestResult.json>  读取共享的 Bar/Depth 多账户报告 DTO\n  \
             read-multi-account-joint-report <MultiAccountJointBacktestOutcome.json>  读取联合账户报告 DTO\n  \
             read-multi-leg-research-report <request.json>  读取完整、有界的多腿研究报告\n  \\
             read-multi-leg-event-page <request.json>  读取并复核多腿 Bar EventLog 页面\n  \
             read-multi-market-event-page <request.json>  读取并复核多市场 Bar EventLog 页面\n  \
             read-multi-market-depth-event-page <request.json>  读取并复核多市场 Depth EventLog 页面\n  \
             read-multi-market-research-report <request.json>  读取完整有界多市场 Bar 研究报告\n  \
             read-multi-market-depth-research-report <request.json>  读取完整有界多市场 L1/L2 研究报告\n  \
             compare-runs <CompareRunsSpec.json>  确定性比较多组回测结果\n  \
             plan-risk-parity <RiskParitySpec.json>  生成风险平价组合调仓计划\n  \
             plan-algo-execution <AlgoExecutionSpec.json>  生成 TWAP/VWAP 子单计划\n  \
             run-experiment <RunExperimentSpec.json>  执行参数网格并比较候选\n  \
             run-depth-experiment <RunDepthExperimentSpec.json> 执行 Tick/OrderBook 参数网格\n  \
             run-multi-leg-experiment <RunMultiLegExperimentSpec.json> 执行多腿参数网格\n  \
             run-multi-market-experiment <RunMultiMarketExperimentSpec.json> 执行多市场策略/参数网格\n  \
             depth-backtest <DepthBacktestSpec.json>  执行 L1 Tick 或 L2 订单簿回测\n  \
             verify-depth <DepthBacktestOutcome.json> 复核 L1/L2 产物\n  \
             multi-leg-backtest <MultiLegBacktestSpec.json> 执行双腿 Bar 套利回测\n  \
             multi-market-backtest <MultiMarketBacktestSpec.json> 执行同账户多市场 Bar 回测\n  \
             multi-market-depth-backtest <MultiMarketDepthBacktestSpec.json> 执行同账户多市场深度回测\n  \\
             multi-account-backtest <MultiAccountBacktestSpec.json> 执行独立账户 Bar 回测\n  \
             multi-account-depth-backtest <MultiAccountDepthBacktestSpec.json> 执行独立账户 Tick/OrderBook 回测"
        );
        std::process::exit(2);
    };
    let result = match &action {
        AppCommand::ValidateDataset { spec } => use_cases::validate(spec),
        AppCommand::Backtest { spec } => use_cases::backtest(spec),
        AppCommand::ProviderBacktest { runtime, spec } => {
            use_cases::provider_backtest(runtime, spec)
        }
        AppCommand::ProviderDepthBacktest { runtime, spec } => {
            use_cases::provider_depth_backtest(runtime, spec)
        }
        AppCommand::Verify { outcome } => use_cases::verify(outcome),
        AppCommand::ReadReport { outcome } => report_readers::read_bar_report(outcome),
        AppCommand::ReadDepthReport { outcome } => report_readers::read_depth_report(outcome),
        AppCommand::ReadProviderReport { outcome } => {
            report_readers::read_provider_bar_report(outcome)
        }
        AppCommand::ReadProviderDepthReport { outcome } => {
            report_readers::read_provider_depth_report(outcome)
        }
        AppCommand::ReadMultiAccountReport { outcome } => {
            report_readers::read_multi_account_report(outcome)
        }
        AppCommand::ReadMultiAccountJointReport { outcome } => {
            report_readers::read_multi_account_joint_report(outcome)
        }
        AppCommand::ReadMultiLegReport { outcome } => {
            report_readers::read_multi_leg_report(outcome)
        }
        AppCommand::ReadMultiLegResearchReport { request } => {
            report_readers::read_multi_leg_research_report(request)
        }
        AppCommand::ReadMultiLegEventPage { request } => {
            report_readers::read_multi_leg_event_page(request)
        }
        AppCommand::ReadMultiMarketEventPage { request } => {
            report_readers::read_multi_market_event_page(request)
        }
        AppCommand::ReadMultiMarketDepthEventPage { request } => {
            report_readers::read_multi_market_depth_event_page(request)
        }
        AppCommand::ReadMultiMarketDepthResearchReport { request } => {
            report_readers::read_multi_market_depth_research_report(request)
        }
        AppCommand::ReadMultiMarketResearchReport { request } => {
            report_readers::read_multi_market_research_report(request)
        }
        AppCommand::CompareRuns { spec } => use_cases::compare(spec),
        AppCommand::PlanRiskParity { spec } => use_cases::risk_parity(spec),
        AppCommand::PlanAlgoExecution { spec } => use_cases::algo_execution(spec),
        AppCommand::RunExperiment { spec } => use_cases::experiment(spec),
        AppCommand::RunDepthExperiment { spec } => use_cases::depth_experiment(spec),
        AppCommand::RunMultiLegExperiment { spec } => use_cases::multi_leg_experiment(spec),
        AppCommand::RunMultiMarketExperiment { spec } => use_cases::multi_market_experiment(spec),
        AppCommand::MultiMarketBacktest { spec } => use_cases::multi_market_backtest(spec),
        AppCommand::MultiMarketDepthBacktest { spec } => {
            use_cases::multi_market_depth_backtest(spec)
        }
        AppCommand::DepthBacktest { spec } => use_cases::depth_backtest(spec),
        AppCommand::VerifyDepth { outcome } => use_cases::verify_depth(outcome),
        AppCommand::MultiLegBacktest { spec } => use_cases::multi_leg_backtest(spec),
        AppCommand::MultiAccountBacktest { spec } => use_cases::multi_account_backtest(spec),
        AppCommand::MultiAccountDepthBacktest { spec } => {
            use_cases::multi_account_depth_backtest(spec)
        }
        AppCommand::MultiAccountJointBacktest { spec } => {
            use_cases::multi_account_joint_backtest(spec)
        }
    };
    match result {
        Ok(payload) => println!("{payload}"),
        Err(error) => {
            // `[组件 · 子域] {AppError JSON}`：前缀是传输层的框，**文档本身**才是三个入口比对的对象
            // （G1 的「错误 code/context 相同」）。失败一律退 2，stdout 保持干净——它只装成功结果。
            eprintln!("[qx-cli · CLI] {}", error.to_json());
            std::process::exit(2);
        }
    }
}

/// 读一份 JSON 文档。文件不在是 `DataUnavailable`（去把数据准备好），不是"输入写错了"。
fn read_document(path: &Path) -> Result<String, AppError> {
    std::fs::read_to_string(path)
        .map_err(|error| AppError::from_io(&format!("读取 {}", path.display()), &error))
}

/// 三个入口的调用上下文完全同形：R 档 + 身份串当 correlation id + 本二进制的构建身份。
fn context(correlation_id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, correlation_id).with_code_commit(BUILD_REVISION)
}
