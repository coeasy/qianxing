//! Runtime-config-relative filesystem paths shared by all service and facade hosts.

use super::StrategyRuntimeConfig;
use std::path::{Path, PathBuf};

/// Whether a configured path is absolute on either the current or Windows host platform.
pub fn is_explicit_absolute_path(configured: &str) -> bool {
    let bytes = configured.as_bytes();
    Path::new(configured).is_absolute()
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
        || configured.starts_with("\\\\")
}

/// Resolve a runtime-config path relative to the config file's directory.
///
/// A leading component equal to the config directory name is stripped once, supporting both
/// `file.json` and `deploy/file.json` spellings for a runtime file inside `deploy/`.
pub fn resolve_runtime_relative_path(runtime_path: &Path, configured: &str) -> PathBuf {
    if is_explicit_absolute_path(configured) {
        PathBuf::from(configured)
    } else {
        let parent = runtime_path.parent().unwrap_or_else(|| Path::new("."));
        let configured_path = Path::new(configured);
        let mut components = configured_path.components();
        let first = components.next();
        let parent_name = parent.file_name().and_then(|value| value.to_str());
        if let (Some(std::path::Component::Normal(first)), Some(parent_name)) = (first, parent_name)
        {
            if first == std::ffi::OsStr::new(parent_name) {
                return parent.join(components.as_path());
            }
        }
        parent.join(configured_path)
    }
}

/// Resolve every filesystem path declared by one strategy against its runtime-config file.
/// Import-like module names and bare executable commands remain names, not filesystem paths.
pub fn resolve_strategy_runtime_paths(strategy: &mut StrategyRuntimeConfig, runtime_path: &Path) {
    fn resolve_slot(slot: &mut Option<String>, runtime_path: &Path) {
        if let Some(configured) = slot.as_deref() {
            *slot = Some(
                resolve_runtime_relative_path(runtime_path, configured)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }

    resolve_slot(&mut strategy.target_snapshot_path, runtime_path);
    resolve_slot(&mut strategy.research_snapshot_path, runtime_path);
    resolve_slot(&mut strategy.dataset_bundle_path, runtime_path);
    resolve_slot(&mut strategy.bars_snapshot_path, runtime_path);
    resolve_slot(&mut strategy.ashare_rules_path, runtime_path);
    resolve_slot(&mut strategy.ashare_actions_path, runtime_path);
    resolve_slot(&mut strategy.ashare_calendar_path, runtime_path);
    resolve_slot(
        &mut strategy.builtin_reference_bars_snapshot_path,
        runtime_path,
    );
    for configured in strategy.dataset_component_paths.values_mut() {
        *configured = resolve_runtime_relative_path(runtime_path, configured)
            .to_string_lossy()
            .into_owned();
    }

    if let Some(executable) = strategy.external_executable.as_deref() {
        let path_like = executable.contains('/')
            || executable.contains('\\')
            || executable.starts_with('.')
            || Path::new(executable).is_absolute();
        if path_like {
            resolve_slot(&mut strategy.external_executable, runtime_path);
        }
    }
    if let Some(module) = strategy.python_module.as_deref() {
        let path_like = module.ends_with(".py") || module.contains('/') || module.contains('\\');
        if path_like {
            resolve_slot(&mut strategy.python_module, runtime_path);
        }
    }
    if let Some(library) = strategy.c_abi_library.as_deref() {
        let path_like = library.contains('/')
            || library.contains('\\')
            || library.starts_with('.')
            || Path::new(library).is_absolute();
        if path_like {
            resolve_slot(&mut strategy.c_abi_library, runtime_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_explicit_absolute_path, resolve_runtime_relative_path, resolve_strategy_runtime_paths,
    };
    use crate::StrategyRuntimeConfig;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    #[test]
    fn runtime_relative_paths_keep_config_directory_and_explicit_absolute_semantics() {
        assert_eq!(
            resolve_runtime_relative_path(
                Path::new("deploy/qianxing.runtime.production.example.json"),
                "qianxing.binance.spot.spec.json",
            ),
            PathBuf::from("deploy/qianxing.binance.spot.spec.json")
        );
        assert_eq!(
            resolve_runtime_relative_path(
                Path::new("deploy/qianxing.runtime.ccxt.example.json"),
                "deploy/qianxing.ccxt.okx.perpetual.spec.json",
            ),
            PathBuf::from("deploy/qianxing.ccxt.okx.perpetual.spec.json")
        );
        assert_eq!(
            resolve_runtime_relative_path(
                Path::new("deploy/runtime.json"),
                "C:/absolute/spec.json",
            ),
            PathBuf::from("C:/absolute/spec.json")
        );
        assert!(is_explicit_absolute_path("C:/absolute/spec.json"));
        assert!(is_explicit_absolute_path("\\\\server\\share\\runtime.json"));
    }

    #[test]
    fn strategy_runtime_paths_resolve_all_declared_files_but_preserve_import_names() {
        let runtime_path = Path::new("deploy/runtime.json");
        let mut strategy = StrategyRuntimeConfig {
            target_snapshot_path: Some("../research/target.json".into()),
            research_snapshot_path: Some("../research/bundle.json".into()),
            dataset_bundle_path: Some("../research/dataset.bundle.json".into()),
            dataset_component_paths: BTreeMap::from([(
                "corporate_actions".into(),
                "../research/actions.json".into(),
            )]),
            bars_snapshot_path: Some("../research/bars.json".into()),
            ashare_rules_path: Some("../research/rules.json".into()),
            ashare_actions_path: Some("../research/actions.json".into()),
            ashare_calendar_path: Some("../research/calendar.json".into()),
            builtin_reference_bars_snapshot_path: Some("../research/reference.json".into()),
            external_executable: Some("../bin/strategy.exe".into()),
            python_module: Some("../python/strategy.py".into()),
            c_abi_library: Some("../lib/strategy.dll".into()),
            ..StrategyRuntimeConfig::default()
        };

        resolve_strategy_runtime_paths(&mut strategy, runtime_path);

        let resolved = [
            strategy.target_snapshot_path.as_deref().unwrap(),
            strategy.research_snapshot_path.as_deref().unwrap(),
            strategy.dataset_bundle_path.as_deref().unwrap(),
            strategy.dataset_component_paths["corporate_actions"].as_str(),
            strategy.bars_snapshot_path.as_deref().unwrap(),
            strategy.ashare_rules_path.as_deref().unwrap(),
            strategy.ashare_actions_path.as_deref().unwrap(),
            strategy.ashare_calendar_path.as_deref().unwrap(),
            strategy
                .builtin_reference_bars_snapshot_path
                .as_deref()
                .unwrap(),
            strategy.external_executable.as_deref().unwrap(),
            strategy.python_module.as_deref().unwrap(),
            strategy.c_abi_library.as_deref().unwrap(),
        ];
        assert!(resolved.iter().all(|path| {
            *path
                == resolve_runtime_relative_path(runtime_path, path)
                    .to_string_lossy()
                    .as_ref()
        }));

        let mut named = StrategyRuntimeConfig {
            external_executable: Some("strategy-worker".into()),
            python_module: Some("package.strategy".into()),
            c_abi_library: Some("strategy_plugin".into()),
            ..StrategyRuntimeConfig::default()
        };
        resolve_strategy_runtime_paths(&mut named, runtime_path);
        assert_eq!(
            named.external_executable.as_deref(),
            Some("strategy-worker")
        );
        assert_eq!(named.python_module.as_deref(), Some("package.strategy"));
        assert_eq!(named.c_abi_library.as_deref(), Some("strategy_plugin"));
    }
}
