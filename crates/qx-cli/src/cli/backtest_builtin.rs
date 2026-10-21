use std::path::PathBuf;

use crate::run_builtin_backtest;

pub(super) fn dispatch_builtin_backtest(
    strategy: String,
    frame: PathBuf,
    spec: Option<PathBuf>,
    quantity: Option<i64>,
    config: Option<PathBuf>,
    dataset_manifest: Option<PathBuf>,
) {
    if let Err(error) = run_builtin_backtest(
        &strategy,
        &frame,
        spec.as_deref(),
        quantity.unwrap_or(1),
        config.as_deref(),
        dataset_manifest.as_deref(),
    ) {
        eprintln!("[qx-cli · CLI] 内置策略回测失败: {error}");
        std::process::exit(2);
    }
}
