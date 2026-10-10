//! `ccxt-submit-order` 一次性提交入口的参数契约。
//!
//! 与 `plan_args.rs` / `data_validate_args.rs` 同因：`cli_args.rs` 处在行数棘轮顶格，
//! 新命令的参数结构单独成文件，既守住预算，也让「哪个命令读哪些参数」一处可见。
//! CCXT 凭据不进本文件：凭据的环境变量名写在 CCXT 配置 JSON 的 `credential_env` 里，
//! 运行时由公共 CCXT Worker 从环境变量读取（见 `python/qianxing_ccxt/worker.py`）。

use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub(crate) struct CcxtSubmitArgs {
    /// 运行时配置（含目标 worker 拓扑与存储根）。
    pub(crate) path: PathBuf,
    /// 执行 SubmitOrder 的 CCXT worker-id（必须已启用、角色为 Execution、Venue 绑定 CCXT）。
    pub(crate) worker_id: String,
    /// CCXT 配置 JSON（exchange_id 必须与 worker 的 venue_id 一致；凭据经环境变量名引用）。
    pub(crate) ccxt_config: PathBuf,
    /// SubmitOrder 控制命令 JSON（含 order / command_id / request_id / permission）。
    pub(crate) command_path: PathBuf,
}
