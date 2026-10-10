//! `qx-cli app` 的参数表：三个应用层用例各一条入口（T2-2 / 退出门 G1）。
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

/// `app` 的子命令表：与 `qx_app::cases` 的三个用例一一对应。
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
    /// 产物复核（`qx_app::verify_run`）。
    #[command(name = "verify")]
    Verify {
        /// `BacktestOutcome` JSON 的路径（取 `app backtest` 的 stdout）。
        outcome: PathBuf,
    },
}
