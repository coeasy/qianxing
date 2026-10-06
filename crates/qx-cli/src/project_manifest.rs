//! `init` 产出的项目身份：运行时、实际数据集和可识别策略引用到一份 ProjectManifest。

use super::*;
use qx_spec::{DatasetRef, FoundationDocument, ProjectManifest, StrategyRef};
use std::path::{Path, PathBuf};

fn manifest_profile(profile: &str) -> &'static str {
    match profile {
        "ashare" => "ashare-research",
        "multi-venue" => "multi-venue-arb",
        "base" | "builtin" | "paper" | "ccxt" | "backtest" => "crypto-paper",
        _ => "crypto-paper",
    }
}

fn project_path(root: &Path, configured: &str) -> PathBuf {
    let path = Path::new(configured);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// 只登记运行时实际引用、且能从磁盘清单读到的数据身份；不制造占位 dataset id。
fn declared_datasets(root: &Path, config: &RuntimeConfig) -> Result<Vec<DatasetRef>, String> {
    if let Some(bundle_path) = config.strategy.dataset_bundle_path.as_deref() {
        let path = project_path(root, bundle_path);
        let payload = std::fs::read_to_string(&path).map_err(|error| {
            format!(
                "项目清单引用的 DatasetBundle 读不到 {}: {error}",
                path.display()
            )
        })?;
        let bundle: qx_data::DatasetBundleManifest = serde_json::from_str(&payload)
            .map_err(|error| format!("项目清单 DatasetBundle 无效 {}: {error}", path.display()))?;
        bundle.validate()?;
        return Ok(bundle
            .components
            .values()
            .map(|component| DatasetRef {
                dataset_id: component.dataset.dataset_id.clone(),
                version: component.dataset.version.clone(),
            })
            .collect());
    }
    let Some(bars_path) = config.strategy.bars_snapshot_path.as_deref() else {
        return Ok(Vec::new());
    };
    let path = project_path(root, bars_path);
    if !path.is_file() {
        // 有些 live/profile 模板预期运行时生成实时 BarFrame；这不是一个可声明的离线数据集。
        return Ok(Vec::new());
    }
    let frame = read_bar_frame_for_backtest(&path)?;
    let (_, manifest) = barframe_dataset_identity(&path, &frame)?;
    Ok(vec![DatasetRef {
        dataset_id: manifest.dataset_id,
        version: manifest.version,
    }])
}

fn strategy_ref(config: &RuntimeConfig) -> Option<StrategyRef> {
    let strategy = &config.strategy;
    let strategy_id = strategy
        .id
        .clone()
        .unwrap_or_else(|| strategy.version.clone());
    if let Some(builtin) = strategy.builtin_strategy.as_deref() {
        return Some(StrategyRef {
            strategy_id,
            language: "rust".into(),
            source: builtin.into(),
        });
    }
    if let Some(module) = strategy.python_module.as_deref() {
        return Some(StrategyRef {
            strategy_id,
            language: "python".into(),
            source: module.into(),
        });
    }
    if let Some(library) = strategy.c_abi_library.as_deref() {
        return Some(StrategyRef {
            strategy_id,
            language: "cpp".into(),
            source: library.into(),
        });
    }
    None
}

/// 给可复核的本地项目写一份严格校验的 `qianxing.project.json`。
/// 没有可读数据集身份时返回 `None`，避免把实时占位配置误记成已绑定数据。
pub(crate) fn write_init_project_manifest(
    runtime_path: &Path,
    root: &Path,
    profile: &str,
    config: &RuntimeConfig,
    force: bool,
) -> Result<Option<(PathBuf, String)>, String> {
    let datasets = declared_datasets(root, config)?;
    if datasets.is_empty() {
        return Ok(None);
    }
    let path = root.join("qianxing.project.json");
    if path.exists() && !force {
        return Err(format!(
            "项目清单已存在: {}；如确认覆盖，请显式添加 --force",
            path.display()
        ));
    }
    let project = ProjectManifest {
        schema_version: qx_spec::PROJECT_MANIFEST_SCHEMA_VERSION,
        project_id: format!(
            "qx-{}",
            config.fingerprint()?.chars().take(16).collect::<String>()
        ),
        profile: manifest_profile(profile).into(),
        runtime: runtime_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "运行时配置文件名无效，无法生成 ProjectManifest".to_string())?
            .into(),
        artifact_root: config.storage.data_dir.clone(),
        datasets,
        strategies: strategy_ref(config).into_iter().collect(),
    };
    let payload = project.to_json()?;
    let readout = qx_spec::describe("project", &payload)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建项目清单目录失败 {}: {error}", parent.display()))?;
    }
    std::fs::write(&path, payload)
        .map_err(|error| format!("写入项目清单失败 {}: {error}", path.display()))?;
    Ok(Some((path, readout.fingerprint)))
}
