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
    compare_runs, plan_algo_execution, plan_risk_parity, run_backtest, run_depth_backtest,
    run_depth_experiment, run_experiment, run_multi_account_backtest, run_multi_leg_backtest,
    run_multi_leg_experiment, validate_dataset, verify_depth_run, verify_run, AlgoExecutionSpec,
    AppError, BacktestOutcome, BacktestSpec, CallerCapability, CompareRunsSpec, DatasetSpec,
    DepthBacktestOutcome, DepthBacktestSpec, MultiAccountBacktestSpec, MultiLegBacktestSpec,
    RiskParitySpec, RunContext, RunDepthExperimentSpec, RunExperimentSpec,
    RunMultiLegExperimentSpec,
};
use std::path::Path;

pub(crate) fn dispatch(action: Option<AppCommand>) {
    let Some(action) = action else {
        eprintln!(
            "[qx-cli · CLI] app 需要一个子命令：app <validate-dataset|backtest|verify|compare-runs|plan-risk-parity|plan-algo-execution|run-experiment|run-depth-experiment|run-multi-leg-experiment|depth-backtest|verify-depth|multi-leg-backtest|multi-account-backtest> <spec.json>\n  \
             validate-dataset <DatasetSpec.json>  校验一份数据集能否支撑 Bar 回测\n  \
             backtest <BacktestSpec.json>         跑一次 Bar 回测并落四份产物\n  \
             verify <BacktestOutcome.json>        复核一轮产物（输入取 `app backtest` 的 stdout）\n  \
             compare-runs <CompareRunsSpec.json>  确定性比较多组回测结果\n  \
             plan-risk-parity <RiskParitySpec.json>  生成风险平价组合调仓计划\n  \
             plan-algo-execution <AlgoExecutionSpec.json>  生成 TWAP/VWAP 子单计划\n  \
             run-experiment <RunExperimentSpec.json>  执行参数网格并比较候选\n  \
             run-depth-experiment <RunDepthExperimentSpec.json> 执行 Tick/OrderBook 参数网格\n  \
             run-multi-leg-experiment <RunMultiLegExperimentSpec.json> 执行多腿参数网格\n  \
             depth-backtest <DepthBacktestSpec.json>  执行 L1 Tick 或 L2 订单簿回测\n  \
             verify-depth <DepthBacktestOutcome.json> 复核 L1/L2 产物\n  \
             multi-leg-backtest <MultiLegBacktestSpec.json> 执行双腿 Bar 套利回测\n  \
             multi-account-backtest <MultiAccountBacktestSpec.json> 执行独立账户 Bar 回测"
        );
        std::process::exit(2);
    };
    let result = match &action {
        AppCommand::ValidateDataset { spec } => validate(spec),
        AppCommand::Backtest { spec } => backtest(spec),
        AppCommand::Verify { outcome } => verify(outcome),
        AppCommand::CompareRuns { spec } => compare(spec),
        AppCommand::PlanRiskParity { spec } => risk_parity(spec),
        AppCommand::PlanAlgoExecution { spec } => algo_execution(spec),
        AppCommand::RunExperiment { spec } => experiment(spec),
        AppCommand::RunDepthExperiment { spec } => depth_experiment(spec),
        AppCommand::RunMultiLegExperiment { spec } => multi_leg_experiment(spec),
        AppCommand::DepthBacktest { spec } => depth_backtest(spec),
        AppCommand::VerifyDepth { outcome } => verify_depth(outcome),
        AppCommand::MultiLegBacktest { spec } => multi_leg_backtest(spec),
        AppCommand::MultiAccountBacktest { spec } => multi_account_backtest(spec),
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

fn verify_depth(outcome_path: &Path) -> Result<String, AppError> {
    let outcome = DepthBacktestOutcome::from_json(&read_document(outcome_path)?)?;
    verify_depth_run(&outcome, &context(&outcome.run_id))?.to_json()
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

fn verify(outcome_path: &Path) -> Result<String, AppError> {
    let outcome = BacktestOutcome::from_json(&read_document(outcome_path)?)?;
    verify_run(&outcome, &context(&outcome.run_id))?.to_json()
}
