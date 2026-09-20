//! 运维门禁命令：doctor、live-check、runtime-check、status、report 和引用校验。
//!
//! 这些入口只做静态检查与本地读模型聚合，不连接交易所、不发送订单。

use super::*;

pub(crate) fn configured_api_readiness(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
    control_store: &ControlStateBackend,
    metrics_dir: &Path,
    now_ms: u64,
    stale_after_ms: u64,
) -> ApiReadiness {
    if control_store.load().is_err() {
        return ApiReadiness {
            ready: false,
            detail: "control_store_unavailable".into(),
        };
    }

    if config.environment.eq_ignore_ascii_case("production")
        && !production_trading_assets_ready(config, runtime_config_path)
    {
        return ApiReadiness {
            ready: false,
            detail: "trading_safety_assets_unavailable".into(),
        };
    }

    let strategies = if config.strategies.is_empty() {
        std::slice::from_ref(&config.strategy)
    } else {
        config.strategies.as_slice()
    };
    let root = Path::new(&config.storage.data_dir);
    for strategy in strategies {
        if !strategy.research_snapshot_required {
            continue;
        }
        let Some(configured) = strategy.research_snapshot_path.as_deref() else {
            return ApiReadiness {
                ready: false,
                detail: "research_snapshot_unavailable".into(),
            };
        };
        let path = resolve_runtime_asset_path(runtime_config_path, root, configured);
        if !path.is_file() {
            return ApiReadiness {
                ready: false,
                detail: "research_snapshot_unavailable".into(),
            };
        }
        let payload = match std::fs::read_to_string(&path) {
            Ok(payload) => payload,
            Err(_) => {
                return ApiReadiness {
                    ready: false,
                    detail: "research_snapshot_unreadable".into(),
                }
            }
        };
        let research = match StrategyResearchSnapshot::from_json(&payload) {
            Ok(research) => research,
            Err(_) => {
                return ApiReadiness {
                    ready: false,
                    detail: "research_snapshot_invalid".into(),
                }
            }
        };
        if validate_research_snapshot_binding(strategy, &research).is_err()
            || research
                .validate_for(
                    &strategy.version,
                    strategy
                        .research_data_fingerprint
                        .as_deref()
                        .unwrap_or(research.candidate.config.data_fingerprint.as_str()),
                    now_ms,
                    config.environment.eq_ignore_ascii_case("production"),
                )
                .is_err()
        {
            return ApiReadiness {
                ready: false,
                detail: "research_snapshot_invalid".into(),
            };
        }
    }

    let metrics_unhealthy = std::fs::read_dir(metrics_dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| {
            (entry.path().extension().and_then(|value| value.to_str()) == Some("prom"))
                .then(|| std::fs::read_to_string(entry.path()).ok())
        })
        .flatten()
        .any(|content| worker_metrics_unhealthy(&content, now_ms, stale_after_ms));
    if metrics_unhealthy {
        return ApiReadiness {
            ready: false,
            detail: "worker_dependency_unavailable".into(),
        };
    }

    ApiReadiness {
        ready: true,
        detail: "dependencies_ready".into(),
    }
}

pub(crate) fn production_trading_assets_ready(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
) -> bool {
    config
        .workers
        .iter()
        .filter(|worker| {
            worker.enabled
                && matches!(
                    worker.role,
                    WorkerRole::UserStream
                        | WorkerRole::Execution
                        | WorkerRole::SpreadRecovery
                        | WorkerRole::Reconciler
                )
        })
        .all(|worker| {
            let credentials_ready =
                worker_credentials_ready(runtime_config_path, worker).unwrap_or(false);
            if !credentials_ready {
                return false;
            }
            worker.instrument_spec_path.as_deref().is_none_or(|path| {
                resolve_runtime_relative_path(runtime_config_path, path).is_file()
            })
        })
}

