//! `fast-backtest` manifest 驱动的离线快回测入口。

use crate::{read_example_json, relocate_deploy_path, run_strategy_backtest_with_quality_manifest};
use qx_app::fast_backtest::{parse_fast_backtest_manifest as parse_manifest, FastBacktestJob};
use std::path::Path;

/// manifest 的**解析段**：只做读文件与形状判定，不起线程、不落产物。
///
/// 拆出来是为了让样例模板能被用例读一次（V13 R1-A6）：执行段在 `run_strategy_backtest`
/// 里跑完整回测，用例付不起那个代价，于是"这份 manifest 有没有人读过"就会长期没人回答。
pub(crate) fn parse_fast_backtest_manifest(
    manifest_path: &Path,
) -> Result<Vec<FastBacktestJob>, String> {
    let payload = read_example_json(manifest_path, "快速回测 manifest ")?;
    parse_manifest(manifest_path, &payload)
}

pub(crate) fn run_fast_backtest_manifest(manifest_path: &Path) -> Result<(), String> {
    // 文档那条命令是从任何目录敲的，manifest 得先按查找面定位；作业里引用的是 manifest 的
    // 同级文件，所以这一步决定了后面几格读取按哪个目录展开。
    let manifest_path = &relocate_deploy_path(manifest_path);
    let parsed = parse_fast_backtest_manifest(manifest_path)?;
    let jobs = parsed.len();
    let available = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let completed = qx_app::fast_backtest::run_fast_backtest_jobs(&parsed, available, |job| {
        run_strategy_backtest_with_quality_manifest(
            &job.runtime,
            &job.bars,
            job.market_spec.as_deref(),
            job.dataset_manifest.as_deref(),
        )
        .map(|_| ())
    })?;
    println!(
        "[Fast Backtest] manifest={} jobs={} completed={}",
        manifest_path.display(),
        jobs,
        completed
    );
    Ok(())
}
