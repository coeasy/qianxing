//! DatasetBundle 的 CLI 边界校验。
//!
//! 该模块只负责把运行时输入文件绑定到不可变 DatasetBundle；数据模型和
//! 持久化仍由 qx-data 提供，回测编排不再直接承担组件指纹细节。

use super::read_example_json;
use crate::contract_adapters;
use qx_data::{JsonBarFrameProvider, JsonDatasetRegistry};
use qx_datastruct::BarFrame;
use qx_guanxing::Bar;
use std::path::Path;

pub(crate) fn run_dataset_ingest(
    frame_path: &Path,
    dataset_id: &str,
    dataset_version: &str,
    data_root: &Path,
) -> Result<(), String> {
    let payload = read_example_json(frame_path, "数据集 BarFrame ")?;
    let frame = BarFrame::from_json(&payload)
        .map_err(|error| format!("数据集 BarFrame 校验失败: {error:?}"))?;
    let start = frame
        .ts
        .first()
        .copied()
        .ok_or_else(|| "数据集 BarFrame 不能为空".to_string())?;
    let end = frame
        .ts
        .last()
        .copied()
        .ok_or_else(|| "数据集 BarFrame 不能为空".to_string())?;
    let provider = JsonBarFrameProvider::new(frame.source.0.clone(), dataset_version, frame_path);
    let mut storage = qx_data::JsonFileDataStorage::new(data_root)?;
    let registry_path = data_root.join("datasets.manifest.json");
    let mut registry = JsonDatasetRegistry::open(&registry_path)?;
    let report = qx_data::ingest_bars(
        &provider,
        &qx_data::IngestionRequest {
            dataset_id: dataset_id.into(),
            dataset_version: dataset_version.into(),
            instrument: frame.instrument.to_string(),
            start,
            end,
        },
        &mut storage,
        &mut registry,
    )?;
    println!(
        "[Data · Ingest] dataset={} version={} source={} incoming={} stored={} fingerprint={} root={}",
        report.dataset_id,
        report.dataset_version,
        report.manifest.source,
        report.incoming_rows,
        report.stored_rows,
        report.manifest.fingerprint,
        data_root.display()
    );
    Ok(())
}

pub(crate) fn run_dataset_bundle(
    bundle_path: &Path,
    data_root: &Path,
    bars_frame_path: Option<&Path>,
) -> Result<(), String> {
    let payload = read_example_json(bundle_path, " DatasetBundleManifest ")?;
    let bundle: qx_data::DatasetBundleManifest = serde_json::from_str(&payload)
        .map_err(|error| format!("DatasetBundleManifest JSON 无效: {error}"))?;
    if let Some(frame_path) = bars_frame_path {
        let frame_payload = read_example_json(frame_path, " DatasetBundle bars BarFrame ")?;
        let frame = BarFrame::from_json(&frame_payload).map_err(|error| {
            format!(
                "DatasetBundle bars BarFrame 校验失败 {}: {error:?}",
                frame_path.display()
            )
        })?;
        let bars: Vec<Bar> = (&frame).into();
        let component = bundle
            .component("bars")
            .ok_or_else(|| "DatasetBundle 缺少 bars 组件".to_string())?;
        let provider = JsonBarFrameProvider::new(
            frame.source.0.clone(),
            component.dataset.version.clone(),
            frame_path,
        );
        let (provider_bars, manifest) = provider.load_bars_with_manifest(
            &component.dataset.dataset_id,
            &frame.instrument.to_string(),
            bars.first().map(|bar| bar.ts).unwrap_or(1),
            bars.last().map(|bar| bar.ts).unwrap_or(1),
        )?;
        // 列级核对：此前只比 `bars.len()`，于是摄取层的 OHLCV 与 BarFrame 漂开一格也不会有人出声。
        // 与 `backtests/artifacts.rs` 共用同一个 adapter（`qx_core::contract::CONTRACT_MATRIX` 的
        // `market_data_bar` 行点名这一对），两处各写一份字段映射的形态在这里结束。
        if provider_bars.len() != bars.len()
            || !provider_bars.iter().zip(&bars).all(|(ingest, market)| {
                contract_adapters::ingest_bar_matches_market_bar(ingest, market)
            })
        {
            return Err("DatasetBundle 的 bars 与 BarFrame 列式输入不一致，拒绝登记".into());
        }
        verify_dataset_bundle_manifest(&bundle, &manifest, bars.len())?;
    }
    let store = qx_data::JsonDatasetBundleStore::new(data_root.join("bundles"))?;
    let fingerprint = store.save(&bundle)?;
    let restored = store.load(&bundle.bundle_id, &bundle.version)?;
    if restored.fingerprint()? != fingerprint {
        return Err("DatasetBundleManifest 持久化后 fingerprint 不一致".into());
    }
    println!(
        "[Data · Bundle] bundle={} version={} components={} fingerprint={} root={}",
        bundle.bundle_id,
        bundle.version,
        bundle.components.len(),
        fingerprint,
        store.root().display()
    );
    Ok(())
}

pub(crate) fn verify_dataset_bundle_manifest(
    bundle: &qx_data::DatasetBundleManifest,
    bars_manifest: &qx_data::DatasetManifest,
    bars_len: usize,
) -> Result<String, String> {
    qx_app::dataset_bundle::verify_dataset_bundle_bars(bundle, bars_manifest, bars_len)
        .map_err(|error| error.message().to_string())
}

#[cfg(test)]
pub(crate) fn verify_dataset_bundle_binding(
    bundle_path: &Path,
    bars_manifest: &qx_data::DatasetManifest,
    bars_len: usize,
) -> Result<String, String> {
    let payload = read_example_json(bundle_path, "策略 dataset_bundle_path ")?;
    let bundle: qx_data::DatasetBundleManifest =
        serde_json::from_str(&payload).map_err(|error| {
            format!(
                "策略 DatasetBundleManifest JSON 无效 {}: {error}",
                bundle_path.display()
            )
        })?;
    verify_dataset_bundle_manifest(&bundle, bars_manifest, bars_len)
}

/// Arrow IPC/C Data Interface 的生产者通过一个小型、可审计的 JSON manifest
/// 把 schema、行数、时间范围和数据 fingerprint 绑定到 DatasetBundle。这里不
/// 在 CLI 内自行实现 Arrow IPC 解码；真正的 Arrow reader 仍属于数据提供方，
/// 但回测启动前会拒绝缺字段、类型不符或 kind 不匹配的 manifest。
pub(crate) fn arrow_dataset_manifest_fingerprint(
    path: &Path,
    kind: &str,
) -> Result<(String, u64), String> {
    let payload = read_example_json(path, " Arrow DatasetBundle 组件 manifest ")?;
    qx_app::dataset_bundle::arrow_dataset_manifest_fingerprint_from_json(&payload, kind, path)
        .map_err(|error| error.message().to_string())
}

#[cfg(test)]
pub(crate) fn dataset_component_file_fingerprint(
    path: &Path,
    kind: &str,
) -> Result<(String, u64), String> {
    let payload = read_example_json(path, " DatasetBundle 组件")?;
    qx_app::dataset_bundle::dataset_component_fingerprint_from_json(&payload, kind, path)
        .map_err(|error| error.message().to_string())
}