/// 检查私有 worker 的密钥来源，兼容三种部署方式：RuntimeConfig 环境变量、
/// RuntimeConfig 凭据文件，以及公共 CCXT JSON 配置中的 `credential_env`。
/// 这里只验证引用和环境变量是否可用，永远不返回或打印密钥内容。
pub(crate) fn worker_credentials_ready(
    runtime_config_path: &Path,
    worker: &WorkerConfig,
) -> Result<bool, String> {
    // CCXT worker 实际由 endpoint 配置驱动；当 endpoint 是本地 JSON 时，
    // 优先读取其中的 credential_env，避免 RuntimeConfig 中的兼容字段
    // 覆盖真正会被公共 CCXT 进程使用的凭据来源。
    let ccxt_endpoint = worker.endpoint.as_deref().filter(|endpoint| {
        !endpoint.contains("://")
            && matches!(
                worker.role,
                WorkerRole::MarketData
                    | WorkerRole::UserStream
                    | WorkerRole::Execution
                    | WorkerRole::SpreadRecovery
                    | WorkerRole::Reconciler
            )
    });
    if let Some(endpoint) = ccxt_endpoint {
        let endpoint_path = resolve_runtime_relative_path(runtime_config_path, endpoint);
        let payload = std::fs::read_to_string(&endpoint_path).map_err(|error| {
            format!(
                "读取 worker {} CCXT 配置失败 {}: {error}",
                worker.id,
                endpoint_path.display()
            )
        })?;
        let value = serde_json::from_str::<serde_json::Value>(&payload).map_err(|error| {
            format!(
                "解析 worker {} CCXT 配置失败 {}: {error}",
                worker.id,
                endpoint_path.display()
            )
        })?;
        let Some(credentials) = value.get("credential_env") else {
            return Ok(false);
        };
        let env_value = |key: &str| {
            credentials
                .get(key)
                .and_then(serde_json::Value::as_str)
                .is_some_and(non_empty_env)
        };
        let optional_env_value = |key: &str| {
            credentials
                .get(key)
                .and_then(serde_json::Value::as_str)
                .is_none_or(non_empty_env)
        };
        return Ok(env_value("api_key") && env_value("secret") && optional_env_value("password"));
    }
    if let Some(credentials) = worker.credential_env.as_ref() {
        return Ok(non_empty_env(&credentials.api_key) && non_empty_env(&credentials.secret));
    }
    if let Some(credentials) = worker.credential_files.as_ref() {
        return Ok(
            resolve_runtime_relative_path(runtime_config_path, &credentials.api_key).is_file()
                && resolve_runtime_relative_path(runtime_config_path, &credentials.secret)
                    .is_file(),
        );
    }
    let Some(endpoint) = worker.endpoint.as_deref() else {
        return Ok(false);
    };
    if endpoint.contains("://") || endpoint.starts_with("ws:") || endpoint.starts_with("wss:") {
        return Ok(false);
    }
    let endpoint_path = resolve_runtime_relative_path(runtime_config_path, endpoint);
    let payload = std::fs::read_to_string(&endpoint_path).map_err(|error| {
        format!(
            "读取 worker {} CCXT 配置失败 {}: {error}",
            worker.id,
            endpoint_path.display()
        )
    })?;
    let value = serde_json::from_str::<serde_json::Value>(&payload).map_err(|error| {
        format!(
            "解析 worker {} CCXT 配置失败 {}: {error}",
            worker.id,
            endpoint_path.display()
        )
    })?;
    let Some(credentials) = value.get("credential_env") else {
        return Ok(false);
    };
    let env_value = |key: &str| {
        credentials
            .get(key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(non_empty_env)
    };
    let optional_env_value = |key: &str| {
        credentials
            .get(key)
            .and_then(serde_json::Value::as_str)
            .is_none_or(non_empty_env)
    };
    Ok(env_value("api_key") && env_value("secret") && optional_env_value("password"))
}

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
        if let Some(costs) = strategy.cost_rules_path.as_deref() {
            require_file(&mut failures, format!("{label}.cost_rules_path"), costs);
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

fn validate_ashare_component_json(
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

fn validate_dataset_bundle_component_references(
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

fn list_backtest_summary_paths(runs_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut summaries = Vec::new();
    if !runs_dir.is_dir() {
        return Ok(summaries);
    }
    for entry in std::fs::read_dir(runs_dir)
        .map_err(|error| format!("读取回测结果目录失败 {}: {error}", runs_dir.display()))?
    {
        let entry = entry.map_err(|error| format!("读取回测结果目录项失败: {error}"))?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".summary.json"))
        {
            summaries.push(path);
        }
    }
    summaries.sort_by(|left, right| {
        let left_modified = std::fs::metadata(left)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        let right_modified = std::fs::metadata(right)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        left_modified
            .cmp(&right_modified)
            .then_with(|| left.cmp(right))
    });
    Ok(summaries)
}

pub(crate) fn resolve_backtest_summary_path(path: &Path) -> Result<PathBuf, String> {
    let is_summary = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".summary.json"));
    if is_summary {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(format!("指定回测摘要不存在: {}", path.display()));
    }
    let config = read_runtime_config(path)?;
    let data_dir = resolve_runtime_relative_path(path, &config.storage.data_dir);
    let summaries = list_backtest_summary_paths(&data_dir.join("runs"))?;
    summaries
        .last()
        .cloned()
        .ok_or_else(|| format!("未找到回测摘要: {}", data_dir.join("runs").display()))
}

