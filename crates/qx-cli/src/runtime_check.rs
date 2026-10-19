//! 运行时体检链：`runtime-check` 的清单采集、引用校验与 JSON 出口。

use crate::{collect_doctor_report_for_config, read_runtime_config, runtime_timestamp_ms};
use qx_runtime::RuntimeSupervisor;
use std::path::Path;

pub(crate) fn collect_runtime_check_report(path: &Path) -> Result<serde_json::Value, String> {
    let config = read_runtime_config(path)?;
    let supervisor = RuntimeSupervisor::new(config.clone())?;
    // `doctor` owns the static configuration/path/topology checklist. Reuse that collected report
    // so `runtime-check` adds runtime health to the same evidence instead of rebuilding a second,
    // narrower reference checklist.
    let base = collect_doctor_report_for_config(path, &config, Ok(()))?;
    // 心跳新鲜度只有一个口径：serve 侧读 worker 指标用的 `messaging.worker_stale_after_ms`；
    // 曾传 `0` 当时刻、`shutdown_timeout_ms` 当预算，前者使过期判定恒不成立（V12 §16 #122）。
    let stale_ms = config.messaging.worker_stale_after_ms;
    let health = supervisor
        .health()
        .lock()
        .map_err(|_| "运行时健康锁已中毒".to_string())?
        .snapshot(runtime_timestamp_ms(), stale_ms);
    let fingerprint = config.fingerprint()?;
    let environment = config.environment.clone();
    let profile = config.profile;
    let api_transport = config.api.transport;
    let storage_backend = config.storage.backend;
    let storage_consistency = config.storage.consistency;
    let fingerprint_locked = config.config_fingerprint.is_some();
    let mut readiness_layers = base
        .get("readiness_layers")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let mut checks = base
        .get("checks")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let warnings = base
        .get("warnings")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut failures = base
        .get("failures")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let health_failed = matches!(health.overall, qx_runtime::OverallHealth::Failed);
    checks.push(serde_json::json!({
        "name": "runtime_health",
        "status": if health_failed { "fail" } else { "pass" },
        "message": format!("overall={:?}", health.overall)
    }));
    if health_failed {
        failures.push(serde_json::json!("运行时监督器健康状态为 Failed"));
    }
    let operator_status = match health.overall {
        qx_runtime::OverallHealth::Ready => "pass",
        qx_runtime::OverallHealth::Failed => "blocked",
        qx_runtime::OverallHealth::Starting
        | qx_runtime::OverallHealth::Degraded
        | qx_runtime::OverallHealth::Stopped => "not_assessed",
    };
    readiness_layers["O"] = serde_json::json!({
        "status": operator_status,
        "scope": "runtime_snapshot",
        "evidence": ["runtime_health"],
        "detail": match operator_status {
            "pass" => "运行时监督器报告 Ready",
            "blocked" => "运行时监督器报告 Failed",
            _ => "监督器快照尚未证明运行就绪",
        }
    });
    let ok = failures.is_empty();
    Ok(serde_json::json!({
        "schema_version": 3,
        "runtime_path": path.display().to_string(),
        "environment": environment,
        "profile": profile,
        "api_transport": api_transport,
        "storage_backend": storage_backend,
        "storage_consistency": storage_consistency,
        "config_fingerprint": fingerprint,
        "config_fingerprint_locked": fingerprint_locked,
        "health": health,
        "readiness_layers": readiness_layers,
        "checks": checks,
        "warnings": warnings,
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

    let environment = text_field(&report, "environment", "-");
    let profile = text_field(&report, "profile", "-");
    let api_transport = text_field(&report, "api_transport", "-");
    let storage_backend = text_field(&report, "storage_backend", "-");
    let storage_consistency = text_field(&report, "storage_consistency", "-");
    let fingerprint = text_field(&report, "config_fingerprint", "-");
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
        health.map_or("unknown", |value| text_field(value, "overall", "unknown"))
    );
    if let Some(services) = health
        .and_then(|value| value.get("services"))
        .and_then(serde_json::Value::as_array)
    {
        for service in services {
            println!(
                "  {} role={:?} status={:?}",
                text_field(service, "id", "-"),
                service.get("role").unwrap_or(&serde_json::Value::Null),
                text_field(service, "status", "unknown")
            );
        }
    }
    for warning in string_array(&report, "warnings") {
        println!("[WARN] {warning}");
    }
    let failures = string_array(&report, "failures");
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

/// 报告里的一个标量字段：缺失、为 null 或不是字符串一律读成 `fallback`，各读点因此只有一份兜底。
fn text_field<'a>(report: &'a serde_json::Value, key: &str, fallback: &'a str) -> &'a str {
    report
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or(fallback)
}

/// 报告里的一个字符串数组字段：缺失或非数组一律按空处理，两处读点因此不必各写一遍兜底。
fn string_array<'a>(report: &'a serde_json::Value, key: &str) -> Vec<&'a str> {
    let Some(items) = report.get(key).and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    items.iter().filter_map(serde_json::Value::as_str).collect()
}

mod references;
pub(crate) use references::*;
