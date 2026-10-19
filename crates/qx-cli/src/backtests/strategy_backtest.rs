//! 跨语言策略回测链：`run_strategy_backtest`（含 L1/L2 归因装配）。

use super::{
    barframe_dataset_identity, barframe_input_provenance, read_bar_frame_for_backtest,
    read_example_json, read_runtime_config, run_single_strategy_backtest,
    verify_dataset_bundle_binding, verify_dataset_bundle_component_bindings,
    verify_strategy_artifact, DatasetRunBinding, JsonDatasetRegistry, RunManifest,
    SingleStrategyBacktestInput,
};
use crate::resolve_strategy_runtime_paths;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(crate) fn run_strategy_backtest(
    runtime_path: &Path,
    frame_path: &Path,
    spec_path: Option<&Path>,
) -> Result<(), String> {
    run_strategy_backtest_with_quality_manifest(runtime_path, frame_path, spec_path, None)
}

pub(crate) fn run_strategy_backtest_with_quality_manifest(
    runtime_path: &Path,
    frame_path: &Path,
    spec_path: Option<&Path>,
    quality_manifest_path: Option<&Path>,
) -> Result<(), String> {
    let config = read_runtime_config(runtime_path)?;
    let frame = read_bar_frame_for_backtest(frame_path)?;
    if frame.ts.len() < 2 {
        return Err("跨语言 Bar 回测至少需要两根 Bar".into());
    }
    let (bars, manifest) = barframe_dataset_identity(frame_path, &frame)?;
    let dataset_quality = quality_manifest_path
        .map(|path| verified_dataset_quality(path, frame_path, &frame, &manifest))
        .transpose()?;
    // `storage.data_dir` 只有一条落点：相对进程当前目录，与可写运行态、`report`/`status` 读侧
    // 和事件回测证据闸门同指一棵树。产物曾按「相对 runtime.json 同级」另立口径，于是同一份配置
    // 下 `fast-backtest` 写出的 runs/ 落在闸门永远不看的那棵树里（V13 R2 第二十七遍 #255）。
    let data_root = Path::new(&config.storage.data_dir).to_path_buf();
    let mut dataset_registry = JsonDatasetRegistry::open(data_root.join("datasets.manifest.json"))?;
    dataset_registry.register(manifest.clone())?;
    dataset_registry.verify(
        &manifest.dataset_id,
        &manifest.version,
        &manifest.fingerprint,
    )?;
    println!(
        "[Data · Dataset] dataset={} version={} source={} fingerprint={}",
        manifest.dataset_id, manifest.version, manifest.source, manifest.fingerprint
    );
    if let Some(quality) = dataset_quality.as_ref() {
        println!(
            "[Data · Quality] manifest_fingerprint={} rows={} interval_ms={} missing_intervals={}",
            quality["manifest_fingerprint"]
                .as_str()
                .unwrap_or("unknown"),
            quality["quality_report"]["row_count"]
                .as_u64()
                .unwrap_or_default(),
            quality["quality_report"]["interval_ms"]
                .as_u64()
                .unwrap_or_default(),
            quality["quality_report"]["missing_intervals"]
                .as_u64()
                .unwrap_or_default(),
        );
    }
    // 同一份身份必须跟着产物走：只印在 stdout 上的指纹，事后拿着摘要无从复核（V11 Q66）。
    let input = barframe_input_provenance(frame_path, &manifest);
    let strategies = if config.strategies.is_empty() {
        vec![config.strategy.clone()]
    } else {
        config.strategies.clone()
    };
    for strategy in strategies {
        let strategy_id = strategy
            .id
            .clone()
            .unwrap_or_else(|| strategy.version.clone());
        let mut strategy_config = config.clone();
        strategy_config.strategy = strategy;
        strategy_config.strategies.clear();
        // Run identity binds the declared config, whose relative dependency names survive
        // portable EvidenceBundle export. Runtime execution below still uses resolved paths.
        let config_fingerprint = strategy_config.fingerprint()?;
        resolve_strategy_runtime_paths(&mut strategy_config.strategy, runtime_path);
        let (bundle_fingerprint, bundle_components) =
            if let Some(bundle_path) = strategy_config.strategy.dataset_bundle_path.as_deref() {
                let bundle_payload =
                    read_example_json(Path::new(bundle_path), "策略 DatasetBundleManifest 组件 ")?;
                let bundle: qx_data::DatasetBundleManifest = serde_json::from_str(&bundle_payload)
                    .map_err(|error| format!("策略 DatasetBundleManifest JSON 无效: {error}"))?;
                let fingerprint =
                    verify_dataset_bundle_binding(Path::new(bundle_path), &manifest, bars.len())?;
                verify_dataset_bundle_component_bindings(
                    &bundle,
                    &strategy_config.strategy,
                    &strategy_id,
                )?;
                let components = bundle
                    .components
                    .iter()
                    .map(|(kind, component)| (kind.clone(), component.dataset.fingerprint.clone()))
                    .collect::<BTreeMap<_, _>>();
                (Some(fingerprint), Some(components))
            } else {
                (None, None)
            };
        if let Some(bundle_path) = strategy_config.strategy.dataset_bundle_path.as_deref() {
            let bundle_payload =
                read_example_json(Path::new(bundle_path), "策略 DatasetBundleManifest 组件 ")?;
            let bundle: qx_data::DatasetBundleManifest = serde_json::from_str(&bundle_payload)
                .map_err(|error| format!("策略 DatasetBundleManifest JSON 无效: {error}"))?;
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
        }
        verify_strategy_artifact(&strategy_config.strategy)?;
        run_single_strategy_backtest(SingleStrategyBacktestInput {
            config: &strategy_config,
            config_fingerprint,
            runtime_config_path: Some(runtime_path),
            frame: &frame,
            bars: &bars,
            spec_path,
            strategy_id: &strategy_id,
            dataset_binding: bundle_fingerprint
                .as_deref()
                .zip(bundle_components.as_ref())
                .map(
                    |(bundle_fingerprint, component_fingerprints)| DatasetRunBinding {
                        bundle_fingerprint,
                        component_fingerprints,
                    },
                ),
            run_manifest_root: &data_root,
            input: input.clone(),
            dataset_quality: dataset_quality.clone(),
        })?;
    }
    Ok(())
}