pub(crate) fn run_report(path: &Path, as_json: bool) -> Result<(), String> {
    let summary_path = resolve_backtest_summary_path(path)?;
    let payload = std::fs::read_to_string(&summary_path)
        .map_err(|error| format!("读取回测摘要失败 {}: {error}", summary_path.display()))?;
    let summary: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| format!("回测摘要 JSON 无效 {}: {error}", summary_path.display()))?;
    if as_json {
        let report = serde_json::json!({
            "schema_version": 1,
            "summary_path": summary_path.display().to_string(),
            "summary": summary
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("编码回测报告 JSON 失败: {error}"))?
        );
        return Ok(());
    }

    let text = |pointer: &str, fallback: &str| -> String {
        summary
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .unwrap_or(fallback)
            .to_string()
    };
    let integer = |pointer: &str| {
        summary
            .pointer(pointer)
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
    };
    println!("[Report] summary={}", summary_path.display());
    println!(
        "  strategy={} instrument={} bars={} fills={}",
        text("/strategy_id", "-"),
        text("/instrument", "-"),
        integer("/bars"),
        integer("/fills")
    );
    println!(
        "  return_bps={} max_drawdown_bps={} fees_raw={} turnover_raw={} final_equity_raw={}",
        integer("/metrics/return_bps"),
        integer("/metrics/max_drawdown_bps"),
        integer("/metrics/fees_raw"),
        integer("/metrics/turnover_raw"),
        integer("/metrics/final_equity_raw")
    );
    println!(
        "  input_data_hash={} result_hash={} replay_hash={}",
        text("/input_data_hash", "-"),
        text("/result_hash", "-"),
        text("/replay_hash", "-")
    );
    Ok(())
}

pub(crate) fn run_status(path: &Path, as_json: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let data_dir = resolve_runtime_relative_path(path, &config.storage.data_dir);
    let runs_dir = data_dir.join("runs");
    let summaries = list_backtest_summary_paths(&runs_dir)?;
    let latest_summary = summaries.last().and_then(|summary_path| {
        std::fs::read_to_string(summary_path)
            .ok()
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(&payload).ok())
    });
    let enabled_workers = config
        .workers
        .iter()
        .filter(|worker| worker.enabled)
        .map(|worker| {
            serde_json::json!({
                "id": worker.id,
                "role": format!("{:?}", worker.role),
                "account_id": worker.account_id,
                "venue_id": worker.venue_id,
            })
        })
        .collect::<Vec<_>>();
    if as_json {
        let status = serde_json::json!({
            "schema_version": 1,
            "runtime_path": path,
            "environment": config.environment,
            "profile": config.profile,
            "storage_backend": config.storage.backend,
            "storage_consistency": config.storage.consistency,
            "data_dir": data_dir,
            "config_fingerprint": config.fingerprint()?,
            "enabled_workers": enabled_workers,
            "backtest_summary_count": summaries.len(),
            "latest_backtest": latest_summary,
            "network_accessed": false,
            "orders_sent": false,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&status)
                .map_err(|error| format!("编码 status JSON 失败: {error}"))?
        );
        return Ok(());
    }
    println!("[Status] runtime={}", path.display());
    println!(
        "  environment={} profile={:?} storage={:?} consistency={:?}",
        config.environment, config.profile, config.storage.backend, config.storage.consistency
    );
    println!(
        "  data_dir={} backtest_summaries={} network_accessed=false orders_sent=false",
        data_dir.display(),
        summaries.len()
    );
    for worker in config.workers.iter().filter(|worker| worker.enabled) {
        println!(
            "  worker id={} role={:?} account={} venue={} configured",
            worker.id,
            worker.role,
            worker.account_id.as_deref().unwrap_or("-"),
            worker.venue_id.as_deref().unwrap_or("-")
        );
    }
    if let Some(summary) = latest_summary {
        println!(
            "[Latest Backtest] strategy={} instrument={} fills={} return_bps={} max_drawdown_bps={} result_hash={}",
            summary
                .get("strategy_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-"),
            summary
                .get("instrument")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-"),
            summary
                .get("fills")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            summary
                .pointer("/metrics/return_bps")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
            summary
                .pointer("/metrics/max_drawdown_bps")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            summary
                .get("result_hash")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-")
        );
    } else {
        println!("[Latest Backtest] 暂无已保存回测摘要");
    }
    Ok(())
}

