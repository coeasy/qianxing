//! Static runtime doctor report assembly and capability query output.

use super::*;

pub(crate) fn collect_doctor_report(path: &Path) -> Result<serde_json::Value, String> {
    let config = read_runtime_config(path)?;
    let supervisor_build = RuntimeSupervisor::new(config.clone()).map(|_| ());
    collect_doctor_report_for_config(path, &config, supervisor_build)
}

pub(crate) fn collect_doctor_report_for_config(
    path: &Path,
    config: &RuntimeConfig,
    supervisor_build: Result<(), String>,
) -> Result<serde_json::Value, String> {
    let fingerprint = config.fingerprint()?;
    // 诊断的第一格先回答"我是哪个构建在说这句话"（U1）。
    let mut checks = vec![crate::build_identity::doctor_check()];
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

    let (reference_failures, reference_warnings) = validate_runtime_references(path, config);
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
        writes_account_ledger(worker)
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

    check_storage_data_dir(
        path,
        &config.storage.data_dir,
        &mut checks,
        &mut warnings,
        &mut failures,
    );
    check_account_log_settlement(config, &mut checks, &mut failures);
    check_orphan_event_logs(path, config, &mut checks, &mut warnings);

    // 判的是"这份配置能否构建出监督器"（`new` = 已判过的 validate + 按启用 worker 注册
    // 服务），不是运行健康：此刻一个 worker 都没启动，原名 `runtime_topology: pass` 加
    // `overall=Starting` 会让读报告的人以为拓扑被判成了健康。
    let enabled_workers = config
        .workers
        .iter()
        .filter(|worker| worker.enabled)
        .count();
    let (status, message) = match supervisor_build {
        Ok(()) => (
            "pass",
            format!("监督器可构建，启用 worker={enabled_workers}（未启动，不代表运行健康）"),
        ),
        Err(error) => ("fail", format!("运行时监督器构建失败: {error}")),
    };
    checks.push(serde_json::json!({
        "name": "runtime_supervisor_build",
        "status": status,
        "message": message.clone()
    }));
    if status == "fail" {
        failures.push(message);
    }

    let readiness_layers = serde_json::json!({
        "R": {
            "status": if failures.is_empty() { "pass" } else { "blocked" },
            "scope": "static_research_preflight",
            "evidence": checks.iter().filter_map(|check| check.get("name").and_then(serde_json::Value::as_str)).collect::<Vec<_>>(),
            "detail": "配置、路径与监督器构建检查；不代表回测结果有效"
        },
        "P": {
            "status": "not_assessed",
            "scope": "paper_execution",
            "required_command": "paper-check",
            "evidence": Vec::<String>::new(),
            "detail": "需要执行 Paper 主链路验收"
        },
        "O": {
            "status": "not_assessed",
            "scope": "operator_runtime",
            "required_command": "runtime-check",
            "evidence": Vec::<String>::new(),
            "detail": "静态 doctor 报告不包含已启动 worker 的运行状态"
        },
        "L": {
            "status": "not_assessed",
            "scope": "live_account",
            "required_command": "live-check",
            "evidence": Vec::<String>::new(),
            "detail": "静态报告不连接真实账户，也不提交订单"
        }
    });

    Ok(serde_json::json!({
        "schema_version": 2,
        "runtime_path": path.display().to_string(),
        "environment": config.environment.clone(),
        "profile": config.profile,
        "config_fingerprint": fingerprint,
        "readiness_layers": readiness_layers,
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
        println!("[Doctor] 检查配置、路径、策略输入与监督器可构建性（不启动 worker）");
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
                println!("[Doctor] 通过：{warnings} 个警告；未连接交易所、未发送订单");
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

pub(crate) fn run_doctor_capabilities(
    as_json: bool,
    capability_id: Option<&str>,
    venue: Option<&str>,
    product: Option<&str>,
    account_mode: Option<&str>,
    account_model: Option<&str>,
    margin_mode: Option<&str>,
    operation: Option<&str>,
) -> Result<(), String> {
    let catalog = qx_api::product_capability_catalog_document()
        .map_err(|error| format!("读取产品能力目录失败: {error}"))?;
    let filters_provided = venue.is_some()
        || product.is_some()
        || account_mode.is_some()
        || account_model.is_some()
        || margin_mode.is_some()
        || operation.is_some();
    let payload = if capability_id.is_some() || filters_provided {
        let mut query = serde_json::Map::new();
        for (name, value) in [
            ("capability_id", capability_id),
            ("venue", venue),
            ("product", product),
            ("account_mode", account_mode),
            ("account_model", account_model),
            ("margin_mode", margin_mode),
            ("operation", operation),
        ] {
            if let Some(value) = value {
                query.insert(name.into(), serde_json::Value::String(value.into()));
            }
        }
        qx_api::product_capability_query_document(&serde_json::Value::Object(query).to_string())
            .map_err(|error| {
                if error == "capability_not_found" {
                    "能力 ID 不存在".to_string()
                } else {
                    format!("查询产品能力失败: {error}")
                }
            })?
    } else {
        serde_json::json!({
            "schema_version": catalog["schema_version"],
            "source_updated_at": catalog["source_updated_at"],
            "capabilities": catalog["index"],
        })
    };

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|error| format!("编码产品能力目录失败: {error}"))?
        );
    } else if let Some(capability) = payload.get("capability") {
        println!(
            "[Product Capability] {}",
            capability["id"].as_str().unwrap_or("unknown")
        );
        println!(
            "{}",
            serde_json::to_string_pretty(capability)
                .map_err(|error| format!("编码产品能力详情失败: {error}"))?
        );
        println!("静态能力与验收登记不代表当前环境连通、凭据可用或沙盒验收通过。");
    } else if let Some(matches) = payload.get("matches").and_then(serde_json::Value::as_array) {
        println!("[Product Capability Scopes] count={}", matches.len());
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|error| format!("编码产品能力查询结果失败: {error}"))?
        );
        println!("范围仅代表仓内配置与代码证据；外部 sandbox/生产验收状态见匹配能力的成熟度证据。");
    } else {
        let entries = payload
            .get("capabilities")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "生成目录缺少 capabilities 索引".to_string())?;
        println!(
            "[Product Capabilities] schema={} count={}",
            payload["schema_version"],
            entries.len()
        );
        for entry in entries {
            let id = entry["id"].as_str().unwrap_or("unknown");
            let kind = entry["kind"].as_str().unwrap_or("unknown");
            let name = entry["name"].as_str().unwrap_or("unknown");
            let status =
                serde_json::to_string(&entry["matrix_status"]).unwrap_or_else(|_| "{}".to_string());
            let maturity_refs = entry
                .get("maturity_refs")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len)
                .ok_or_else(|| format!("能力 {id} 缺少 maturity_refs 数组"))?;
            let evidence_count = entry
                .get("evidence_path_count")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| format!("能力 {id} 缺少 evidence_path_count 数值"))?;
            println!(
                "{id}  [{kind}] {name}  status={status}  maturity_refs={maturity_refs} evidence_paths={evidence_count}"
            );
        }
        println!("静态能力与验收登记不代表当前环境连通、凭据可用或沙盒验收通过。");
        println!("查看详情：qx-cli doctor --capabilities --capability-id <能力 ID>");
        println!(
            "按交易范围查询：qx-cli doctor --capabilities --venue BINANCE --product perpetual"
        );
        println!(
            "按账户读取能力查询：qx-cli doctor --capabilities --venue OKX --product perpetual --account-model unified --operation positions.read"
        );
    }
    Ok(())
}
