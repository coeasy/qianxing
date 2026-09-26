//! 运行时引用校验的组件细则：A 股动作/日历 JSON 与数据集 bundle 组件的一致性核对。
//!
//! 两个入口都只往 `failures` 追加结论，不改配置也不写盘。从 `runtime_check.rs` 拆出来是
//! 纯搬家：兄弟模块门槛把体检主链压在 500 行以内，而它已经贴在上限了（V11 L4）。

use super::*;

pub(crate) fn validate_ashare_component_json(
    runtime_path: &Path,
    failures: &mut Vec<String>,
    label: String,
    configured: &str,
    kind: &str,
    instrument: Option<&str>,
) {
    let resolved = resolve_runtime_relative_path(runtime_path, configured);
    if !resolved.is_file() {
        failures.push(format!(
            "{label} 文件不存在: {} (configured={configured})",
            resolved.display()
        ));
        return;
    }
    let payload = match std::fs::read_to_string(&resolved) {
        Ok(payload) => payload,
        Err(error) => {
            failures.push(format!(
                "{label} 文件不可读 {}: {error}",
                resolved.display()
            ));
            return;
        }
    };
    let validation = if kind == "calendar" {
        let mut rules = AshareRuleConfig::default();
        rules.apply_calendar_json(&payload).map(|_| ())
    } else if let Some(instrument) = instrument {
        let mut rules = AshareRuleConfig::default();
        rules
            .apply_corporate_actions_json(instrument, &payload)
            .map(|_| ())
    } else {
        serde_json::from_str::<serde_json::Value>(&payload)
            .map_err(|error| format!("JSON 无效: {error}"))
            .and_then(|document| {
                let is_array = document.is_array();
                let is_wrapped = document
                    .get("actions")
                    .and_then(serde_json::Value::as_array)
                    .is_some();
                if is_array || is_wrapped {
                    Ok(())
                } else {
                    Err("必须是数组或包含 actions 数组的对象".into())
                }
            })
    };
    match validation {
        Ok(()) => {}
        Err(error) => failures.push(format!("{label} 内容非法: {error}")),
    }
}

pub(crate) fn validate_dataset_bundle_component_references(
    runtime_path: &Path,
    failures: &mut Vec<String>,
    label: &str,
    configured_bundle: &str,
    strategy: &StrategyRuntimeConfig,
) {
    let bundle_path = resolve_runtime_relative_path(runtime_path, configured_bundle);
    let payload = match std::fs::read_to_string(&bundle_path) {
        Ok(payload) => payload,
        Err(_) => return,
    };
    let bundle: qx_data::DatasetBundleManifest = match serde_json::from_str(&payload) {
        Ok(bundle) => bundle,
        Err(error) => {
            failures.push(format!("{label}.dataset_bundle_path JSON 无效: {error}"));
            return;
        }
    };
    if let Err(error) = bundle.validate() {
        failures.push(format!("{label}.dataset_bundle_path 校验失败: {error}"));
        return;
    }
    for kind in bundle
        .components
        .keys()
        .filter(|kind| kind.as_str() != "bars")
    {
        let configured = strategy
            .dataset_component_paths
            .get(kind)
            .map(String::as_str)
            .or(match kind.as_str() {
                "corporate_actions" => strategy.ashare_actions_path.as_deref(),
                "calendar" => strategy.ashare_calendar_path.as_deref(),
                _ => None,
            });
        let Some(configured) = configured else {
            failures.push(format!(
                "{label}.dataset_bundle_path 组件 {kind} 没有绑定输入文件"
            ));
            continue;
        };
        let path = resolve_runtime_relative_path(runtime_path, configured);
        if !path.is_file() {
            failures.push(format!(
                "{label}.dataset_component_paths.{kind} 文件不存在: {}",
                path.display()
            ));
        } else if matches!(
            bundle
                .components
                .get(kind)
                .map(|component| &component.format),
            Some(qx_data::DatasetComponentFormat::Arrow)
        ) {
            match dataset_commands::arrow_dataset_manifest_fingerprint(&path, kind) {
                Ok((fingerprint, row_count)) => {
                    let component = bundle
                        .components
                        .get(kind)
                        .expect("bundle component exists");
                    if fingerprint != component.dataset.fingerprint {
                        failures.push(format!(
                            "{label}.dataset_component_paths.{kind} Arrow fingerprint 不匹配: bundle={} input={fingerprint}",
                            component.dataset.fingerprint
                        ));
                    }
                    if row_count != component.row_count {
                        failures.push(format!(
                            "{label}.dataset_component_paths.{kind} Arrow 行数不匹配: bundle={} input={row_count}",
                            component.row_count
                        ));
                    }
                }
                Err(error) => failures.push(format!(
                    "{label}.dataset_component_paths.{kind} Arrow manifest 校验失败: {error}"
                )),
            }
        }
    }
}