pub(crate) fn collect_doctor_report(path: &Path) -> Result<serde_json::Value, String> {
    let config = read_runtime_config(path)?;
    let fingerprint = config.fingerprint()?;
    let mut checks = Vec::new();
    let mut failures = Vec::new();
    let mut warnings = Vec::new();

    checks.push(serde_json::json!({
        "name": "config",
        "status": "pass",
        "message": "配置解析与领域校验通过"
    }));
    checks.push(serde_json::json!({
        "name": "fingerprint",
        "status": "pass",
        "message": format!("配置指纹={fingerprint}")
    }));

    let (reference_failures, reference_warnings) = validate_runtime_references(path, &config);
    for warning in reference_warnings {
        checks.push(serde_json::json!({
            "name": "runtime_reference",
            "status": "warn",
            "message": warning.clone()
        }));
        warnings.push(warning);
    }
    for failure in reference_failures {
        checks.push(serde_json::json!({
            "name": "runtime_reference",
            "status": "fail",
            "message": failure.clone()
        }));
        failures.push(failure);
    }

    for worker in config.workers.iter().filter(|worker| {
        worker.enabled
            && matches!(
                worker.role,
                WorkerRole::UserStream
                    | WorkerRole::Execution
                    | WorkerRole::SpreadRecovery
                    | WorkerRole::Reconciler
            )
            && worker.endpoint.is_some()
            && worker
                .endpoint
                .as_deref()
                .is_some_and(|endpoint| !endpoint.contains("://"))
    }) {
        match worker_credentials_ready(path, worker) {
            Ok(true) => checks.push(serde_json::json!({
                "name": format!("worker[{}].credentials", worker.id),
                "status": "pass",
                "message": "CCXT 配置中的凭据环境变量可用"
            })),
            Ok(false) => {
                let message = format!(
                    "worker {} 的 CCXT 配置未提供可用 credential_env；当前仅能运行公共能力",
                    worker.id
                );
                checks.push(serde_json::json!({
                    "name": format!("worker[{}].credentials", worker.id),
                    "status": "warn",
                    "message": message
                }));
                warnings.push(message);
            }
            Err(error) => {
                checks.push(serde_json::json!({
                    "name": format!("worker[{}].credentials", worker.id),
                    "status": "fail",
                    "message": error
                }));
                failures.push(error);
            }
        }
    }

    let data_dir = resolve_runtime_relative_path(path, &config.storage.data_dir);
    if data_dir.exists() {
        if data_dir.is_dir() {
            checks.push(serde_json::json!({
                "name": "storage.data_dir",
                "status": "pass",
                "message": data_dir.display().to_string()
            }));
        } else {
            let message = format!("storage.data_dir 不是目录: {}", data_dir.display());
            checks.push(serde_json::json!({
                "name": "storage.data_dir",
                "status": "fail",
                "message": message
            }));
            failures.push(message);
        }
    } else if data_dir.parent().is_some_and(|parent| parent.is_dir()) {
        let message = format!(
            "storage.data_dir 尚不存在，将在首次运行时创建: {}",
            data_dir.display()
        );
        checks.push(serde_json::json!({
            "name": "storage.data_dir",
            "status": "warn",
            "message": message
        }));
        warnings.push(message);
    } else {
        let message = format!("storage.data_dir 的父目录不存在: {}", data_dir.display());
        checks.push(serde_json::json!({
            "name": "storage.data_dir",
            "status": "fail",
            "message": message
        }));
        failures.push(message);
    }

    match RuntimeSupervisor::new(config.clone()) {
        Ok(supervisor) => {
            let health = supervisor
                .health()
                .lock()
                .map_err(|_| "运行时健康锁已中毒".to_string())?
                .snapshot(0, config.shutdown_timeout_ms);
            checks.push(serde_json::json!({
                "name": "runtime_topology",
                "status": "pass",
                "message": format!("overall={:?}", health.overall)
            }));
        }
        Err(error) => {
            let message = format!("运行拓扑构建失败: {error}");
            checks.push(serde_json::json!({
                "name": "runtime_topology",
                "status": "fail",
                "message": message
            }));
            failures.push(message);
        }
    }

    Ok(serde_json::json!({
        "schema_version": 1,
        "runtime_path": path.display().to_string(),
        "environment": config.environment,
        "profile": config.profile,
        "config_fingerprint": fingerprint,
        "ok": failures.is_empty(),
        "checks": checks,
        "warnings": warnings,
        "failures": failures,
        "network_accessed": false,
        "orders_sent": false
    }))
}

