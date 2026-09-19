//! 运行时体检链：`runtime-check` 的清单采集、引用校验与 JSON 出口。

use super::*;

pub(crate) fn collect_runtime_check_report(path: &Path) -> Result<serde_json::Value, String> {
    let config = read_runtime_config(path)?;
    let (reference_failures, reference_warnings) = validate_runtime_references(path, &config);
    let supervisor = RuntimeSupervisor::new(config.clone())?;
    let health = supervisor
        .health()
        .lock()
        .map_err(|_| "运行时健康锁已中毒".to_string())?
        .snapshot(0, config.shutdown_timeout_ms);
    let fingerprint = config.fingerprint()?;
    let environment = config.environment.clone();
    let profile = config.profile;
    let api_transport = config.api.transport;
    let storage_backend = config.storage.backend;
    let storage_consistency = config.storage.consistency;
    let fingerprint_locked = config.config_fingerprint.is_some();
    let ok = reference_failures.is_empty()
        && !matches!(health.overall, qx_runtime::OverallHealth::Failed);
    Ok(serde_json::json!({
        "schema_version": 1,
        "runtime_path": path.display().to_string(),
        "environment": environment,
        "profile": profile,
        "api_transport": api_transport,
        "storage_backend": storage_backend,
        "storage_consistency": storage_consistency,
        "config_fingerprint": fingerprint,
        "config_fingerprint_locked": fingerprint_locked,
        "health": health,
        "warnings": reference_warnings,
        "failures": reference_failures,
        "ok": ok,
        "network_accessed": false,
        "orders_sent": false
    }))
}

pub(crate) fn run_runtime_check(path: &Path, as_json: bool) -> Result<(), String> {
    let report = collect_runtime_check_report(path)?;
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("编码 runtime-check JSON 失败: {error}"))?
        );
        if report.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
            return Err("运行时配置校验未通过".into());
        }
        return Ok(());
    }

    let environment = report
        .get("environment")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("-");
    let profile = report
        .get("profile")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("-");
    let api_transport = report
        .get("api_transport")
        .map(ToString::to_string)
        .unwrap_or_else(|| "-".into());
    let storage_backend = report
        .get("storage_backend")
        .map(ToString::to_string)
        .unwrap_or_else(|| "-".into());
    let storage_consistency = report
        .get("storage_consistency")
        .map(ToString::to_string)
        .unwrap_or_else(|| "-".into());
    let fingerprint = report
        .get("config_fingerprint")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("-");
    let locked = report
        .get("config_fingerprint_locked")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let health = report.get("health");
    println!(
        "[运行时 · 配置] environment={} profile={:?} api={:?} workers={} storage={:?} consistency={:?}",
        environment,
        profile,
        api_transport,
        health
            .and_then(|value| value.get("services"))
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len),
        storage_backend,
        storage_consistency
    );
    println!(
        "[运行时 · 指纹] config_fingerprint={} locked={}",
        fingerprint, locked
    );
    println!(
        "[运行时 · 健康] overall={}",
        health
            .and_then(|value| value.get("overall"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
    );
    if let Some(services) = health
        .and_then(|value| value.get("services"))
        .and_then(serde_json::Value::as_array)
    {
        for service in services {
            println!(
                "  {} role={:?} status={:?}",
                service
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("-"),
                service.get("role").unwrap_or(&serde_json::Value::Null),
                service
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
            );
        }
    }
    for warning in report
        .get("warnings")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
    {
        println!("[WARN] {warning}");
    }
    let failures = report
        .get("failures")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    if failures.is_empty() && report.get("ok").and_then(serde_json::Value::as_bool) != Some(false) {
        println!("[PASS] runtime 引用文件校验通过");
        Ok(())
    } else {
        for failure in &failures {
            eprintln!("[FAIL] {failure}");
        }
        Err(format!(
            "运行时配置校验失败，共 {} 项",
            failures.len().max(1)
        ))
    }
}

pub(crate) fn validate_runtime_references(
    runtime_path: &Path,
    config: &RuntimeConfig,
) -> (Vec<String>, Vec<String>) {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();

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
        if let Some(spec) = worker.instrument_spec_path.as_deref() {
            require_file(
                &mut failures,
                format!("worker {} instrument_spec_path", worker.id),
                spec,
            );
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
