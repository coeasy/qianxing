//! Single-symbol builtin command adapter to the shared strategy backtest use case.

use super::BuiltinStrategyKind;
use std::path::Path;

pub(crate) fn run_builtin_backtest_via_app(
    strategy_name: &str,
    frame_path: &Path,
    spec_path: Option<&Path>,
    quantity: i64,
    runtime_config_path: Option<&Path>,
    quality_manifest_path: Option<&Path>,
) -> Result<(), String> {
    if quantity <= 0 {
        return Err("内置策略 quantity 必须为正整数".into());
    }
    let kind = BuiltinStrategyKind::parse(strategy_name)?;
    let source_config_path = runtime_config_path
        .map(Path::to_path_buf)
        .unwrap_or_else(crate::unified_default_runtime_path);
    let config = super::read_runtime_config(&source_config_path)?;
    run_builtin_backtest_with_runtime_snapshot(
        frame_path,
        spec_path,
        quantity,
        quality_manifest_path,
        &source_config_path,
        kind,
        config,
    )
}

/// Keep the CLI at the single BarFrame reader, then delegate snapshot preparation and execution.
/// CCXT validates its runtime before starting a worker and passes the same config snapshot here.
pub(crate) fn run_builtin_backtest_with_runtime_snapshot(
    frame_path: &Path,
    spec_path: Option<&Path>,
    quantity: i64,
    quality_manifest_path: Option<&Path>,
    source_config_path: &Path,
    kind: BuiltinStrategyKind,
    config: qx_runtime::RuntimeConfig,
) -> Result<(), String> {
    // A bare `--config runtime.json` has an empty relative parent on Windows and Unix. Resolve
    // it before freezing the config: otherwise strategy asset paths remain relative and are
    // later interpreted from `data_dir/runtime-snapshots/` when the snapshot is replayed.
    let source_config_path = source_config_path.canonicalize().map_err(|error| {
        format!(
            "解析 builtin runtime 配置路径失败 {}: {error}",
            source_config_path.display()
        )
    })?;
    let instrument = super::read_bar_frame_for_backtest(frame_path)?
        .instrument
        .to_string();
    let prepared = qx_service::backtest_bindings::prepare_builtin_backtest_snapshot(
        qx_service::backtest_bindings::BuiltinBacktestSnapshotInput {
            frame_path,
            instrument: &instrument,
            quantity,
            source_config_path: &source_config_path,
            kind,
            config,
        },
    )?;
    crate::run_strategy_backtest_with_quality_manifest(
        &prepared.runtime_snapshot_path,
        &prepared.frame_path,
        spec_path,
        quality_manifest_path,
    )
}
