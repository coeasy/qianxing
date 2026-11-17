//! Runtime and DatasetBundle preparation shared by single and multi-strategy Bar backtests.

use qx_runtime::{resolve_strategy_runtime_paths, RuntimeConfig};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct StrategyBatchBacktestBinding {
    pub config: RuntimeConfig,
    pub strategy_id: String,
    pub config_fingerprint: String,
    pub dataset_bundle_fingerprint: Option<String>,
    pub dataset_component_fingerprints: BTreeMap<String, String>,
}

/// Apply the canonical version used by built-in strategy run identity.
pub fn normalize_builtin_strategy_version(
    strategy: &mut qx_runtime::StrategyRuntimeConfig,
) -> Result<(), String> {
    if let Some(name) = strategy.builtin_strategy.as_deref() {
        let kind = qx_strategy::BuiltinStrategyKind::parse(name)?;
        strategy.version = format!("builtin-{}-v1", kind.name());
    }
    Ok(())
}

/// Freeze the declared strategy configurations and validate their DatasetBundle inputs once.
/// Fingerprints are calculated before paths are resolved so portable run identity retains the
/// user's declared relative paths while execution receives resolved paths.
pub fn strategy_batch_backtest_bindings(
    config: &RuntimeConfig,
    runtime_path: &Path,
    bars_manifest: &qx_data::DatasetManifest,
    bars_frame: &qx_datastruct::BarFrame,
    bars_content_sha256: &str,
) -> Result<Vec<StrategyBatchBacktestBinding>, String> {
    let strategies = if config.strategies.is_empty() {
        vec![config.strategy.clone()]
    } else {
        config.strategies.clone()
    };
    strategies
        .into_iter()
        .map(|mut strategy| {
            normalize_builtin_strategy_version(&mut strategy)?;
            let strategy_id = strategy
                .id
                .clone()
                .unwrap_or_else(|| strategy.version.clone());
            let mut strategy_config = config.clone();
            strategy_config.strategy = strategy;
            strategy_config.strategies.clear();
            let config_fingerprint = strategy_config.fingerprint()?;
            resolve_strategy_runtime_paths(&mut strategy_config.strategy, runtime_path);

            let mut dataset_bundle_fingerprint = None;
            let mut dataset_component_fingerprints = BTreeMap::new();
            if let Some(bundle_path) = strategy_config.strategy.dataset_bundle_path.as_deref() {
                let bundle_path = PathBuf::from(bundle_path);
                let payload = std::fs::read_to_string(&bundle_path).map_err(|error| {
                    format!(
                        "读取策略 DatasetBundleManifest 组件失败 {}: {error}",
                        bundle_path.display()
                    )
                })?;
                let bundle: qx_data::DatasetBundleManifest = serde_json::from_str(&payload)
                    .map_err(|error| {
                        format!(
                            "策略 DatasetBundleManifest JSON 无效 {}: {error}",
                            bundle_path.display()
                        )
                    })?;
                let fingerprint = qx_app::dataset_bundle::verify_dataset_bundle_bars(
                    &bundle,
                    bars_manifest,
                    bars_frame.ts.len(),
                )
                .map_err(|error| error.message().to_string())?;
                let snapshot = bundle.snapshots.get("bars").ok_or_else(|| {
                    format!(
                        "策略 DatasetBundle 缺少 bars snapshot，拒绝执行无来源绑定的 BarFrame: {strategy_id}"
                    )
                })?;
                verify_bundle_bar_snapshot(
                    snapshot,
                    bars_manifest,
                    bars_frame,
                    bars_content_sha256,
                )?;
                let snapshot_sha256 = Some(snapshot.content_sha256()?);
                verify_dataset_bundle_component_bindings(
                    &bundle,
                    &strategy_config.strategy,
                    &strategy_id,
                )?;
                if bundle.component("corporate_actions").is_some()
                    && !strategy_config
                        .strategy
                        .dataset_component_paths
                        .contains_key("corporate_actions")
                    && strategy_config.strategy.ashare_actions_path.is_none()
                {
                    return Err(format!(
                        "策略 Bundle 包含 corporate_actions，但未配置 ashare_actions_path: {strategy_id}"
                    ));
                }
                if bundle.component("calendar").is_some()
                    && !strategy_config
                        .strategy
                        .dataset_component_paths
                        .contains_key("calendar")
                    && strategy_config.strategy.ashare_calendar_path.is_none()
                {
                    return Err(format!(
                        "策略 Bundle 包含 calendar，但未配置 ashare_calendar_path: {strategy_id}"
                    ));
                }
                dataset_bundle_fingerprint = Some(fingerprint);
                dataset_component_fingerprints = bundle
                    .components
                    .into_iter()
                    .map(|(kind, component)| (kind, component.dataset.fingerprint))
                    .collect();
                if let Some(snapshot_sha256) = snapshot_sha256 {
                    dataset_component_fingerprints
                        .insert("dataset_snapshot_sha256".into(), snapshot_sha256);
                }
            }

            Ok(StrategyBatchBacktestBinding {
                config: strategy_config,
                strategy_id,
                config_fingerprint,
                dataset_bundle_fingerprint,
                dataset_component_fingerprints,
            })
        })
        .collect()
}

