//! `plan` 命令的参数契约：`plan <kind> <file> [--json]`。
//!
//! 与 `data_validate_args.rs` 同因：`cli_args.rs` 处在行数棘轮顶格，新增命令的参数结构
//! 单独成文件，既守住预算，也让「哪个命令读哪些参数」一处可见。
//! `kind` 的合法取值由 `qx_spec::FoundationKind` 定义（project / dataset / experiment /
//! run-record / capability / evidence / schema-registry），本文件不另抄一份。

use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub(crate) struct PlanArgs {
    /// 地基规格对象的类型名，交给 `qx_spec::describe` 解析。
    pub(crate) kind: String,
    /// 待读入的规格 JSON 文件路径。
    pub(crate) file: PathBuf,
    /// 打印规范化正文而非一行摘要。
    #[arg(long)]
    pub(crate) json: bool,
}
