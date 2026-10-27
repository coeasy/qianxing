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
        AppCommand::ValidateDataset { spec } => validate(spec),
        AppCommand::Backtest { spec } => backtest(spec),
        AppCommand::ProviderBacktest { runtime, spec } => provider_backtest(runtime, spec),
        AppCommand::ProviderDepthBacktest { runtime, spec } => {
            provider_depth_backtest(runtime, spec)
        }
        AppCommand::Verify { outcome } => verify(outcome),
        AppCommand::ReadReport { outcome } => read_bar_report(outcome),
        AppCommand::ReadDepthReport { outcome } => read_depth_report(outcome),
        AppCommand::ReadProviderReport { outcome } => read_provider_bar_report(outcome),
        AppCommand::ReadProviderDepthReport { outcome } => read_provider_depth_report(outcome),
        AppCommand::ReadMultiAccountReport { outcome } => read_multi_account_report(outcome),
        AppCommand::ReadMultiAccountJointReport { outcome } => {
            read_multi_account_joint_report(outcome)
        }
        AppCommand::ReadMultiLegReport { outcome } => read_multi_leg_report(outcome),
        AppCommand::ReadMultiLegResearchReport { request } => {
            read_multi_leg_research_report(request)
        }
        AppCommand::ReadMultiLegEventPage { request } => read_multi_leg_event_page(request),
        AppCommand::ReadMultiMarketEventPage { request } => read_multi_market_event_page(request),
        AppCommand::ReadMultiMarketDepthEventPage { request } => {
            read_multi_market_depth_event_page(request)
        }
        AppCommand::ReadMultiMarketDepthResearchReport { request } => {
            read_multi_market_depth_research_report(request)
        }
        AppCommand::ReadMultiMarketResearchReport { request } => {
            read_multi_market_research_report(request)
        }
        AppCommand::CompareRuns { spec } => compare(spec),
        AppCommand::PlanRiskParity { spec } => risk_parity(spec),
        AppCommand::PlanAlgoExecution { spec } => algo_execution(spec),
        AppCommand::RunExperiment { spec } => experiment(spec),
        AppCommand::RunDepthExperiment { spec } => depth_experiment(spec),
        AppCommand::RunMultiLegExperiment { spec } => multi_leg_experiment(spec),
        AppCommand::RunMultiMarketExperiment { spec } => multi_market_experiment(spec),
        AppCommand::MultiMarketBacktest { spec } => multi_market_backtest(spec),
        AppCommand::MultiMarketDepthBacktest { spec } => multi_market_depth_backtest(spec),
        AppCommand::DepthBacktest { spec } => depth_backtest(spec),
        AppCommand::VerifyDepth { outcome } => verify_depth(outcome),
        AppCommand::MultiLegBacktest { spec } => multi_leg_backtest(spec),
        AppCommand::MultiAccountBacktest { spec } => multi_account_backtest(spec),
        AppCommand::MultiAccountDepthBacktest { spec } => multi_account_depth_backtest(spec),
        AppCommand::MultiAccountJointBacktest { spec } => multi_account_joint_backtest(spec),
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

fn compare(spec_path: &Path) -> Result<String, AppError> {
    let spec = CompareRunsSpec::from_json(&read_document(spec_path)?)?;
    compare_runs(&spec, &context("compare-runs"))?.to_json()
}

fn risk_parity(spec_path: &Path) -> Result<String, AppError> {
    let spec = RiskParitySpec::from_json(&read_document(spec_path)?)?;
    plan_risk_parity(&spec, &context(&spec.portfolio_id))?.to_json()
}

fn algo_execution(spec_path: &Path) -> Result<String, AppError> {
    let spec = AlgoExecutionSpec::from_json(&read_document(spec_path)?)?;
    plan_algo_execution(&spec, &context(&spec.parent_id))?.to_json()
}

fn experiment(spec_path: &Path) -> Result<String, AppError> {
    let spec = RunExperimentSpec::from_json(&read_document(spec_path)?)?;
    run_experiment(&spec, &context(&spec.experiment_id))?.to_json()
}

