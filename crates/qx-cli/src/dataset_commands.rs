//! DatasetBundle 的 CLI 边界校验。
//!
//! 该模块只负责把运行时输入文件绑定到不可变 DatasetBundle；数据模型和
//! 持久化仍由 qx-data 提供，回测编排不再直接承担组件指纹细节。

use qx_data::{JsonBarFrameProvider, JsonDatasetRegistry};
use qx_datastruct::BarFrame;
use qx_guanxing::Bar;
use qx_runtime::StrategyRuntimeConfig;
use std::path::{Path, PathBuf};

pub(crate) fn run_dataset_ingest(
    frame_path: &Path,
    dataset_id: &str,
    dataset_version: &str,
    data_root: &Path,
) -> Result<(), String> {
    let payload = std::fs::read_to_string(frame_path)
        .map_err(|error| format!("读取数据集 BarFrame 失败 {}: {error}", frame_path.display()))?;
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
    let payload = std::fs::read_to_string(bundle_path).map_err(|error| {
        format!(
            "读取 DatasetBundleManifest 失败 {}: {error}",
            bundle_path.display()
        )
    })?;
    let bundle: qx_data::DatasetBundleManifest = serde_json::from_str(&payload)
        .map_err(|error| format!("DatasetBundleManifest JSON 无效: {error}"))?;
    if let Some(frame_path) = bars_frame_path {
        let frame_payload = std::fs::read_to_string(frame_path).map_err(|error| {
            format!(
                "读取 DatasetBundle bars BarFrame 失败 {}: {error}",
                frame_path.display()
            )
        })?;
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
        let (_, manifest) = provider.load_bars_with_manifest(
            &component.dataset.dataset_id,
            &frame.instrument.to_string(),
            bars.first().map(|bar| bar.ts).unwrap_or(1),
            bars.last().map(|bar| bar.ts).unwrap_or(1),
        )?;
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
    bundle
        .validate()
        .map_err(|error| format!("策略 DatasetBundleManifest 校验失败: {error}"))?;
    let bars = bundle
        .component("bars")
        .ok_or_else(|| "策略 DatasetBundleManifest 缺少 bars 组件".to_string())?;
    if bars.dataset.fingerprint != bars_manifest.fingerprint {
        return Err(format!(
            "策略 DatasetBundleManifest bars fingerprint 不匹配: bundle={} input={}",
            bars.dataset.fingerprint, bars_manifest.fingerprint
        ));
    }
    if bars.row_count != bars_len as u64 {
        return Err(format!(
            "策略 DatasetBundleManifest bars row_count 不匹配: bundle={} input={bars_len}",
            bars.row_count
        ));
    }
    bundle.fingerprint()
}

pub(crate) fn verify_dataset_bundle_binding(
    bundle_path: &Path,
    bars_manifest: &qx_data::DatasetManifest,
    bars_len: usize,
) -> Result<String, String> {
    let payload = std::fs::read_to_string(bundle_path).map_err(|error| {
        format!(
            "读取策略 dataset_bundle_path 失败 {}: {error}",
            bundle_path.display()
        )
    })?;
    let bundle: qx_data::DatasetBundleManifest =
        serde_json::from_str(&payload).map_err(|error| {
            format!(
                "策略 DatasetBundleManifest JSON 无效 {}: {error}",
                bundle_path.display()
            )
        })?;
    verify_dataset_bundle_manifest(&bundle, bars_manifest, bars_len)
}

pub(crate) fn verify_dataset_bundle_component_bindings(
    bundle: &qx_data::DatasetBundleManifest,
    strategy: &StrategyRuntimeConfig,
    strategy_id: &str,
) -> Result<(), String> {
    for (kind, component) in &bundle.components {
        if kind == "bars" {
            continue;
        }
        let path = strategy
            .dataset_component_paths
            .get(kind)
            .map(String::as_str)
            .or(match kind.as_str() {
                "corporate_actions" => strategy.ashare_actions_path.as_deref(),
                "calendar" => strategy.ashare_calendar_path.as_deref(),
                _ => None,
            })
            .ok_or_else(|| {
                format!(
                    "策略 {strategy_id} 的 DatasetBundle 组件 {kind} 没有对应输入文件；请配置 dataset_component_paths.{kind}"
                )
            })?;
        let (fingerprint, row_count) = match component.format {
            qx_data::DatasetComponentFormat::Json => {
                dataset_component_file_fingerprint(Path::new(path), kind)?
            }
            qx_data::DatasetComponentFormat::Arrow => {
                arrow_dataset_manifest_fingerprint(Path::new(path), kind)?
            }
        };
        if fingerprint != component.dataset.fingerprint {
            return Err(format!(
                "策略 {strategy_id} 的 DatasetBundle 组件 {kind} 指纹不匹配: bundle={} input={fingerprint}",
                component.dataset.fingerprint
            ));
        }
        if row_count != component.row_count {
            return Err(format!(
                "策略 {strategy_id} 的 DatasetBundle 组件 {kind} 行数不匹配: bundle={} input={row_count}",
                component.row_count
            ));
        }
    }
    Ok(())
}

/// Arrow IPC/C Data Interface 的生产者通过一个小型、可审计的 JSON manifest
/// 把 schema、行数、时间范围和数据 fingerprint 绑定到 DatasetBundle。这里不
/// 在 CLI 内自行实现 Arrow IPC 解码；真正的 Arrow reader 仍属于数据提供方，
/// 但回测启动前会拒绝缺字段、类型不符或 kind 不匹配的 manifest。
pub(crate) fn arrow_dataset_manifest_fingerprint(
    path: &Path,
    kind: &str,
) -> Result<(String, u64), String> {
    let payload = std::fs::read_to_string(path).map_err(|error| {
        format!(
            "读取 Arrow DatasetBundle 组件 manifest 失败 {}: {error}",
            path.display()
        )
    })?;
    let manifest = qx_data::ArrowDatasetManifest::from_json(&payload).map_err(|error| {
        format!(
            "Arrow DatasetBundle 组件 manifest 非法 {}: {error}",
            path.display()
        )
    })?;
    if manifest.kind != kind {
        return Err(format!(
            "Arrow DatasetBundle 组件 kind 不匹配: expected={kind} actual={}",
            manifest.kind
        ));
    }
    Ok((manifest.dataset.fingerprint, manifest.row_count))
}

pub(crate) fn dataset_component_file_fingerprint(
    path: &Path,
    kind: &str,
) -> Result<(String, u64), String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取 DatasetBundle 组件失败 {}: {error}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| format!("DatasetBundle 组件 JSON 无效 {}: {error}", path.display()))?;
    match kind {
        "corporate_actions" => {
            let actions = value
                .as_array()
                .or_else(|| value.get("actions").and_then(serde_json::Value::as_array))
                .ok_or_else(|| "corporate_actions 组件必须是数组或包含 actions 数组".to_string())?;
            let canonical = serde_json::to_vec(actions)
                .map_err(|error| format!("规范化 corporate_actions 组件失败: {error}"))?;
            Ok((qx_strategy::sha256_hex(&canonical), actions.len() as u64))
        }
        "calendar" => {
            let calendar_id = value
                .get("calendar_id")
                .ok_or_else(|| "calendar 组件缺少 calendar_id".to_string())?;
            let trading_days = value
                .get("trading_days")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| "calendar 组件缺少 trading_days 数组".to_string())?;
            let sessions = value
                .get("sessions")
                .ok_or_else(|| "calendar 组件缺少 sessions".to_string())?;
            // 与 Python AshareTradingCalendar.to_json 的 dataclass 字段顺序保持一致。
            let canonical = format!(
                "{{\"calendar_id\":{},\"trading_days\":{},\"sessions\":{}}}",
                serde_json::to_string(calendar_id).map_err(|error| error.to_string())?,
                serde_json::to_string(trading_days).map_err(|error| error.to_string())?,
                serde_json::to_string(sessions).map_err(|error| error.to_string())?
            )
            .into_bytes();
            Ok((
                qx_strategy::sha256_hex(&canonical),
                trading_days.len() as u64,
            ))
        }
        _ => {
            let rows = value
                .as_array()
                .or_else(|| value.get("rows").and_then(serde_json::Value::as_array))
                .or_else(|| value.get("data").and_then(serde_json::Value::as_array))
                .ok_or_else(|| {
                    format!("DatasetBundle 组件 {kind} 必须是数组，或包含 rows/data 数组")
                })?;
            let canonical = canonicalize_json(&value);
            let bytes = serde_json::to_vec(&canonical)
                .map_err(|error| format!("规范化 DatasetBundle 组件 {kind} 失败: {error}"))?;
            Ok((qx_strategy::sha256_hex(&bytes), rows.len() as u64))
        }
    }
}

