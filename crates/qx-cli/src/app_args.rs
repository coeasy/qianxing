//! `qx-cli app` 的参数表：每个应用层入口各一条命令（T2-2 / 退出门 G1）。
//!
//! 与 `data_validate_args.rs` / `plan_args.rs` / `console_args.rs` 同因：`cli_args.rs` 处在行数
//! 棘轮顶格，新增命令的参数结构单独成文件。与它们不同的是，这是本仓**第一个自带子命令**的
//! 外部参数结构——`tools/check_architecture.py` 的 `clap_subcommand_parents` 因此扩了一格：
//! 它过去只在 `cli_args.rs` 里找 `#[command(subcommand)]`，`App(AppArgs)` 这种写法会让 `app`
//! 被当成"没有子命令的父命令"，三条叶子从此没人跑过 `--help`。
//!
//! ## 为什么输入是**文件路径**而不是一堆旗标
//!
//! 三个用例的输入是 `qx-app` 的 spec JSON（`DatasetSpec` / `BacktestSpec` / `BacktestOutcome`），
//! 而 Python SDK 与 HTTP API 那两个入口拿到的也是同一份 JSON 字节。CLI 若把 spec 拆成旗标、
//! 再在本地拼回一份 JSON，三个入口就又开始各拼各的——§15.2「所有业务入口调用同一 `qx-app`
//! use case」要消灭的正是这个形状。所以这里只给**路径**：命令行交出与另两个入口逐字节相同的
//! 那份文档，用例换回同一份结果。
//!
//! ## 为什么没有 `--json`
//!
//! 三条入口的 stdout **本来就只有**那一份 JSON，没有"人读正文"可以切换。给它挂一个只压横幅的
//! `--json` 正是 `cli_json_surface.rs` 守的那类谎话（`config validate|fingerprint|lock` 的旗标
//! 因此被摘掉）。所以 `app` 的横幅一律抑制（见 `Command::machine_output`），一个旗标都不给。

use clap::Subcommand;
use std::path::PathBuf;

/// `app` 的参数：只带子命令，自身没有旗标。
#[derive(clap::Args)]
pub(crate) struct AppArgs {
    #[command(subcommand)]
    pub(crate) action: Option<AppCommand>,
}