pub(crate) fn verified_dataset_quality(
    path: &Path,
    frame_path: &Path,
    frame: &qx_datastruct::BarFrame,
    input_manifest: &qx_data::DatasetManifest,
) -> Result<serde_json::Value, String> {
    let payload = read_example_json(path, "数据集质量清单 v2")?;
    let quality_manifest = qx_data::DatasetManifestV2::from_json(&payload)?;
    let expected_content_hash = qx_strategy::file_digest::sha256_file_hex(frame_path)?;
    let declared = &quality_manifest.dataset;
    for (field, actual, expected) in [
        (
            "dataset_id",
            declared.dataset_id.as_str(),
            input_manifest.dataset_id.as_str(),
        ),
        (
            "version",
            declared.version.as_str(),
            input_manifest.version.as_str(),
        ),
        (
            "fingerprint",
            declared.fingerprint.as_str(),
            input_manifest.fingerprint.as_str(),
        ),
        (
            "content_hash",
            quality_manifest.content_hash.as_str(),
            expected_content_hash.as_str(),
        ),
    ] {
        if actual != expected {
            return Err(format!(
                "数据集质量清单与本次 BarFrame 不一致: {field} 清单={actual} 实际={expected} ({})",
                path.display()
            ));
        }
    }
    let declared_instrument = qx_core::InstrumentId::parse(&quality_manifest.instrument)
        .ok_or_else(|| {
            format!(
                "数据集质量清单 instrument 非法: {}",
                quality_manifest.instrument
            )
        })?;
    if declared_instrument != frame.instrument {
        return Err(format!(
            "数据集质量清单与本次 BarFrame 不一致: instrument 清单={} 实际={} ({})",
            quality_manifest.instrument,
            frame.instrument,
            path.display()
        ));
    }
    let Some((&first_ts, &last_ts)) = frame.ts.first().zip(frame.ts.last()) else {
        return Err(format!(
            "BarFrame 缺少时间戳，无法复核数据集质量清单 ({})",
            path.display()
        ));
    };
    if quality_manifest.quality_report.row_count != frame.ts.len() as u64
        || quality_manifest.tier != qx_data::DatasetTier::Bar
        || quality_manifest.provider_version != qx_data::BARFRAME_DATASET_VERSION
        || declared.start_timestamp != first_ts
        || declared.end_timestamp != last_ts
    {
        return Err(format!(
            "数据集质量清单的样本数或时间范围与本次 BarFrame 不一致 ({})",
            path.display()
        ));
    }
    let interval_ms = quality_manifest
        .quality_report
        .interval_ms
        .filter(|interval| *interval > 0)
        .ok_or_else(|| {
            format!(
                "数据集质量清单缺少可复核的 interval_ms，需用当前 data-validate 重新生成 ({})",
                path.display()
            )
        })?;
    let recomputed_missing = frame
        .ts
        .windows(2)
        .try_fold(0_u64, |total, pair| {
            let missing = (pair[1] - pair[0]).div_ceil(interval_ms) - 1;
            total.checked_add(missing)
        })
        .ok_or_else(|| format!("按 interval_ms 复算缺口数溢出 ({})", path.display()))?;
    if quality_manifest.quality_report.missing_intervals != recomputed_missing
        || quality_manifest.quality_report.duplicate_rows != 0
        || quality_manifest.quality_report.out_of_order_rows != 0
    {
        return Err(format!(
            "数据集质量报告与严格 BarFrame 实况不一致: missing_intervals 清单={} 重算={}，duplicate_rows={}，out_of_order_rows={} ({})",
            quality_manifest.quality_report.missing_intervals,
            recomputed_missing,
            quality_manifest.quality_report.duplicate_rows,
            quality_manifest.quality_report.out_of_order_rows,
            path.display()
        ));
    }
    let manifest_fingerprint = quality_manifest.fingerprint()?;
    Ok(serde_json::json!({
        "manifest_path": path.to_string_lossy(),
        "manifest_fingerprint": manifest_fingerprint,
        "content_hash": quality_manifest.content_hash,
        "quality_report": quality_manifest.quality_report,
        "manifest": quality_manifest.clone(),
    }))
}

pub(crate) fn persist_backtest_run_manifest(
    root: &Path,
    manifest: &RunManifest,
) -> Result<PathBuf, String> {
    qx_app::artifact_store::persist_run_manifest(root, manifest)
        .map_err(|error| error.message().to_string())
}

pub(crate) fn write_backtest_artifact(
    path: &Path,
    payload: &str,
    label: &str,
) -> Result<(), String> {
    qx_app::artifact_store::write_backtest_artifact(path, payload, label)
}
