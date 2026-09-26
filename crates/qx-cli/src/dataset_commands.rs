//! DatasetBundle 的 CLI 边界校验，以及数据集登记记录在运行前的读侧。
//!
//! 该模块把运行时输入文件绑定到不可变 DatasetBundle，并按 Bundle 声明的
//! `(dataset_id, version)` 回读 `datasets.manifest.json`；数据模型和持久化仍由
//! qx-data 提供，回测编排不再直接承担组件指纹细节。

use qx_data::{DatasetRef, DatasetResolver, JsonBarFrameProvider, JsonDatasetRegistry};
use qx_datastruct::BarFrame;
use qx_guanxing::Bar;
use qx_runtime::StrategyRuntimeConfig;
use std::path::Path;

/// Bundle 声明的每一档 `(dataset_id, version)` 都回数据集注册表解析一次（V11 F2）。
///
/// `dataset-ingest` 写的就是那张表，而它此前一个读者都没有：登记记录既拦不住后来被改过的
/// 输入文件，也不参与 Bundle 的核对，同一个数据集身份于是可以由 ingest 与 Bundle 两个工具
/// 各写一遍而互不知情。现在登记过的那一份必须与声明的指纹相等；没登记过要说成"未核对"
/// 并把组件名念出来——把"没人读过这条记录"混进"已核对"是更坏的结果。
pub(crate) fn verify_dataset_registry_declarations(
    registry: &JsonDatasetRegistry,
    bundle: &qx_data::DatasetBundleManifest,
) -> Result<(usize, Vec<String>), String> {
    let mut checked = 0;
    let mut unrecorded = Vec::new();
    for (kind, component) in &bundle.components {
        let dataset = &component.dataset;
        let reference = DatasetRef::new(
            dataset.dataset_id.clone(),
            dataset.version.clone(),
            dataset.fingerprint.clone(),
        )
        .map_err(|error| format!("Bundle 组件 {kind} 的数据集声明非法: {error}"))?;
        if registry
            .resolve(&dataset.dataset_id, &dataset.version)
            .is_none()
        {
            unrecorded.push(format!("{kind}:{}@{}", dataset.dataset_id, dataset.version));
            continue;
        }
        // 上一行确认了记录在场，所以这里只剩"指纹相符"与"指纹不符"两种结果。
        DatasetResolver::resolve(registry, &reference)
            .map_err(|error| format!("Bundle 组件 {kind} 与数据集登记记录不符: {error}"))?;
        checked += 1;
    }
    Ok((checked, unrecorded))
}

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
    let registry = JsonDatasetRegistry::open(data_root.join("datasets.manifest.json"))?;
    let (checked, unrecorded) = verify_dataset_registry_declarations(&registry, &bundle)?;
    let store = qx_data::JsonDatasetBundleStore::new(data_root.join("bundles"))?;
    let fingerprint = store.save(&bundle)?;
    let restored = store.load(&bundle.bundle_id, &bundle.version)?;
    if restored.fingerprint()? != fingerprint {
        return Err("DatasetBundleManifest 持久化后 fingerprint 不一致".into());
    }
    println!(
        "[Data · Bundle] bundle={} version={} components={} fingerprint={} registry_checked={}/{} root={}",
        bundle.bundle_id,
        bundle.version,
        bundle.components.len(),
        fingerprint,
        checked,
        bundle.components.len(),
        store.root().display()
    );
    if !unrecorded.is_empty() {
        println!(
            "[Data · Bundle] 这些组件在 datasets.manifest.json 里没有登记记录，只按输入文件核对了内容: {}",
            unrecorded.join(", ")
        );
    }
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
            let sessions: &[serde_json::Value] = match value.get("sessions") {
                // 旧版日历（顶层没有 `schema_version`）可以不写 sessions：Python 的
                // `AshareTradingCalendar.from_json` 与本 crate 的 `apply_calendar_json` 都把它
                // 读成"没有时段"。指纹侧此前直接报错，于是 Python 登记好的 bundle 会在回测
                // 启动前被判成组件非法而拒启，两份读法对同一份文档一宽一严（V11 R17）。
                None => &[],
                Some(value) => value
                    .as_array()
                    .ok_or_else(|| "calendar 组件的 sessions 必须是数组".to_string())?,
            };
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
