//! `console` 命令的参数契约：`console [runtime.json] [--init <path>] [--generate-token]`。
//!
//! 与 `data_validate_args.rs` / `plan_args.rs` 同因：`cli_args.rs` 处在行数棘轮顶格，新增
//! 命令的参数结构单独成文件，也让「这个命令读哪些参数」一处可见。
//!
//! 启动服务的那些面**没有**旗标：监听地址、静态目录、身份与令牌来源全部来自运行时配置的
//! `api.console` 段，命令行的作用只有"用哪一份配置"。引导令牌只从环境变量读
//! （`bootstrap_token_env` 点名那个变量），不接受命令行传入——命令行会进进程列表与
//! shell 历史，那不是放秘密的地方。
//!
//! 两个旗标都**不启动服务**，只服务易用性：`--init` 写出一份就绪模板（拒绝覆盖已有文件），
//! `--generate-token` 打印一枚新令牌与可粘贴的 `export` 行。

use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub(crate) struct ConsoleArgs {
    /// 带 `api.console` 段的运行时配置。
    #[arg(default_value = "deploy/qianxing.runtime.console.example.json", value_parser = crate::parse_deploy_path)]
    pub(crate) path: PathBuf,
    /// 写出一份就绪的运行时模板到指定路径后退出（不启动服务；已存在的文件拒绝覆盖）。
    #[arg(long, value_name = "PATH")]
    pub(crate) init: Option<PathBuf>,
    /// 只打印一枚新生成的引导令牌与可粘贴的 export 行后退出（不启动服务）。
    #[arg(long = "generate-token")]
    pub(crate) generate_token: bool,
}