pub(crate) fn run_doctor(path: &Path, as_json: bool) -> Result<(), String> {
    let report = collect_doctor_report(path)?;
    let failures = report
        .get("failures")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    let warnings = report
        .get("warnings")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    let ok = report
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("编码 doctor JSON 失败: {error}"))?
        );
    } else {
        println!("[Doctor] 检查配置、路径、运行拓扑和策略输入");
        if let Some(checks) = report.get("checks").and_then(serde_json::Value::as_array) {
            for check in checks {
                let status = check
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_ascii_uppercase();
                let name = check
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("check");
                let message = check
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                println!("[{status}] {name}: {message}");
            }
            if ok {
                println!(
                    "[Doctor] 通过：{} 个警告；未连接交易所、未发送订单",
                    warnings
                );
            } else if let Some(items) = report.get("failures").and_then(serde_json::Value::as_array)
            {
                for failure in items.iter().filter_map(serde_json::Value::as_str) {
                    eprintln!("[FAIL] {failure}");
                }
            }
        }
    }

    if ok {
        Ok(())
    } else {
        Err(format!("Doctor 发现 {failures} 个必须修复的问题"))
    }
}

fn push_live_check(
    checks: &mut Vec<serde_json::Value>,
    name: impl Into<String>,
    status: &str,
    message: impl Into<String>,
) {
    checks.push(serde_json::json!({
        "name": name.into(),
        "status": status,
        "message": message.into(),
    }));
}