/// `app` 的子命令表：与 `qx_app::cases` 的应用研究用例一一对应。
#[derive(Subcommand)]
pub(crate) enum AppCommand {
    /// 数据集校验（`qx_app::validate_dataset`）。
    #[command(name = "validate-dataset")]
    ValidateDataset {
        /// `DatasetSpec` JSON 的路径。
        spec: PathBuf,
    },
    /// Bar 回测（`qx_app::run_backtest`），落四份产物。
    #[command(name = "backtest")]
    Backtest {
        /// `BacktestSpec` JSON 的路径。
        spec: PathBuf,
    },
    /// 从运行时配置的 Provider 获取 Bar、冻结 manifest 并执行共享 Rust 回测。
    #[command(name = "provider-backtest")]
    ProviderBacktest {
        /// 含 `api.data_providers` 的 RuntimeConfig JSON。
        runtime: PathBuf,
        /// `ProviderBacktestSpec` JSON。
        spec: PathBuf,
    },
    /// 从运行时配置的 Provider 获取 Quote 快照并执行共享 Rust 深度回测。
    #[command(name = "provider-depth-backtest")]
    ProviderDepthBacktest {
        /// 含 `api.data_providers` 的 RuntimeConfig JSON。
        runtime: PathBuf,
        /// `ProviderDepthBacktestSpec` JSON。
        spec: PathBuf,
    },
    /// 产物复核（`qx_app::verify_run`）。
    #[command(name = "verify")]
    Verify {
        /// `BacktestOutcome` JSON 的路径（取 `app backtest` 的 stdout）。
        outcome: PathBuf,
    },
    /// 读取并复核 Bar 运行的共享版本化研究报告核心 DTO（`qx_app::read_run_report_v1`）。
    #[command(name = "read-report")]
    ReadReport {
        /// `BacktestOutcome` JSON 的路径。
        outcome: PathBuf,
    },
    /// 读取并复核 Tick/OrderBook 运行的共享版本化研究报告核心 DTO。
    #[command(name = "read-depth-report")]
    ReadDepthReport {
        /// `DepthBacktestOutcome` JSON 的路径。
        outcome: PathBuf,
    },
    /// 读取并复核 Provider Bar 回测封装与共享报告核心 DTO。
    #[command(name = "read-provider-report")]
    ReadProviderReport {
        /// `ProviderBacktestOutcome` JSON 的路径。
        outcome: PathBuf,
    },
    /// 读取并复核 Provider Tick/OrderBook 回测封装与冻结输入。
    #[command(name = "read-provider-depth-report")]
    ReadProviderDepthReport {
        /// `ProviderDepthBacktestOutcome` JSON 的路径。
        outcome: PathBuf,
    },
    /// 读取并复核 Bar/Depth 多账户批次的共享版本化研究报告核心 DTO。
    #[command(name = "read-multi-account-report")]
    ReadMultiAccountReport {
        /// `MultiAccountBacktestResult` 或 `MultiAccountDepthBacktestResult` JSON 的路径。
        outcome: PathBuf,
    },
    /// 读取并复核多交易所联合账户 Bar 回测的共享版本化研究报告核心 DTO。
    #[command(name = "read-multi-account-joint-report")]
    ReadMultiAccountJointReport {
        /// `MultiAccountJointBacktestOutcome` 或含 outcome 与 joint_account_event_pages 的 JSON 路径。
        outcome: PathBuf,
    },
    /// 读取并复核多腿 Bar 回测的共享版本化报告事实 DTO。
    #[command(name = "read-multi-leg-report")]
    ReadMultiLegReport {
        /// `MultiLegBacktestOutcome` JSON 的路径。
        outcome: PathBuf,
    },
    /// 读取并复核完整、有界的多腿 Bar 研究报告 DTO。
    #[command(name = "read-multi-leg-research-report")]
    ReadMultiLegResearchReport {
        /// `MultiLegResearchReportReadRequestV1` JSON 的路径。
        request: PathBuf,
    },
    /// 复核并读取多腿 Bar EventLog 的有界共享页面。
    #[command(name = "read-multi-leg-event-page")]
    ReadMultiLegEventPage {
        /// `MultiLegEventReportReadRequestV1` JSON 的路径。
        request: PathBuf,
    },
    /// 复核并读取多市场 Bar EventLog 的有界共享页面。
    #[command(name = "read-multi-market-event-page")]
    ReadMultiMarketEventPage {
        /// `MultiMarketEventReportReadRequestV1` JSON 的路径。
        request: PathBuf,
    },
    /// 读取并复核完整、有界的共享账户多市场 Bar 研究报告 DTO。
    #[command(name = "read-multi-market-research-report")]
    ReadMultiMarketResearchReport {
        /// `MultiMarketResearchReportReadRequestV1` JSON 的路径。
        request: PathBuf,
    },
    /// 读取并复核完整、有界的共享账户多市场 L1/L2 研究报告 DTO。
    #[command(name = "read-multi-market-depth-research-report")]
    ReadMultiMarketDepthResearchReport {
        /// `MultiMarketDepthResearchReportReadRequestV1` JSON 的路径。
        request: PathBuf,
    },
    /// 复核并读取多市场 Depth EventLog 的有界共享页面。
    #[command(name = "read-multi-market-depth-event-page")]
    ReadMultiMarketDepthEventPage {
        /// `MultiMarketDepthEventReportReadRequestV1` JSON 的路径。
        request: PathBuf,
    },
    /// 确定性比较同一标的与同一数据指纹下的多组已完成回测。
    #[command(name = "compare-runs")]
    CompareRuns {
        /// `CompareRunsSpec` JSON 的路径。
        spec: PathBuf,
    },
    /// 风险平价组合再平衡规划（共享 Rust 计算核，不直接提交订单）。
    #[command(name = "plan-risk-parity")]
    PlanRiskParity {
        /// `RiskParitySpec` JSON 的路径。
        spec: PathBuf,
    },
    /// TWAP/VWAP 拆单计划（不直接提交订单）。
    #[command(name = "plan-algo-execution")]
    PlanAlgoExecution {
        /// `AlgoExecutionSpec` JSON 的路径。
        spec: PathBuf,
    },
    /// 运行有界笛卡尔参数实验并生成 Rust 比较报告。
    #[command(name = "run-experiment")]
    RunExperiment {
        /// `RunExperimentSpec` JSON 的路径。
        spec: PathBuf,
    },
    /// 通过共享 Rust Tick/OrderBook 回测用例执行有界参数网格。
    #[command(name = "run-depth-experiment")]
    RunDepthExperiment {
        /// `RunDepthExperimentSpec` JSON 的路径。
        spec: PathBuf,
    },
    /// 通过共享 Rust 多腿回测用例执行有界参数网格。
    #[command(name = "run-multi-leg-experiment")]
    RunMultiLegExperiment {
        /// `RunMultiLegExperimentSpec` JSON 的路径。
        spec: PathBuf,
    },
    /// 通过共享 Rust 多市场账户账本执行策略/参数网格。
    #[command(name = "run-multi-market-experiment")]
    RunMultiMarketExperiment {
        /// `RunMultiMarketExperimentSpec` JSON 的路径。
        spec: PathBuf,
    },
    /// 同一账户共享 Rust Bar 引擎执行多市场策略组合。
    #[command(name = "multi-market-backtest")]
    MultiMarketBacktest {
        /// `MultiMarketBacktestSpec` JSON 的路径。
        spec: PathBuf,
    },
    /// 同一账户共享 Rust 订单簿引擎执行多市场深度策略组合。
    #[command(name = "multi-market-depth-backtest")]
    MultiMarketDepthBacktest {
        /// `MultiMarketDepthBacktestSpec` JSON 的路径。
        spec: PathBuf,
    },
    /// L1 Tick 或 L2/L3 订单簿回测（复用 Rust 深度撮合内核）。
    #[command(name = "depth-backtest")]
    DepthBacktest {
        /// `DepthBacktestSpec` JSON 的路径。
        spec: PathBuf,
    },
    /// L1/L2 回测产物复核（`qx_app::verify_depth_run`）。
    #[command(name = "verify-depth")]
    VerifyDepth {
        /// `DepthBacktestOutcome` JSON 的路径（取 `app depth-backtest` 的 stdout）。
        outcome: PathBuf,
    },
    /// 双腿 Bar 套利回测（复用 qx-app 与 Rust 撮合引擎）。
    #[command(name = "multi-leg-backtest")]
    MultiLegBacktest {
        /// `MultiLegBacktestSpec` JSON 的路径。
        spec: PathBuf,
    },
    /// 多账户独立 Bar 回测（每账户拥有各自账本，不进行跨账户净额）。
    #[command(name = "multi-account-backtest")]
    MultiAccountBacktest {
        /// `MultiAccountBacktestSpec` JSON 的路径。
        spec: PathBuf,
    },
    /// 多账户独立 Tick/OrderBook 回测（每账户拥有独立账本，不进行跨账户净额）。
    #[command(name = "multi-account-depth-backtest")]
    MultiAccountDepthBacktest {
        /// `MultiAccountDepthBacktestSpec` JSON 的路径。
        spec: PathBuf,
    },
    /// 多交易所多账户分别保留账本并生成汇总权益曲线的 Bar 联合回测。
    #[command(name = "multi-account-joint-backtest")]
    MultiAccountJointBacktest {
        /// `MultiAccountJointBacktestSpec` JSON 的路径。
        spec: PathBuf,
    },
}