fn verify_bundle_bar_snapshot(
    snapshot: &qx_data::DatasetSnapshotV1,
    bars_manifest: &qx_data::DatasetManifest,
    bars_frame: &qx_datastruct::BarFrame,
    bars_content_sha256: &str,
) -> Result<(), String> {
    snapshot.validate()?;
    let manifest = &snapshot.manifest;
    let instrument = bars_frame.instrument.to_string();
    if manifest.tier != qx_data::DatasetTier::Bar
        || !matches!(
            snapshot.timestamp_semantics,
            qx_data::DatasetTimestampSemantics::BarOpen
                | qx_data::DatasetTimestampSemantics::BarClose
        )
        || manifest.instrument != instrument
        || manifest.dataset.dataset_id != bars_manifest.dataset_id
        || manifest.dataset.version != bars_manifest.version
        || manifest.dataset.source != bars_manifest.source
        || manifest.dataset.fingerprint != bars_manifest.fingerprint
        || manifest.dataset.schema_version != bars_manifest.schema_version
        || manifest.dataset.start_timestamp != bars_manifest.start_timestamp
        || manifest.dataset.end_timestamp != bars_manifest.end_timestamp
        || manifest.content_hash != bars_content_sha256
        || manifest.quality_report.row_count != bars_frame.ts.len() as u64
        || manifest.dataset.start_timestamp != bars_frame.ts.first().copied().unwrap_or(0)
        || manifest.dataset.end_timestamp != bars_frame.ts.last().copied().unwrap_or(0)
    {
        return Err("策略 DatasetBundle bars snapshot does not bind to the exact BarFrame".into());
    }
    if snapshot.frequency_ms == 0 {
        return Err("策略 DatasetBundle bars snapshot frequency_ms must be positive".into());
    }
    let mut gaps = Vec::new();
    let mut missing = 0_u64;
    for pair in bars_frame.ts.windows(2) {
        let delta = pair[1] - pair[0];
        missing = missing
            .checked_add(delta.div_ceil(snapshot.frequency_ms) - 1)
            .ok_or_else(|| "DatasetBundle snapshot gap count overflow".to_string())?;
        if delta > snapshot.frequency_ms {
            gaps.push(qx_data::DatasetCoverageGapV1 {
                start_ts_ms: pair[0].saturating_add(snapshot.frequency_ms),
                end_ts_ms: pair[1],
            });
        }
    }
    if snapshot.gaps != gaps || manifest.quality_report.missing_intervals != missing {
        return Err("策略 DatasetBundle bars snapshot gaps do not match the BarFrame".into());
    }
    Ok(())
}

pub fn verify_dataset_bundle_component_bindings(
    bundle: &qx_data::DatasetBundleManifest,
    strategy: &qx_runtime::StrategyRuntimeConfig,
    strategy_id: &str,
) -> Result<(), String> {
    qx_app::dataset_bundle::verify_dataset_bundle_components(
        bundle,
        &qx_app::dataset_bundle::DatasetBundleComponentBindings {
            strategy_id,
            component_paths: &strategy.dataset_component_paths,
            ashare_actions_path: strategy.ashare_actions_path.as_deref(),
            ashare_calendar_path: strategy.ashare_calendar_path.as_deref(),
        },
        |path, _kind, format| {
            let label = match format {
                qx_data::DatasetComponentFormat::Json => " DatasetBundle 组件",
                qx_data::DatasetComponentFormat::Arrow => " Arrow DatasetBundle 组件 manifest ",
            };
            std::fs::read_to_string(path).map_err(|error| {
                qx_app::AppError::new(
                    qx_app::AppErrorCategory::InvalidInput,
                    format!("读取{label}失败 {}: {error}", path.display()),
                )
            })
        },
    )
    .map_err(|error| error.message().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategy_batch_binding_validates_and_retains_one_dataset_bundle_snapshot() {
        let deploy = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .join("deploy");
        let runtime_path = deploy.join("qianxing.runtime.example.json");
        let bars_path = deploy.join("qianxing.bar-frame.example.json");
        let frame = qx_datastruct::BarFrame::from_json(
            &std::fs::read_to_string(&bars_path).expect("read BarFrame example"),
        )
        .expect("BarFrame example is valid");
        let (_bars, manifest) =
            qx_app::backtest_input_identity::barframe_dataset_identity(&bars_path, &frame)
                .expect("BarFrame identity is valid");
        let mut runtime = RuntimeConfig::from_json(include_str!(
            "../../../../deploy/qianxing.runtime.example.json"
        ))
        .expect("runtime example config is valid");
        runtime.strategy.id = None;
        runtime.strategy.version = "user-declared-builtin-version".into();
        runtime.strategy.builtin_strategy = Some("sma_cross".into());
        runtime.strategy.dataset_bundle_path =
            Some("qianxing.dataset-bundle.bar-frame.example.json".into());

        let bindings = strategy_batch_backtest_bindings(
            &runtime,
            &runtime_path,
            &manifest,
            &frame,
            &qx_data::sha256_hex_bytes(&std::fs::read(&bars_path).expect("read BarFrame bytes")),
        )
        .expect("one validated strategy binding should be returned");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].strategy_id, "builtin-sma_cross-v1");
        assert_eq!(bindings[0].config.strategy.version, "builtin-sma_cross-v1");
        assert_eq!(
            bindings[0].dataset_component_fingerprints.get("bars"),
            Some(&manifest.fingerprint)
        );
        assert!(bindings[0].dataset_bundle_fingerprint.is_some());
        assert!(!bindings[0].config_fingerprint.is_empty());
    }
}