pub(crate) fn collect_live_check_report(path: &Path) -> Result<serde_json::Value, String> {
    let config = read_runtime_config(path)?;
    let fingerprint = config.fingerprint()?;
    let mut failures = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut checks = Vec::new();

    if !config.environment.eq_ignore_ascii_case("production") {
        let message = format!(
            "environment 必须为 production，当前为 {}",
            config.environment
        );
        push_live_check(&mut checks, "environment", "fail", &message);
        failures.push(message);
    } else {
        push_live_check(&mut checks, "environment", "pass", "environment=production");
    }
    match config.config_fingerprint.as_deref() {
        Some(_) => match config.verify_fingerprint() {
            Ok(()) => push_live_check(
                &mut checks,
                "config_fingerprint",
                "pass",
                "config_fingerprint locked=true",
            ),
            Err(error) => {
                push_live_check(&mut checks, "config_fingerprint", "fail", &error);
                failures.push(error);
            }
        },
        None => {
            let message = "production 必须配置 config_fingerprint 发布锁".to_string();
            push_live_check(&mut checks, "config_fingerprint", "fail", &message);
            failures.push(message);
        }
    }

    let expected_consistency = if config.messaging.enabled {
        StorageConsistency::DistributedOutbox
    } else {
        StorageConsistency::Transactional
    };
    if config.storage.consistency != expected_consistency {
        let message = format!(
            "production storage.consistency={:?} 与拓扑要求 {:?} 不一致",
            config.storage.consistency, expected_consistency
        );
        push_live_check(&mut checks, "storage.consistency", "fail", &message);
        failures.push(message);
    } else {
        push_live_check(
            &mut checks,
            "storage.consistency",
            "pass",
            format!("consistency={:?}", config.storage.consistency),
        );
    }

    for (label, configured) in [
        (
            "api.tls.certificate_chain",
            config
                .api
                .tls
                .as_ref()
                .map(|tls| tls.certificate_chain.as_str()),
        ),
        (
            "api.tls.private_key",
            config.api.tls.as_ref().map(|tls| tls.private_key.as_str()),
        ),
        (
            "api.tls.client_ca",
            config.api.tls.as_ref().map(|tls| tls.client_ca.as_str()),
        ),
    ] {
        match configured {
            Some(file) if resolve_runtime_relative_path(path, file).exists() => push_live_check(
                &mut checks,
                label,
                "pass",
                resolve_runtime_relative_path(path, file)
                    .display()
                    .to_string(),
            ),
            Some(file) => {
                let message = format!(
                    "{label} 文件不存在: {}",
                    resolve_runtime_relative_path(path, file).display()
                );
                push_live_check(&mut checks, label, "fail", &message);
                failures.push(message);
            }
            None => {
                let message = format!("{label} 未配置");
                push_live_check(&mut checks, label, "fail", &message);
                failures.push(message);
            }
        }
    }

    let mut execution_count = 0_usize;
    for worker in config.workers.iter().filter(|worker| worker.enabled) {
        if matches!(
            worker.role,
            WorkerRole::Execution | WorkerRole::SpreadRecovery
        ) {
            if worker.role == WorkerRole::Execution {
                execution_count += 1;
            }
            if worker
                .venue_id
                .as_deref()
                .is_some_and(|venue| venue.eq_ignore_ascii_case("paper"))
            {
                let message = format!(
                    "{} 是 production 中不允许启用的 Paper Execution/SpreadRecovery worker",
                    worker.id
                );
                push_live_check(
                    &mut checks,
                    format!("worker[{}].venue", worker.id),
                    "fail",
                    &message,
                );
                failures.push(message);
            }
            let spec = worker.instrument_spec_path.as_deref().unwrap_or_default();
            let spec_path = resolve_runtime_relative_path(path, spec);
            if spec.is_empty() || !spec_path.exists() {
                let message = format!(
                    "{} instrument_spec_path 不可用: {}",
                    worker.id,
                    spec_path.display()
                );
                push_live_check(
                    &mut checks,
                    format!("worker[{}].instrument_spec", worker.id),
                    "fail",
                    &message,
                );
                failures.push(message);
            } else {
                push_live_check(
                    &mut checks,
                    format!("worker[{}].instrument_spec", worker.id),
                    "pass",
                    spec_path.display().to_string(),
                );
            }
            if worker.max_order_notional_raw.is_none() || worker.max_position_notional_raw.is_none()
            {
                let message = format!("{} 缺少订单或持仓名义额上限", worker.id);
                push_live_check(
                    &mut checks,
                    format!("worker[{}].risk_limits", worker.id),
                    "fail",
                    &message,
                );
                failures.push(message);
            } else {
                push_live_check(
                    &mut checks,
                    format!("worker[{}].risk_limits", worker.id),
                    "pass",
                    "order and position notional limits configured",
                );
            }
        }
        if matches!(
            worker.role,
            WorkerRole::UserStream
                | WorkerRole::Execution
                | WorkerRole::SpreadRecovery
                | WorkerRole::Reconciler
        ) {
            match worker_credentials_ready(path, worker) {
                Ok(true) => push_live_check(
                    &mut checks,
                    format!("worker[{}].credentials", worker.id),
                    "pass",
                    "credentials source is available",
                ),
                Ok(false) => {
                    let message = format!("{} 凭据环境变量或凭据文件不可用", worker.id);
                    push_live_check(
                        &mut checks,
                        format!("worker[{}].credentials", worker.id),
                        "fail",
                        &message,
                    );
                    failures.push(message);
                }
                Err(error) => {
                    push_live_check(
                        &mut checks,
                        format!("worker[{}].credentials", worker.id),
                        "fail",
                        &error,
                    );
                    failures.push(error);
                }
            }
        }
    }
    if execution_count == 0 {
        let message = "production 至少需要一个启用的 Execution worker".to_string();
        push_live_check(&mut checks, "execution_workers", "fail", &message);
        failures.push(message);
    } else {
        push_live_check(
            &mut checks,
            "execution_workers",
            "pass",
            format!("enabled_execution_workers={execution_count}"),
        );
    }

    for strategy in config
        .strategies
        .iter()
        .chain(std::iter::once(&config.strategy))
        .filter(|strategy| strategy.account_id.is_some() || strategy.venue_id.is_some())
    {
        if let Some(snapshot) = strategy.research_snapshot_path.as_deref() {
            let snapshot_path = resolve_runtime_relative_path(path, snapshot);
            if snapshot_path.is_file() {
                push_live_check(
                    &mut checks,
                    "research_snapshot",
                    "pass",
                    snapshot_path.display().to_string(),
                );
            } else {
                let message = format!(
                    "research_snapshot_path 文件不存在: {}",
                    snapshot_path.display()
                );
                push_live_check(&mut checks, "research_snapshot", "fail", &message);
                failures.push(message);
            }
        }
    }
    if config.api.bind.starts_with("127.") || config.api.bind.starts_with("localhost") {
        let message = "API 仅绑定本机地址，适合单机部署，不适合跨节点访问".to_string();
        push_live_check(&mut checks, "api.bind", "warn", &message);
        warnings.push(message);
    }

    let ok = failures.is_empty();
    Ok(serde_json::json!({
        "schema_version": 1,
        "runtime_path": path.display().to_string(),
        "environment": config.environment,
        "profile": config.profile,
        "storage_backend": config.storage.backend,
        "storage_consistency": config.storage.consistency,
        "config_fingerprint": fingerprint,
        "config_fingerprint_locked": config.config_fingerprint.is_some(),
        "checks": checks,
        "warnings": warnings,
        "failures": failures,
        "ok": ok,
        "network_accessed": false,
        "orders_sent": false
    }))
}

