//! 上线体检链：`live-check` 的报告采集与逐项检查出口。

use super::*;

pub(crate) fn push_live_check(
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
            if VenueFamily::parse_option(worker.venue_id.as_deref()) == Some(VenueFamily::Paper) {
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