/// 将通用组件 JSON 递归规范化，确保字段顺序差异不会改变 Bundle 指纹。
/// 数组顺序仍然保留，因为停牌、限价规则和因子快照通常具有时间/优先级语义。
fn canonicalize_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => {
            let mut sorted = serde_json::Map::new();
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            for (key, value) in entries {
                sorted.insert(key.clone(), canonicalize_json(value));
            }
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonicalize_json).collect())
        }
        _ => value.clone(),
    }
}

pub(crate) fn dataset_ingest_command(argv: &[String]) {
    let frame = match argv.get(2).cloned() {
        Some(value) => PathBuf::from(value),
        None => {
            eprintln!("dataset-ingest 需要 bar-frame.json dataset-id version data-dir");
            std::process::exit(2);
        }
    };
    let dataset_id = match argv.get(3).cloned() {
        Some(value) => value,
        None => {
            eprintln!("dataset-ingest 缺少 dataset-id");
            std::process::exit(2);
        }
    };
    let version = match argv.get(4).cloned() {
        Some(value) => value,
        None => {
            eprintln!("dataset-ingest 缺少 version");
            std::process::exit(2);
        }
    };
    let data_root = match argv.get(5).cloned() {
        Some(value) => PathBuf::from(value),
        None => {
            eprintln!("dataset-ingest 缺少 data-dir");
            std::process::exit(2);
        }
    };
    if let Err(error) = run_dataset_ingest(&frame, &dataset_id, &version, &data_root) {
        eprintln!("数据集摄取失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn dataset_bundle_command(argv: &[String]) {
    let bundle = match argv.get(2).cloned() {
        Some(value) => PathBuf::from(value),
        None => {
            eprintln!("dataset-bundle 需要 bundle.json data-dir");
            std::process::exit(2);
        }
    };
    let data_root = match argv.get(3).cloned() {
        Some(value) => PathBuf::from(value),
        None => {
            eprintln!("dataset-bundle 缺少 data-dir");
            std::process::exit(2);
        }
    };
    let bars_frame = argv.get(4).cloned().map(PathBuf::from);
    if let Err(error) = run_dataset_bundle(&bundle, &data_root, bars_frame.as_deref()) {
        eprintln!("数据集 Bundle 保存失败: {error}");
        std::process::exit(2);
    }
}
