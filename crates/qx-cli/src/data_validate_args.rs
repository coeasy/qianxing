//! `data-validate` 的参数契约，与顶层命令表分开以遵守 cli_args.rs 行数棘轮。

use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub(crate) struct DatasetValidateArgs {
    pub(crate) frame: PathBuf,
    pub(crate) dataset_id: String,
    pub(crate) version: String,
    pub(crate) interval_ms: u64,
    #[arg(long, default_value = "UTC")]
    pub(crate) timezone: String,
    pub(crate) output: PathBuf,
    #[arg(long)]
    pub(crate) json: bool,
}
