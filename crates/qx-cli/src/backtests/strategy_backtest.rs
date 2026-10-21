//! 跨语言策略回测链：`run_strategy_backtest`（含 L1/L2 归因装配）。

use super::{
    barframe_dataset_identity, read_bar_frame_for_backtest, read_runtime_config,
    run_single_strategy_backtest, verify_strategy_artifact, DatasetRunBinding, JsonDatasetRegistry,
    SingleStrategyBacktestInput,
};
use std::path::Path;

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
        .map(|path| {
            qx_app::artifact_store::verify_barframe_dataset_quality(
                path, frame_path, &frame, &manifest,
            )
        })
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
    let input = qx_app::backtest_input_identity::barframe_input_provenance(frame_path, &manifest);
    let strategy_bindings =
        qx_service::backtest_bindings::strategy_batch::strategy_batch_backtest_bindings(
            &config,
            runtime_path,
            &manifest,
            bars.len(),
        )?;
    for binding in strategy_bindings {
        let bundle_fingerprint = binding.dataset_bundle_fingerprint.as_deref();
        verify_strategy_artifact(&binding.config.strategy)?;
        run_single_strategy_backtest(SingleStrategyBacktestInput {
            config: &binding.config,
            config_fingerprint: binding.config_fingerprint,
            runtime_config_path: Some(runtime_path),
            frame: &frame,
            bars: &bars,
            spec_path,
            strategy_id: &binding.strategy_id,
            dataset_binding: bundle_fingerprint.map(|bundle_fingerprint| DatasetRunBinding {
                bundle_fingerprint,
                component_fingerprints: &binding.dataset_component_fingerprints,
            }),
            run_manifest_root: &data_root,
            input: input.clone(),
            dataset_quality: dataset_quality.clone(),
        })?;
    }
    Ok(())
}
