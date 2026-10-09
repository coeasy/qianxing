//! `console` 命令的参数契约：`console [runtime.json]`。
//!
//! 与 `data_validate_args.rs` / `plan_args.rs` 同因：`cli_args.rs` 处在行数棘轮顶格，新增
//! 命令的参数结构单独成文件，也让「这个命令读哪些参数」一处可见。
//!
//! 控制台面**没有**旗标：监听地址、静态目录、身份与令牌来源全部来自运行时配置的
//! `api.console` 段，命令行的作用只有"用哪一份配置"。引导令牌只从环境变量读
//! （`bootstrap_token_env` 点名那个变量），不接受命令行传入——命令行会进进程列表与
//! shell 历史，那不是放秘密的地方。

use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub(crate) struct ConsoleArgs {
    /// 带 `api.console` 段的运行时配置。
    #[arg(default_value = "deploy/qianxing.runtime.console.example.json", value_parser = crate::parse_deploy_path)]
    pub(crate) path: PathBuf,
}
