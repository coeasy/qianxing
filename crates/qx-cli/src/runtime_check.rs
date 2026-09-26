//! 运行时体检链：`runtime-check` 的清单采集、引用校验与 JSON 出口。

use super::*;

pub(crate) fn collect_runtime_check_report(path: &Path) -> Result<serde_json::Value, String> {
    let config = read_runtime_config(path)?;
    let (failures, reference_warnings) = validate_runtime_references(path, &config);
    let supervisor = RuntimeSupervisor::new(config.clone())?;
    let health = supervisor
        .health()
        .lock()
        .map_err(|_| "运行时健康锁已中毒".to_string())?
        // 时钟取真实的现在：`snapshot(0, …)` 会让"心跳是否过期"这道比较恒为假，校验通过的
        // shutdown_timeout_ms 在这一格就等于什么都不约束（V11 L4）。
        .snapshot(runtime_timestamp_ms(), config.shutdown_timeout_ms);
    // 体检跑在任何 worker 启动之前，注册表里只有 `register()` 写下的 Starting 行：`overall`
    // 到不了 Failed，健康半边对 `ok` 投不出反对票。所以这块如实声明成"配置派生的花名册、没有
    // 心跳事实"（health_observed），别让人把 Starting 读成活体探测过了；真正会反对的是上面的
    // 引用校验与 `RuntimeSupervisor::new` 的拓扑校验这两处。
    let health_observed = health
        .services
        .iter()
        .any(|service| service.last_heartbeat_ms.is_some());
    let fingerprint = config.fingerprint()?;
    let environment = config.environment.clone();
    let profile = config.profile;
    let api_transport = config.api.transport;
    let storage_backend = config.storage.backend;
    let storage_consistency = config.storage.consistency;
    let fingerprint_locked = config.config_fingerprint.is_some();
    let ok = failures.is_empty() && !matches!(health.overall, qx_runtime::OverallHealth::Failed);
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
        "health_observed": health_observed,
        "warnings": reference_warnings,
        "failures": failures,
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
        "[运行时 · 健康] overall={} observed={}（observed=false 表示这份花名册来自配置，没有心跳事实）",
        health
            .and_then(|value| value.get("overall"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown"),
        report
            .get("health_observed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
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

/// 只在 `strategies[]` 上声明、而顶层 `strategy` 段没有同一路径的成本口径，没有任何执行
/// 入口会去应用它：装配走的是 [`execution_cost_binding_from_config`]，那里读的是顶层那一份。
/// `config validate` 会把实例那一份文件读一遍并说"没问题"，跑起来用的却是顶层路径或内核
/// 默认费率——声明与生效之间是断的，所以在装配处与校验处一起拒（V11 N5）。
pub(crate) fn unapplied_cost_rules_declaration(
    config: &RuntimeConfig,
    runtime_config_path: Option<&Path>,
) -> Option<String> {
    let resolve = |configured: &str| match runtime_config_path {
        Some(base) => resolve_runtime_relative_path(base, configured),
        None => PathBuf::from(configured),
    };
    let applied = config.strategy.cost_rules_path.as_deref().map(resolve);
    for strategy in &config.strategies {
        let Some(configured) = strategy.cost_rules_path.as_deref() else {
            continue;
        };
        let declared = resolve(configured);
        if applied.as_ref() == Some(&declared) {
            continue;
        }
        let applied_note = match &applied {
            Some(path) => format!("顶层当前生效的是 {}", path.display()),
            None => "顶层没有声明，当前生效的是内核默认费率".to_string(),
        };
        return Some(format!(
            "strategies[{}] 声明的 cost_rules_path {} 不会被应用：执行平面只读顶层 `strategy.cost_rules_path`（{}）；\
             请把这条声明挪到顶层，或从实例里删掉它",
            strategy.id.as_deref().unwrap_or("<missing-id>"),
            declared.display(),
            applied_note
        ));
    }
    None
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

    // 成本口径"声明了却没人应用"要在装配处之外同样报出来：`config validate` 是唯一能
    // 一次列全这类问题的入口，它不能比执行平面更宽松（V11 N5）。
    if let Some(problem) = unapplied_cost_rules_declaration(config, Some(runtime_path)) {
        failures.push(problem);
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
