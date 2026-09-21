//! 跨语言策略回测链：`run_strategy_backtest`（含 L1/L2 归因装配）。

use super::*;

pub(crate) fn run_strategy_backtest(
    runtime_path: &Path,
    frame_path: &Path,
    spec_path: Option<&Path>,
) -> Result<(), String> {
    let config = read_runtime_config(runtime_path)?;
    let payload = std::fs::read_to_string(frame_path).map_err(|error| {
        format!(
            "读取策略回测 BarFrame 失败 {}: {error}",
            frame_path.display()
        )
    })?;
    let frame = BarFrame::from_json(&payload).map_err(|error| {
        format!(
            "策略回测 BarFrame 校验失败 {}: {error:?}",
            frame_path.display()
        )
    })?;
    let bars: Vec<Bar> = (&frame).into();
    if bars.len() < 2 {
        return Err("跨语言 Bar 回测至少需要两根 Bar".into());
    }
    let provider =
        JsonBarFrameProvider::new(frame.source.0.clone(), "barframe-json-v1", frame_path);
    let (provider_bars, manifest) = provider.load_bars_with_manifest(
        &format!("strategy-bars:{}", frame.instrument),
        &frame.instrument.to_string(),
        bars.first().map(|bar| bar.ts).unwrap_or(1),
        bars.last().map(|bar| bar.ts).unwrap_or(1),
    )?;
    if provider_bars.len() != bars.len()
        || provider_bars.iter().zip(&bars).any(|(left, right)| {
            left.timestamp != right.ts
                || left.open_raw != right.open
                || left.high_raw != right.high
                || left.low_raw != right.low
                || left.close_raw != right.close
                || left.volume_raw != right.volume
        })
    {
        return Err("qx-data Provider 与 BarFrame 列式输入不一致，拒绝开始回测".into());
    }
    let data_root = resolve_runtime_relative_path(runtime_path, &config.storage.data_dir);
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
        resolve_strategy_runtime_paths(&mut strategy_config.strategy, runtime_path);
        let (bundle_fingerprint, bundle_components) =
            if let Some(bundle_path) = strategy_config.strategy.dataset_bundle_path.as_deref() {
                let bundle_payload = std::fs::read_to_string(bundle_path).map_err(|error| {
                    format!(
                        "读取策略 DatasetBundleManifest 组件失败 {}: {error}",
                        bundle_path
                    )
                })?;
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
            let bundle_payload = std::fs::read_to_string(bundle_path).map_err(|error| {
                format!(
                    "读取策略 DatasetBundleManifest 组件失败 {}: {error}",
                    bundle_path
                )
            })?;
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
        run_single_strategy_backtest(
            &strategy_config,
            Some(runtime_path),
            &frame,
            &bars,
            spec_path,
            &strategy_id,
            bundle_fingerprint
                .as_deref()
                .zip(bundle_components.as_ref())
                .map(
                    |(bundle_fingerprint, component_fingerprints)| DatasetRunBinding {
                        bundle_fingerprint,
                        component_fingerprints,
                    },
                ),
            &data_root,
        )?;
    }
    Ok(())
}

pub(crate) fn persist_backtest_run_manifest(
    root: &Path,
    manifest: &RunManifest,
) -> Result<PathBuf, String> {
    let runs_root = root.join("runs");
    std::fs::create_dir_all(&runs_root)
        .map_err(|error| format!("创建回测 RunManifest 目录失败: {error}"))?;
    let safe_run_id = manifest
        .run_id
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.') {
                value
            } else {
                '_'
            }
        })
        .collect::<String>();
    let path = runs_root.join(format!("{safe_run_id}-{:016x}.run.json", manifest.digest()));
    let payload = manifest.to_json()?;
    if path.exists() {
        let existing = std::fs::read_to_string(&path).map_err(|error| {
            format!("读取已有回测 RunManifest 失败 {}: {error}", path.display())
        })?;
        let restored = RunManifest::from_json(&existing)?;
        if restored != *manifest {
            return Err(format!(
                "同一回测 RunManifest 路径已存在不同内容: {}",
                path.display()
            ));
        }
        return Ok(path);
    }
    let temporary = path.with_extension(format!("run.json.tmp.{}", std::process::id()));
    std::fs::write(&temporary, payload)
        .map_err(|error| format!("写入回测 RunManifest 失败 {}: {error}", temporary.display()))?;
    if let Err(error) = std::fs::rename(&temporary, &path) {
        let _ = std::fs::remove_file(&temporary);
        if !path.exists() {
            return Err(format!(
                "提交回测 RunManifest 失败 {}: {error}",
                path.display()
            ));
        }
    }
    Ok(path)
}

pub(crate) fn write_backtest_artifact(
    path: &Path,
    payload: &str,
    label: &str,
) -> Result<(), String> {
    if path.exists() {
        let existing = std::fs::read_to_string(path)
            .map_err(|error| format!("读取已有{label}失败 {}: {error}", path.display()))?;
        if existing == payload {
            return Ok(());
        }
        return Err(format!(
            "同一回测{label}路径已存在不同内容: {}",
            path.display()
        ));
    }
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&temporary, payload)
        .map_err(|error| format!("写入{label}失败 {}: {error}", temporary.display()))?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        if path.exists() {
            let existing = std::fs::read_to_string(path).map_err(|read_error| {
                format!("读取并发生成的{label}失败 {}: {read_error}", path.display())
            })?;
            let _ = std::fs::remove_file(&temporary);
            if existing == payload {
                return Ok(());
            }
        }
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("提交{label}失败 {}: {error}", path.display()));
    }
    Ok(())
}
