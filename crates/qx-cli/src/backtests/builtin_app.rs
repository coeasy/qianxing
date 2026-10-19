//! Single-symbol builtin command adapter to the shared strategy backtest use case.

use super::{BuiltinStrategyKind, Quantity};
use std::path::{Path, PathBuf};

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
    let mut config = super::read_runtime_config(&source_config_path)?;
    crate::resolve_strategy_runtime_paths(&mut config.strategy, &source_config_path);
    config.strategies.clear();
    config.strategy.builtin_strategy = Some(kind.name().to_string());
    // The selected command kind is the actual executable strategy. Inheriting the sample
    // runtime's version (often `builtin-macd-v1`) would mislabel every other selected kind and
    // bind the wrong strategy identity into the run manifest.
    config.strategy.version = format!("builtin-{}-v1", kind.name());
    config.strategy.python_module = None;
    config.strategy.external_executable = None;
    config.strategy.external_args.clear();
    config.strategy.external_env.clear();
    config.strategy.strategy_artifact_sha256 = None;
    config.strategy.c_abi_library = None;
    config.strategy.c_abi_sha256 = None;
    config.strategy.c_abi_ed25519_public_key = None;
    config.strategy.c_abi_ed25519_signature = None;
    config.strategy.c_abi_trusted_native = false;
    config.strategy.c_abi_target_triple = None;
    let instrument = super::read_bar_frame_for_backtest(frame_path)?
        .instrument
        .to_string();
    config.strategy.bars_snapshot_path = Some(frame_path.to_string_lossy().into_owned());
    config.strategy.instrument = Some(instrument.clone());
    config.strategy.builtin_quantity = Some(
        i64::try_from(Quantity::from_i64(quantity).raw())
            .map_err(|_| "内置策略 quantity 超出运行时策略定点数量范围".to_string())?,
    );
    config.strategy.builtin_target_notional_raw = None;
    config.strategy.builtin_target_available_margin_bps = None;
    config.strategy.builtin_target_volatility_bps = None;
    config.strategy.builtin_target_kelly_fraction_bps = None;
    config.strategy.builtin_kelly_lookback_bars = None;
    config.strategy.builtin_reference_instrument = None;
    config.strategy.builtin_reference_bars_snapshot_path = None;
    config.strategy.dataset_bundle_path = None;
    config.strategy.dataset_component_paths.clear();
    config.strategy.research_snapshot_path = None;
    config.strategy.research_snapshot_required = false;
    config.strategy.research_data_fingerprint = None;
    let account_id = config.strategy.account_id.clone();
    let venue_id = config.strategy.venue_id.clone();
    for worker in config.workers.iter_mut().filter(|worker| {
        worker.enabled
            && worker.role == qx_runtime::WorkerRole::Strategy
            && worker.account_id == account_id
            && worker.venue_id == venue_id
    }) {
        if !worker.symbols.contains(&instrument) {
            worker.symbols.push(instrument.clone());
        }
    }

    let configured_data_root = PathBuf::from(&config.storage.data_dir);
    let data_root = if configured_data_root.is_absolute() {
        configured_data_root
    } else {
        std::env::current_dir()
            .map_err(|error| format!("读取当前目录失败: {error}"))?
            .join(configured_data_root)
    };
    std::fs::create_dir_all(&data_root)
        .map_err(|error| format!("创建 builtin 数据目录失败 {}: {error}", data_root.display()))?;
    let data_root = data_root
        .canonicalize()
        .map_err(|error| format!("解析 builtin 数据目录失败: {error}"))?;
    let frame_path = frame_path
        .canonicalize()
        .map_err(|error| format!("解析 builtin BarFrame 路径失败: {error}"))?;
    let portable_frame_path = frame_path
        .strip_prefix(&data_root)
        .ok()
        .map(|relative| relative.to_string_lossy().into_owned())
        .filter(|relative| {
            !Path::new(relative).components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::RootDir
                )
            })
        });
    config.strategy.bars_snapshot_path = Some(
        portable_frame_path
            .clone()
            .unwrap_or_else(|| frame_path.to_string_lossy().into_owned()),
    );
    let fingerprint = config.fingerprint()?;
    let snapshot_name = format!("builtin-{}-{fingerprint}.json", kind.name());
    let snapshot_path = if portable_frame_path.is_some() {
        data_root.join(snapshot_name)
    } else {
        data_root.join("runtime-snapshots").join(snapshot_name)
    };
    if let Some(parent) = snapshot_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建 builtin runtime 快照目录失败: {error}"))?;
    }
    let snapshot = serde_json::to_string_pretty(&config)
        .map_err(|error| format!("编码 builtin runtime 快照失败: {error}"))?;
    super::write_backtest_artifact(&snapshot_path, &snapshot, "builtin runtime 快照")?;
    crate::run_strategy_backtest_with_quality_manifest(
        &snapshot_path,
        &frame_path,
        spec_path,
        quality_manifest_path,
    )
}
