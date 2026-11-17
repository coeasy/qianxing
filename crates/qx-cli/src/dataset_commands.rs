//! DatasetBundle 的 CLI 边界校验。
//!
//! 该模块只负责把运行时输入文件绑定到不可变 DatasetBundle；数据模型和
//! 持久化仍由 qx-data 提供，回测编排不再直接承担组件指纹细节。

use super::read_example_json;
use crate::contract_adapters;
use qx_data::{DataStorage, JsonBarFrameProvider, JsonDatasetRegistry};
use qx_datastruct::BarFrame;
use qx_guanxing::Bar;
use std::path::Path;

pub(crate) fn run_dataset_ingest(
    frame_path: &Path,
    dataset_id: &str,
    dataset_version: &str,
    data_root: &Path,
    snapshot_context_path: Option<&Path>,
    timezone: Option<&str>,
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
    let mut snapshot_identity = None;
    if let Some(context_path) = snapshot_context_path {
        let timezone = timezone.ok_or_else(|| {
            "--timezone is required when --snapshot-context is supplied".to_string()
        })?;
        if dataset_id.contains('/')
            || dataset_id.contains('\\')
            || dataset_id.contains("..")
            || dataset_version.contains('/')
            || dataset_version.contains('\\')
            || dataset_version.contains("..")
        {
            return Err("dataset id/version contains an unsafe snapshot identity".into());
        }
        let mut stored = storage
            .load_bars(dataset_id)?
            .into_iter()
            .filter(|bar| bar.instrument == frame.instrument.to_string())
            .collect::<Vec<_>>();
        stored.sort_by_key(|bar| bar.timestamp);
        let exact_count = stored
            .iter()
            .filter(|bar| bar.exact_ohlcv.is_some())
            .count();
        if exact_count != 0 && exact_count != stored.len() {
            return Err(
                "cannot publish one imported BarFrame snapshot from mixed exact/legacy rows".into(),
            );
        }
        let imported_frame = qx_datastruct::BarFrame {
            instrument: frame.instrument.clone(),
            source: qx_guanxing::DataSourceId::new(report.manifest.source.clone()),
            ts: stored.iter().map(|bar| bar.timestamp).collect(),
            open_raw: stored.iter().map(|bar| bar.open_raw).collect(),
            high_raw: stored.iter().map(|bar| bar.high_raw).collect(),
            low_raw: stored.iter().map(|bar| bar.low_raw).collect(),
            close_raw: stored.iter().map(|bar| bar.close_raw).collect(),
            volume_raw: stored.iter().map(|bar| bar.volume_raw).collect(),
            exact_ohlcv: (exact_count == stored.len() && exact_count > 0)
                .then(|| stored.iter().filter_map(|bar| bar.exact_ohlcv).collect()),
        };
        imported_frame
            .validate()
            .map_err(|error| format!("合并后的 BarFrame 无效: {error:?}"))?;
        let context_payload = read_example_json(context_path, "DatasetSnapshotContextV1 ")?;
        let context: qx_data::DatasetSnapshotContextV1 = serde_json::from_str(&context_payload)
            .map_err(|error| format!("DatasetSnapshotContextV1 JSON 无效: {error}"))?;
        let manifest = qx_data::manifest_from_bar_frame(
            &imported_frame,
            dataset_id,
            dataset_version,
            context.frequency_ms,
            timezone,
            &report.manifest.source,
            &report.manifest.source,
        )?;
        let identity = format!("{dataset_id}@{dataset_version}");
        let identity_hash = qx_data::sha256_hex_bytes(identity.as_bytes());
        let snapshot_root = data_root.join("snapshots");
        std::fs::create_dir_all(&snapshot_root)
            .map_err(|error| format!("create import snapshot directory failed: {error}"))?;
        let frame_output = snapshot_root.join(format!("{identity_hash}.frame.json"));
        let manifest_output = snapshot_root.join(format!("{identity_hash}.manifest.json"));
        let context_output = snapshot_root.join(format!("{identity_hash}.context.json"));
        let snapshot_output = snapshot_root.join(format!("{identity_hash}.snapshot.json"));
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| format!("system clock is before UNIX epoch: {error}"))?
            .as_nanos();
        let staging = snapshot_root.join(format!(
            ".staging-{identity_hash}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&staging)
            .map_err(|error| format!("create import snapshot staging directory failed: {error}"))?;
        let frame_json = imported_frame.to_json();
        let manifest_json = manifest.to_json()?;
        let publication = (|| {
            let staged_frame = staging.join("data.frame.json");
            let staged_manifest = staging.join("dataset.manifest.json");
            let staged_context = staging.join("context.json");
            let staged_snapshot = staging.join("snapshot.json");
            for (path, payload, label) in [
                (&staged_frame, frame_json.as_str(), "imported BarFrame"),
                (
                    &staged_manifest,
                    manifest_json.as_str(),
                    "DatasetManifestV2",
                ),
                (
                    &staged_context,
                    context_payload.as_str(),
                    "DatasetSnapshotContextV1",
                ),
            ] {
                qx_app::artifact_store::sink::publish(
                    path,
                    payload,
                    qx_app::artifact_store::sink::PublishMode::Idempotent,
                )
                .map_err(|error| format!("stage {label} {} failed: {error}", path.display()))?;
            }
            let publication = qx_app::publish_dataset_snapshot(
                &qx_app::DatasetSnapshotPublicationSpec::new(
                    staged_frame.to_string_lossy(),
                    staged_manifest.to_string_lossy(),
                    staged_context.to_string_lossy(),
                    staged_snapshot.to_string_lossy(),
                ),
                &qx_app::RunContext::new(
                    qx_app::CallerCapability::Research,
                    format!("dataset-ingest-{identity_hash}"),
                ),
            )
            .map_err(|error| format!("validate imported DatasetSnapshotV1 failed: {error}"))?;
            let snapshot_json = std::fs::read_to_string(&staged_snapshot).map_err(|error| {
                format!(
                    "read staged DatasetSnapshotV1 {} failed: {error}",
                    staged_snapshot.display()
                )
            })?;
            for (path, payload, label) in [
                (&frame_output, frame_json.as_str(), "imported BarFrame"),
                (
                    &manifest_output,
                    manifest_json.as_str(),
                    "DatasetManifestV2",
                ),
                (
                    &context_output,
                    context_payload.as_str(),
                    "DatasetSnapshotContextV1",
                ),
                (
                    &snapshot_output,
                    snapshot_json.as_str(),
                    "DatasetSnapshotV1",
                ),
            ] {
                qx_app::artifact_store::sink::publish(
                    path,
                    payload,
                    qx_app::artifact_store::sink::PublishMode::Idempotent,
                )
                .map_err(|error| format!("publish {label} {} failed: {error}", path.display()))?;
            }
            Ok::<_, String>(publication.snapshot_sha256)
        })();
        let _ = std::fs::remove_dir_all(&staging);
        snapshot_identity = Some((snapshot_output, publication?));
    } else if timezone.is_some() {
        return Err("--timezone requires --snapshot-context".into());
    }
    println!(
        "[Data · Ingest] dataset={} version={} source={} incoming={} stored={} fingerprint={} snapshot_sha256={} root={}",
        report.dataset_id,
        report.dataset_version,
        report.manifest.source,
        report.incoming_rows,
        report.stored_rows,
        report.manifest.fingerprint,
        snapshot_identity
            .as_ref()
            .map(|(_, digest)| digest.as_str())
            .unwrap_or("unpublished"),
        data_root.display()
    );
    if let Some((path, digest)) = snapshot_identity {
        println!("[Data · Snapshot] path={} sha256={digest}", path.display());
    }
    Ok(())
}

pub(crate) fn run_dataset_bundle(
    bundle_path: &Path,
    data_root: &Path,
    bars_frame_path: Option<&Path>,
    snapshot_path: Option<&Path>,
) -> Result<(), String> {
    let payload = read_example_json(bundle_path, " DatasetBundleManifest ")?;
    let bundle: qx_data::DatasetBundleManifest = serde_json::from_str(&payload)
        .map_err(|error| format!("DatasetBundleManifest JSON 无效: {error}"))?;
    let mut bundle = bundle;
    if let Some(path) = snapshot_path {
        let snapshot_payload = read_example_json(path, "DatasetSnapshotV1 ")?;
        let snapshot = qx_data::DatasetSnapshotV1::from_json(&snapshot_payload)
            .map_err(|error| format!("DatasetSnapshotV1 JSON 无效: {error}"))?;
        bundle.snapshots.insert("bars".into(), snapshot);
    }
    if bundle.component("bars").is_some() {
        if bars_frame_path.is_none() {
            return Err("含 bars 组件的 DatasetBundle 必须提供 BarFrame 以复核快照".into());
        }
        if !bundle.snapshots.contains_key("bars") {
            return Err("含 bars 组件的 DatasetBundle 必须绑定 DatasetSnapshotV1".into());
        }
    }
    if let Some(frame_path) = bars_frame_path {
        let frame_payload = read_example_json(frame_path, " DatasetBundle bars BarFrame ")?;
        let frame = BarFrame::from_json(&frame_payload).map_err(|error| {
            format!(
                "DatasetBundle bars BarFrame 校验失败 {}: {error:?}",
                frame_path.display()
            )
        })?;
        let bars: Vec<Bar> = frame
            .bars()
            .map_err(|error| format!("DatasetBundle bars 转换为 Bar 失败: {error:?}"))?;
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
        let snapshot = bundle
            .snapshots
            .get("bars")
            .ok_or_else(|| "DatasetBundle 缺少 bars DatasetSnapshotV1".to_string())?;
        verify_dataset_bundle_bar_snapshot(
            &bundle,
            snapshot,
            &frame,
            frame_path,
            &manifest,
            bars.len(),
        )?;
    }
    let store = qx_data::JsonDatasetBundleStore::new(data_root.join("bundles"))?;
    let fingerprint = store.save(&bundle)?;
    let restored = store.load(&bundle.bundle_id, &bundle.version)?;
    if restored.fingerprint()? != fingerprint {
        return Err("DatasetBundleManifest 持久化后 fingerprint 不一致".into());
    }
    println!(
        "[Data · Bundle] bundle={} version={} components={} fingerprint={} bars_snapshot_sha256={} root={}",
        bundle.bundle_id,
        bundle.version,
        bundle.components.len(),
        fingerprint,
        bundle
            .snapshots
            .get("bars")
            .and_then(|snapshot| snapshot.content_sha256().ok())
            .as_deref()
            .unwrap_or("unbound"),
        store.root().display()
    );
    Ok(())
}

fn verify_dataset_bundle_bar_snapshot(
    bundle: &qx_data::DatasetBundleManifest,
    snapshot: &qx_data::DatasetSnapshotV1,
    frame: &BarFrame,
    frame_path: &Path,
    bars_manifest: &qx_data::DatasetManifest,
    bars_len: usize,
) -> Result<(), String> {
    snapshot.validate()?;
    let bytes = std::fs::read(frame_path).map_err(|error| {
        format!(
            "读取 DatasetBundle BarFrame {} 失败: {error}",
            frame_path.display()
        )
    })?;
    let frequency_ms = snapshot.frequency_ms;
    if frequency_ms == 0
        || snapshot.manifest.tier != qx_data::DatasetTier::Bar
        || !matches!(
            snapshot.timestamp_semantics,
            qx_data::DatasetTimestampSemantics::BarOpen
                | qx_data::DatasetTimestampSemantics::BarClose
        )
        || snapshot.manifest.instrument != frame.instrument.to_string()
        || snapshot.manifest.dataset.dataset_id != bars_manifest.dataset_id
        || snapshot.manifest.dataset.version != bars_manifest.version
        || snapshot.manifest.dataset.source != bars_manifest.source
        || snapshot.manifest.dataset.fingerprint != bars_manifest.fingerprint
        || snapshot.manifest.dataset.schema_version != bars_manifest.schema_version
        || snapshot.manifest.dataset.start_timestamp != bars_manifest.start_timestamp
        || snapshot.manifest.dataset.end_timestamp != bars_manifest.end_timestamp
        || snapshot.manifest.quality_report.row_count != bars_len as u64
        || snapshot.manifest.content_hash != qx_data::sha256_hex_bytes(&bytes)
    {
        return Err("DatasetBundle bars snapshot does not bind to the exact BarFrame input".into());
    }
    let exact_gaps = frame
        .ts
        .windows(2)
        .filter_map(|pair| {
            (pair[1] - pair[0] > frequency_ms).then_some(qx_data::DatasetCoverageGapV1 {
                start_ts_ms: pair[0].saturating_add(frequency_ms),
                end_ts_ms: pair[1],
            })
        })
        .collect::<Vec<_>>();
    let missing_intervals = frame.ts.windows(2).try_fold(0_u64, |total, pair| {
        total.checked_add((pair[1] - pair[0]).div_ceil(frequency_ms) - 1)
    });
    if snapshot.gaps != exact_gaps
        || missing_intervals != Some(snapshot.manifest.quality_report.missing_intervals)
    {
        return Err("DatasetBundle bars snapshot gap evidence does not match the BarFrame".into());
    }
    bundle.validate()?;
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
