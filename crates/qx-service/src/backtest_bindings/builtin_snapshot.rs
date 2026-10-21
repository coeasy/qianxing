//! Preparation of immutable runtime snapshots for builtin backtests.

use super::*;
use std::path::{Path, PathBuf};

pub struct PreparedBuiltinBacktestSnapshot {
    pub runtime_snapshot_path: PathBuf,
    pub frame_path: PathBuf,
}

pub struct BuiltinBacktestSnapshotInput<'a> {
    pub frame_path: &'a Path,
    pub instrument: &'a str,
    pub quantity: i64,
    pub source_config_path: &'a Path,
    pub kind: qx_strategy::BuiltinStrategyKind,
    pub config: RuntimeConfig,
}

/// Normalize runtime declarations for the selected builtin strategy and publish the immutable
/// snapshot consumed by the shared strategy-backtest workflow.
pub fn prepare_builtin_backtest_snapshot(
    input: BuiltinBacktestSnapshotInput<'_>,
) -> Result<PreparedBuiltinBacktestSnapshot, String> {
    let BuiltinBacktestSnapshotInput {
        frame_path,
        instrument,
        quantity,
        source_config_path,
        kind,
        mut config,
    } = input;
    if quantity <= 0 {
        return Err("内置策略 quantity 必须为正整数".into());
    }
    qx_runtime::resolve_strategy_runtime_paths(&mut config.strategy, source_config_path);
    config.strategies.clear();
    config.strategy.builtin_strategy = Some(kind.name().to_string());
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
    config.strategy.bars_snapshot_path = Some(frame_path.to_string_lossy().into_owned());
    config.strategy.instrument = Some(instrument.to_string());
    config.strategy.builtin_quantity = Some(
        i64::try_from(qx_core::Quantity::from_i64(quantity).raw())
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
        if !worker.symbols.iter().any(|symbol| symbol == instrument) {
            worker.symbols.push(instrument.to_string());
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
    qx_app::artifact_store::write_backtest_artifact(
        &snapshot_path,
        &snapshot,
        "builtin runtime 快照",
    )?;
    Ok(PreparedBuiltinBacktestSnapshot {
        runtime_snapshot_path: snapshot_path,
        frame_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_CASE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn builtin_snapshot_normalizes_strategy_identity_and_portable_frame_path() {
        let workspace_target =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/qx-service-tests");
        std::fs::create_dir_all(&workspace_target).expect("create workspace test root");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after Unix epoch")
            .as_nanos();
        let root = workspace_target.join(format!(
            "builtin-snapshot-{}-{nonce}-{}",
            std::process::id(),
            NEXT_CASE.fetch_add(1, Ordering::Relaxed)
        ));
        let data_root = root.join("data");
        std::fs::create_dir_all(&data_root).expect("create temporary data root");
        let frame_path = data_root.join("bars.json");
        std::fs::write(
            &frame_path,
            include_str!("../../../../deploy/qianxing.ashare.bar-frame.example.json"),
        )
        .expect("write fixture BarFrame");
        let instrument = qx_datastruct::BarFrame::from_json(include_str!(
            "../../../../deploy/qianxing.ashare.bar-frame.example.json"
        ))
        .expect("fixture BarFrame is valid")
        .instrument
        .to_string();
        let source_config_path = root.join("runtime.json");
        let mut config = RuntimeConfig::from_json(include_str!(
            "../../../../deploy/qianxing.runtime.example.json"
        ))
        .expect("runtime example config is valid");
        config.storage.data_dir = data_root.to_string_lossy().into_owned();
        config.strategy.external_executable = Some("must-be-cleared".into());
        config.strategy.builtin_reference_instrument = Some("OTHER.SYMBOL".into());
        config.strategies.push(config.strategy.clone());
        let account_id = config.strategy.account_id.clone();
        let venue_id = config.strategy.venue_id.clone();
        let worker = config
            .workers
            .iter_mut()
            .find(|worker| worker.role == qx_runtime::WorkerRole::Strategy)
            .expect("runtime fixture has a strategy worker");
        worker.enabled = true;
        worker.account_id = account_id.clone();
        worker.venue_id = venue_id.clone();
        worker.symbols.retain(|symbol| symbol != &instrument);

        let rejected = prepare_builtin_backtest_snapshot(BuiltinBacktestSnapshotInput {
            frame_path: &frame_path,
            instrument: &instrument,
            quantity: 0,
            source_config_path: &source_config_path,
            kind: qx_strategy::BuiltinStrategyKind::SmaCross,
            config: config.clone(),
        });
        assert_eq!(
            rejected.err().as_deref(),
            Some("内置策略 quantity 必须为正整数")
        );

        let prepared = prepare_builtin_backtest_snapshot(BuiltinBacktestSnapshotInput {
            frame_path: &frame_path,
            instrument: &instrument,
            quantity: 100,
            source_config_path: &source_config_path,
            kind: qx_strategy::BuiltinStrategyKind::SmaCross,
            config,
        })
        .expect("prepare builtin runtime snapshot");
        let snapshot = std::fs::read_to_string(&prepared.runtime_snapshot_path)
            .expect("read persisted runtime snapshot");
        let snapshot = RuntimeConfig::from_json(&snapshot).expect("snapshot is valid runtime JSON");

        assert_eq!(
            prepared.frame_path,
            frame_path.canonicalize().expect("canonical frame")
        );
        let canonical_data_root = data_root.canonicalize().expect("canonical data root");
        assert_eq!(
            prepared.runtime_snapshot_path.parent(),
            Some(canonical_data_root.as_path())
        );
        assert_eq!(
            snapshot.strategy.builtin_strategy.as_deref(),
            Some("sma_cross")
        );
        assert_eq!(snapshot.strategy.version, "builtin-sma_cross-v1");
        assert_eq!(
            snapshot.strategy.instrument.as_deref(),
            Some(instrument.as_str())
        );
        let expected_quantity = i64::try_from(qx_core::Quantity::from_i64(100).raw())
            .expect("fixture quantity fits runtime representation");
        assert_eq!(snapshot.strategy.builtin_quantity, Some(expected_quantity));
        assert_eq!(
            snapshot.strategy.bars_snapshot_path.as_deref(),
            Some("bars.json")
        );
        assert!(snapshot.strategy.external_executable.is_none());
        assert!(snapshot.strategy.builtin_reference_instrument.is_none());
        assert!(snapshot.strategies.is_empty());
        let worker = snapshot
            .workers
            .iter()
            .find(|worker| worker.role == qx_runtime::WorkerRole::Strategy)
            .expect("matching strategy worker remains configured");
        assert_eq!(worker.account_id, account_id);
        assert_eq!(worker.venue_id, venue_id);
        assert!(worker.symbols.contains(&instrument));

        std::fs::remove_dir_all(root).expect("remove temporary snapshot fixture");
    }
}