fn multi_leg_experiment(spec_path: &Path) -> Result<String, AppError> {
    let spec = RunMultiLegExperimentSpec::from_json(&read_document(spec_path)?)?;
    run_multi_leg_experiment(&spec, &context(&spec.experiment_id))?.to_json()
}

fn multi_market_experiment(spec_path: &Path) -> Result<String, AppError> {
    let spec = RunMultiMarketExperimentSpec::from_json(&read_document(spec_path)?)?;
    run_multi_market_experiment(&spec, &context(&spec.experiment_id))?.to_json()
}

fn depth_experiment(spec_path: &Path) -> Result<String, AppError> {
    let spec = RunDepthExperimentSpec::from_json(&read_document(spec_path)?)?;
    run_depth_experiment(&spec, &context(&spec.experiment_id))?.to_json()
}

fn depth_backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = DepthBacktestSpec::from_json(&read_document(spec_path)?)?;
    run_depth_backtest(&spec, &context(&spec.run_id))?.to_json()
}

fn multi_leg_backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = MultiLegBacktestSpec::from_json(&read_document(spec_path)?)?;
    run_multi_leg_backtest(&spec, &context(&spec.run_id))?.to_json()
}

fn multi_account_backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = MultiAccountBacktestSpec::from_json(&read_document(spec_path)?)?;
    run_multi_account_backtest(&spec, &context(&spec.batch_id))?.to_json()
}

fn multi_account_depth_backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = MultiAccountDepthBacktestSpec::from_json(&read_document(spec_path)?)?;
    run_multi_account_depth_backtest(&spec, &context(&spec.batch_id))?.to_json()
}

fn multi_account_joint_backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = MultiAccountJointBacktestSpec::from_json(&read_document(spec_path)?)?;
    qx_app::run_multi_account_joint_backtest(&spec, &context(&spec.run_id))?.to_json()
}

fn multi_market_backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = MultiMarketBacktestSpec::from_json(&read_document(spec_path)?)?;
    run_multi_market_backtest(&spec, &context(&spec.run_id))?.to_json()
}

fn multi_market_depth_backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = MultiMarketDepthBacktestSpec::from_json(&read_document(spec_path)?)?;
    run_multi_market_depth_backtest(&spec, &context(&spec.run_id))?.to_json()
}

fn verify_depth(outcome_path: &Path) -> Result<String, AppError> {
    let outcome = DepthBacktestOutcome::from_json(&read_document(outcome_path)?)?;
    verify_depth_run(&outcome, &context(&outcome.run_id))?.to_json()
}

fn read_bar_report(outcome_path: &Path) -> Result<String, AppError> {
    let outcome = BacktestOutcome::from_json(&read_document(outcome_path)?)?;
    let report = read_run_report_v1(&outcome, &context(&outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("Bar 研究报告 DTO 序列化失败: {error}"),
        )
    })
}

fn read_depth_report(outcome_path: &Path) -> Result<String, AppError> {
    let outcome = DepthBacktestOutcome::from_json(&read_document(outcome_path)?)?;
    let report = read_depth_run_report_v1(&outcome, &context(&outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("深度研究报告 DTO 序列化失败: {error}"),
        )
    })
}

fn read_provider_bar_report(outcome_path: &Path) -> Result<String, AppError> {
    let outcome: ProviderBacktestOutcome = serde_json::from_str(&read_document(outcome_path)?)
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("Provider Bar 回测结果 JSON 无效: {error}"),
            )
        })?;
    let report = read_provider_backtest_report_v1(&outcome, &context(&outcome.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("Provider Bar 研究报告 DTO 序列化失败: {error}"),
        )
    })
}

fn read_provider_depth_report(outcome_path: &Path) -> Result<String, AppError> {
    let outcome: ProviderDepthBacktestOutcome = serde_json::from_str(&read_document(outcome_path)?)
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("Provider 深度回测结果 JSON 无效: {error}"),
            )
        })?;
    let report =
        read_provider_depth_backtest_report_v1(&outcome, &context(&outcome.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("Provider 深度研究报告 DTO 序列化失败: {error}"),
        )
    })
}