pub(crate) fn run_live_check(path: &Path, as_json: bool) -> Result<(), String> {
    let report = collect_live_check_report(path)?;
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("编码 live-check JSON 失败: {error}"))?
        );
        if report.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
            return Err("实盘前置检查未通过".into());
        }
        return Ok(());
    }

    if let Some(checks) = report.get("checks").and_then(serde_json::Value::as_array) {
        for check in checks {
            let status = check
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_ascii_uppercase();
            let name = check
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("check");
            let message = check
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if status == "FAIL" {
                eprintln!("[{status}] {name}: {message}");
            } else {
                println!("[{status}] {name}: {message}");
            }
        }
    }
    let failures = report
        .get("failures")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    let ok = report
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if ok {
        println!("[PASS] live-check 全部通过：未连接交易所，未发送订单");
        Ok(())
    } else {
        Err(format!("实盘前置检查失败，共 {failures} 项"))
    }
}

pub(crate) fn doctor_command(argv: &[String]) {
    let path = argv
        .iter()
        .skip(2)
        .find(|&argument| !argument.starts_with('-'))
        .cloned()
        .map(PathBuf::from)
        .unwrap_or_else(default_runtime_path);
    if let Err(error) = run_doctor(&path, argv.iter().any(|argument| argument == "--json")) {
        eprintln!("Doctor 失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn report_command(argv: &[String]) {
    let path = argv
        .iter()
        .skip(2)
        .find(|&argument| !argument.starts_with('-'))
        .cloned()
        .map(PathBuf::from)
        .unwrap_or_else(default_runtime_path);
    if let Err(error) = run_report(&path, argv.iter().any(|argument| argument == "--json")) {
        eprintln!("报告查看失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn status_command(argv: &[String]) {
    let path = argv
        .get(2)
        .cloned()
        .filter(|value| !value.starts_with('-'))
        .map(PathBuf::from)
        .unwrap_or_else(default_runtime_path);
    if let Err(error) = run_status(&path, argv.iter().any(|argument| argument == "--json")) {
        eprintln!("状态查看失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn live_check_command(argv: &[String]) {
    let path = argv
        .iter()
        .skip(2)
        .find(|&argument| !argument.starts_with('-'))
        .cloned()
        .unwrap_or_else(|| "deploy/qianxing.runtime.production.example.json".into());
    if let Err(error) = run_live_check(
        Path::new(&path),
        argv.iter().any(|argument| argument == "--json"),
    ) {
        eprintln!("实盘前置检查失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn runtime_check_command(argv: &[String]) {
    let path = argv
        .iter()
        .skip(2)
        .find(|&argument| !argument.starts_with('-'))
        .cloned()
        .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
    if let Err(error) = run_runtime_check(
        Path::new(&path),
        argv.iter().any(|argument| argument == "--json"),
    ) {
        eprintln!("运行时配置校验失败: {error}");
        std::process::exit(2);
    }
}
