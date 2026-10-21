use crate::{
    cost_rules_problem, dataset_commands, resolve_runtime_relative_path,
    validate_ccxt_worker_binding, worker_market_spec_failures,
};
use qx_app::fill_model_failure;
use qx_runtime::{RuntimeConfig, StrategyRuntimeConfig, WorkerRole};
use qx_xingban::AshareRuleConfig;
use std::path::Path;

pub(crate) fn validate_runtime_references(
    runtime_path: &Path,
    config: &RuntimeConfig,
) -> (Vec<String>, Vec<String>) {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();

    // 浏览器准入这两格的口径住在 `qx-api`，`qx-runtime` 不许反向依赖它，所以由同样看得见
    // 两侧的这里调那一份实现：坏源与 0 上限此前要等到 `serve` 起不来才现身。
    if let Err(problem) = qx_api::validate_admission_config(
        &config.api.cors_allowed_origins,
        config.api.max_concurrent_connections,
    ) {
        failures.push(format!("api {problem}"));
    }

    let require_file = |failures: &mut Vec<String>, label: String, configured: &str| {
        let resolved = resolve_runtime_relative_path(runtime_path, configured);
        if !resolved.is_file() {
            failures.push(format!(
                "{label} 文件不存在: {} (configured={configured})",
                resolved.display()
            ));
        }
    };
    let path_like = |value: &str| {
        !(value.contains("://") || value.starts_with("ws:") || value.starts_with("wss:"))
            && (value.contains('/')
                || value.contains('\\')
                || value.ends_with(".json")
                || value.ends_with(".yaml")
                || value.ends_with(".yml")
                || value.ends_with(".toml"))
    };

    for worker in config.workers.iter().filter(|worker| worker.enabled) {
        if worker.instrument_spec_path.is_some() {
            failures.extend(worker_market_spec_failures(runtime_path, worker));
        }
        if let Some(credentials) = worker.credential_files.as_ref() {
            require_file(
                &mut failures,
                format!("worker {} credential api_key", worker.id),
                &credentials.api_key,
            );
            require_file(
                &mut failures,
                format!("worker {} credential secret", worker.id),
                &credentials.secret,
            );
        }
        if let Some(endpoint) = worker.endpoint.as_deref().filter(|value| path_like(value)) {
            require_file(
                &mut failures,
                format!("worker {} endpoint", worker.id),
                endpoint,
            );
            if matches!(
                worker.role,
                WorkerRole::MarketData
                    | WorkerRole::UserStream
                    | WorkerRole::Execution
                    | WorkerRole::SpreadRecovery
                    | WorkerRole::Reconciler
            ) {
                let endpoint_path = resolve_runtime_relative_path(runtime_path, endpoint);
                if endpoint_path.is_file() {
                    if let Err(error) = validate_ccxt_worker_binding(worker, &endpoint_path) {
                        failures.push(error);
                    }
                }
            }
        }
        if worker.role == WorkerRole::Scheduler {
            require_file(
                &mut failures,
                "scheduler.jobs_path".into(),
                &config.scheduler.jobs_path,
            );
        }
    }

    let mut strategies = Vec::with_capacity(config.strategies.len() + 1);
    strategies.push(("strategy".to_string(), &config.strategy));
    for strategy in &config.strategies {
        strategies.push((
            format!(
                "strategy[{}]",
                strategy.id.as_deref().unwrap_or("<missing-id>")
            ),
            strategy,
        ));
    }
    for (label, strategy) in strategies {
        if let Some(target) = strategy.target_snapshot_path.as_deref() {
            require_file(
                &mut failures,
                format!("{label}.target_snapshot_path"),
                target,
            );
        }
        if let Some(research) = strategy.research_snapshot_path.as_deref() {
            require_file(
                &mut failures,
                format!("{label}.research_snapshot_path"),
                research,
            );
        }
        if let Some(bundle) = strategy.dataset_bundle_path.as_deref() {
            require_file(
                &mut failures,
                format!("{label}.dataset_bundle_path"),
                bundle,
            );
            validate_dataset_bundle_component_references(
                runtime_path,
                &mut failures,
                &label,
                bundle,
                strategy,
            );
        }
        for (kind, component_path) in &strategy.dataset_component_paths {
            if kind != "bars" {
                require_file(
                    &mut failures,
                    format!("{label}.dataset_component_paths.{kind}"),
                    component_path,
                );
            }
        }
        if let Some(rules) = strategy.ashare_rules_path.as_deref() {
            require_file(&mut failures, format!("{label}.ashare_rules_path"), rules);
        }
        if let Some(actions) = strategy.ashare_actions_path.as_deref() {
            validate_ashare_component_json(
                runtime_path,
                &mut failures,
                format!("{label}.ashare_actions_path"),
                actions,
                "actions",
                strategy.instrument.as_deref(),
            );
        }
        if let Some(calendar) = strategy.ashare_calendar_path.as_deref() {
            validate_ashare_component_json(
                runtime_path,
                &mut failures,
                format!("{label}.ashare_calendar_path"),
                calendar,
                "calendar",
                None,
            );
        }
        if let Some(cost_rules) = strategy.cost_rules_path.as_deref() {
            // 缺失与"存在但内容非法"都要在这里报出来：装配只会带着第一条失败原因退出，
            // 而 `config validate` 是唯一能一次列全这类问题的入口。
            if let Some(problem) = cost_rules_problem(runtime_path, cost_rules) {
                failures.push(format!("{label}.cost_rules_path {problem}"));
            }
        }
        let fill_problem = fill_model_failure(strategy.fill_model.as_deref(), label.as_str());
        failures.extend(fill_problem);
        if let Some(bars) = strategy.bars_snapshot_path.as_deref() {
            if strategy.live_enabled {
                if !resolve_runtime_relative_path(runtime_path, bars).is_file() {
                    warnings.push(format!(
                        "{label}.bars_snapshot_path 尚不存在，将由 live market worker 首次生成: {bars}"
                    ));
                }
            } else if strategy.builtin_strategy.is_some() {
                require_file(&mut failures, format!("{label}.bars_snapshot_path"), bars);
            }
        } else if strategy.builtin_strategy.is_some() && !strategy.live_enabled {
            failures.push(format!(
                "{label}.builtin_strategy 非 live 模式必须配置 bars_snapshot_path"
            ));
        }
        if let Some(reference) = strategy.builtin_reference_bars_snapshot_path.as_deref() {
            if strategy.live_enabled {
                if !resolve_runtime_relative_path(runtime_path, reference).is_file() {
                    warnings.push(format!(
                        "{label}.builtin_reference_bars_snapshot_path 尚不存在，将由 live market worker 首次生成: {reference}"
                    ));
                }
            } else {
                require_file(
                    &mut failures,
                    format!("{label}.builtin_reference_bars_snapshot_path"),
                    reference,
                );
            }
        }
        for (field, value) in [
            (
                "external_executable",
                strategy.external_executable.as_deref(),
            ),
            ("python_module", strategy.python_module.as_deref()),
            ("c_abi_library", strategy.c_abi_library.as_deref()),
        ] {
            if let Some(value) = value.filter(|value| path_like(value)) {
                require_file(&mut failures, format!("{label}.{field}"), value);
            }
        }
    }

    (failures, warnings)
}

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
                    let Some(component) = bundle.components.get(kind) else {
                        failures.push(format!(
                            "{label}.dataset_component_paths.{kind} 对应的 bundle component 缺失"
                        ));
                        continue;
                    };
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