fn read_multi_account_report(outcome_path: &Path) -> Result<String, AppError> {
    let payload = read_document(outcome_path)?;
    let outcome: serde_json::Value = serde_json::from_str(&payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("多账户结果 JSON 无效: {error}"),
        )
    })?;
    let batch_id = outcome
        .get("batch_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("multi-account-report");
    let report = read_multi_account_report_v1(&outcome, &context(batch_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多账户研究报告 DTO 序列化失败: {error}"),
        )
    })
}

fn read_multi_account_joint_report(outcome_path: &Path) -> Result<String, AppError> {
    let input: serde_json::Value =
        serde_json::from_str(&read_document(outcome_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("联合账户报告请求 JSON 无效: {error}"),
            )
        })?;
    let (outcome_value, options_value) = if let Some(object) = input.as_object() {
        if let Some(outcome) = object.get("outcome") {
            if object
                .keys()
                .any(|key| !matches!(key.as_str(), "outcome" | "joint_account_event_pages"))
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "联合账户报告请求包含未知字段",
                ));
            }
            (
                outcome.clone(),
                object.get("joint_account_event_pages").cloned(),
            )
        } else {
            (input.clone(), None)
        }
    } else {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "联合账户报告请求必须是 JSON 对象",
        ));
    };
    let outcome: MultiAccountJointBacktestOutcome =
        serde_json::from_value(outcome_value).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("联合多账户结果 JSON 无效: {error}"),
            )
        })?;
    let options = options_value
        .map(|pages| {
            serde_json::from_value::<MultiAccountJointReportReadOptionsV1>(serde_json::json!({
                "joint_account_event_pages": pages
            }))
        })
        .transpose()
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("联合账户事件分页选项无效: {error}"),
            )
        })?
        .unwrap_or_default();
    let report = read_multi_account_joint_report_v1_with_options(
        &outcome,
        &context(&outcome.run_id),
        &options,
    )?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("联合多账户研究报告 DTO 序列化失败: {error}"),
        )
    })
}

fn read_multi_leg_report(outcome_path: &Path) -> Result<String, AppError> {
    let outcome = MultiLegBacktestOutcome::from_json(&read_document(outcome_path)?)?;
    let report = read_multi_leg_report_facts_v1(&outcome, &context(&outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多腿研究报告 DTO 序列化失败: {error}"),
        )
    })
}

fn read_multi_leg_research_report(request_path: &Path) -> Result<String, AppError> {
    let request: qx_app::MultiLegResearchReportReadRequestV1 =
        serde_json::from_str(&read_document(request_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("多腿完整研究报告请求 JSON 无效: {error}"),
            )
        })?;
    let report = read_multi_leg_research_report_v1(&request, &context(&request.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多腿完整研究报告 DTO 序列化失败: {error}"),
        )
    })
}

fn read_multi_leg_event_page(request_path: &Path) -> Result<String, AppError> {
    let request: qx_app::MultiLegEventReportReadRequestV1 =
        serde_json::from_str(&read_document(request_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("多腿事件报告读取请求 JSON 无效: {error}"),
            )
        })?;
    let report = read_multi_leg_event_report_page_v1(&request, &context(&request.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多腿事件报告 DTO 序列化失败: {error}"),
        )
    })
}

fn read_multi_market_event_page(request_path: &Path) -> Result<String, AppError> {
    let request: qx_app::MultiMarketEventReportReadRequestV1 =
        serde_json::from_str(&read_document(request_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("多市场事件报告读取请求 JSON 无效: {error}"),
            )
        })?;
    let report =
        read_multi_market_event_report_page_v1(&request, &context(&request.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多市场事件报告 DTO 序列化失败: {error}"),
        )
    })
}

fn read_multi_market_depth_event_page(request_path: &Path) -> Result<String, AppError> {
    let request: qx_app::MultiMarketDepthEventReportReadRequestV1 =
        serde_json::from_str(&read_document(request_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("多市场深度事件报告读取请求 JSON 无效: {error}"),
            )
        })?;
    let report =
        read_multi_market_depth_event_report_page_v1(&request, &context(&request.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多市场深度事件报告 DTO 序列化失败: {error}"),
        )
    })
}

fn read_multi_market_research_report(request_path: &Path) -> Result<String, AppError> {
    let request: qx_app::MultiMarketResearchReportReadRequestV1 =
        serde_json::from_str(&read_document(request_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("多市场完整研究报告请求 JSON 无效: {error}"),
            )
        })?;
    let report = read_multi_market_research_report_v1(&request, &context(&request.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多市场完整研究报告 DTO 序列化失败: {error}"),
        )
    })
}

fn read_multi_market_depth_research_report(request_path: &Path) -> Result<String, AppError> {
    let request: qx_app::MultiMarketDepthResearchReportReadRequestV1 =
        serde_json::from_str(&read_document(request_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("多市场深度完整研究报告请求 JSON 无效: {error}"),
            )
        })?;
    let report =
        read_multi_market_depth_research_report_v1(&request, &context(&request.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多市场深度完整研究报告 DTO 序列化失败: {error}"),
        )
    })
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

fn validate(spec_path: &Path) -> Result<String, AppError> {
    let spec = DatasetSpec::from_json(&read_document(spec_path)?)?;
    validate_dataset(&spec, &context(&spec.dataset_id))?.to_json()
}

fn backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = BacktestSpec::from_json(&read_document(spec_path)?)?;
    run_backtest(&spec, &context(&spec.run_id))?.to_json()
}

fn provider_backtest(runtime_path: &Path, spec_path: &Path) -> Result<String, AppError> {
    let runtime = RuntimeConfig::from_json(&read_document(runtime_path)?).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("运行时 Provider 配置无效: {error}"),
        )
    })?;
    let spec: ProviderBacktestSpec =
        serde_json::from_str(&read_document(spec_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("ProviderBacktestSpec JSON 无效: {error}"),
            )
        })?;
    spec.validate()?;
    let registry = qx_service::provider_registry::from_runtime_config(
        &runtime,
        runtime_path,
        &crate::python_interpreter(),
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("装配 ProviderRegistry 失败: {error}"),
        )
    })?
    .ok_or_else(|| {
        AppError::new(
            AppErrorCategory::DataUnavailable,
            "运行时配置没有 api.data_providers，无法执行 Provider 回测",
        )
    })?;
    let outcome =
        run_provider_backtest_with_manifest(&registry, &spec, &context(&spec.backtest.run_id))?;
    serde_json::to_string(&outcome).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("ProviderBacktestOutcome 序列化失败: {error}"),
        )
    })
}

fn provider_depth_backtest(runtime_path: &Path, spec_path: &Path) -> Result<String, AppError> {
    let runtime = RuntimeConfig::from_json(&read_document(runtime_path)?).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("运行时 Provider 配置无效: {error}"),
        )
    })?;
    let spec: ProviderDepthBacktestSpec = serde_json::from_str(&read_document(spec_path)?)
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("ProviderDepthBacktestSpec JSON 无效: {error}"),
            )
        })?;
    spec.validate()?;
    let registry = qx_service::provider_registry::from_runtime_config(
        &runtime,
        runtime_path,
        &crate::python_interpreter(),
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("装配 ProviderRegistry 失败: {error}"),
        )
    })?
    .ok_or_else(|| {
        AppError::new(
            AppErrorCategory::DataUnavailable,
            "运行时配置没有 api.data_providers，无法执行 Provider 深度回测",
        )
    })?;
    let outcome = run_provider_depth_backtest(&registry, &spec, &context(&spec.backtest.run_id))?;
    serde_json::to_string(&outcome).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("ProviderDepthBacktestOutcome 序列化失败: {error}"),
        )
    })
}

fn verify(outcome_path: &Path) -> Result<String, AppError> {
    let outcome = BacktestOutcome::from_json(&read_document(outcome_path)?)?;
    verify_run(&outcome, &context(&outcome.run_id))?.to_json()
}
