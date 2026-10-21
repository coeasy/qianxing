//! 配置与运维查询命令面：`config`/`status`/`doctor`/`report`/`live-check` 与 `run` 统一派发。
//! 只做配置读取、引用校验与人读/机读输出；帮助文本见 `cli_help.rs`，初始化一族见 `init_project.rs`。

use crate::{
    build_identity, check_account_log_settlement, check_orphan_event_logs, check_storage_data_dir,
    effective_storage_root, latest_backtest_readout_lines, list_backtest_summary_paths,
    read_runtime_config, repository_deploy_path, run_entry_arguments, run_live_check,
    run_paper_check, run_paper_pipeline_once, run_runtime_check, run_unified_backtest,
    run_unified_report_entry, run_usage, validate_runtime_references, worker_credentials_ready,
    writes_account_ledger,
};
use qx_runtime::{RuntimeConfig, RuntimeSupervisor};
use std::path::{Path, PathBuf};

pub(crate) fn default_runtime_path() -> PathBuf {
    repository_deploy_path("qianxing.runtime.example.json")
}

/// `run` 统一入口的**单一**默认运行时：与 `run backtest` 用的是同一份。
///
/// 这不是口味问题。`run` 自称"统一执行常用安全入口"（`cli_help.rs`），而它最常用的一对是
/// `run backtest` 写产物、`run report` 读产物——两者落到不同的 `storage.data_dir` 时，刚写出的
/// 摘要另一端一个都找不到。实测（V13 R28）：`run backtest` 写进
/// `data/qianxing-builtin-strategy/runs`，紧接着 `run report` 报
/// `未找到回测摘要: data/qianxing\runs` 并以 2 退出，而两份模板的差别只是默认运行时不同
/// （`run backtest` 走 `qianxing.runtime.builtin-strategy.example.json`，其余数据面子命令走
/// [`default_runtime_path`] 的 `qianxing.runtime.example.json`）。
///
/// 为什么统一到内置策略那一份而不是反过来：`main.rs` 的 `run_unified_backtest` 特意把默认从
/// 跨语言策略模板换成内置策略模板（V13 R31 ① F6）——README 安装路径 A 只装 CLI 不装 Python，
/// 旧默认那份策略由 Python worker 承担，`qx-cli backtest` 一敲就撞解释器。所以"链条的写侧"
/// 已经定在内置策略那一份，读侧跟上；`default_runtime_path` 保持给顶层命令（`config`/`status`/
/// 顶层 `doctor`/`report`）用，那里没有"写侧与读侧配对"的关系。
pub(crate) fn unified_default_runtime_path() -> PathBuf {
    repository_deploy_path("qianxing.runtime.builtin-strategy.example.json")
}

pub(crate) fn run_config_explain(path: &Path, as_json: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    if as_json {
        // RuntimeConfig 只包含凭据引用（环境变量名/投影文件路径），不包含
        // secret 内容；输出可直接交给脚本或配置审计工具继续处理。
        println!("{}", config.to_json()?);
        return Ok(());
    }
    let strategy_count = config.strategies.len().max(1);
    println!("[配置 · 有效] path={}", path.display());
    println!(
        "  schema={} environment={} profile={:?}",
        config.schema_version, config.environment, config.profile
    );
    println!(
        "  storage={:?} data_dir={} api={} transport={:?}",
        config.storage.backend,
        // 与 doctor 同口径：data_dir 有两个落点（运行态按当前目录、产物按 runtime.json 同级）。
        effective_storage_root(path, &config.storage.data_dir).display(),
        config.api.bind,
        config.api.transport
    );
    println!(
        "  workers={} enabled={} strategies={} fingerprint={} locked={}",
        config.workers.len(),
        config
            .workers
            .iter()
            .filter(|worker| worker.enabled)
            .count(),
        strategy_count,
        config.fingerprint()?,
        config.config_fingerprint.is_some()
    );
    for worker in config.workers.iter().filter(|worker| worker.enabled) {
        println!(
            "  worker id={} role={:?} account={} venue={}",
            worker.id,
            worker.role,
            worker.account_id.as_deref().unwrap_or("-"),
            worker.venue_id.as_deref().unwrap_or("-")
        );
    }
    println!("[配置 · 安全] 未读取密钥内容，仅检查引用名称和文件路径");
    Ok(())
}

pub(crate) fn run_config_fingerprint(path: &Path) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    println!(
        "[配置 · Fingerprint] path={} fingerprint={}",
        path.display(),
        config.fingerprint()?
    );
    Ok(())
}

pub(crate) fn run_config_lock(input: &Path, output: &Path, force: bool) -> Result<(), String> {
    let config = read_runtime_config(input)?;
    let fingerprint = config.fingerprint()?;
    if output.exists() && !force {
        return Err(format!(
            "目标发布配置已存在: {}；如确认覆盖，请显式添加 --force",
            output.display()
        ));
    }
    let mut locked = config;
    locked.config_fingerprint = Some(fingerprint.clone());
    let payload = locked.to_json()?;
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建发布配置目录失败 {}: {error}", parent.display()))?;
    }
    std::fs::write(output, payload)
        .map_err(|error| format!("写入发布配置失败 {}: {error}", output.display()))?;
    let verified = read_runtime_config(output)?;
    verified.verify_fingerprint()?;
    println!(
        "[配置 · Lock] input={} output={} fingerprint={} locked=true",
        input.display(),
        output.display(),
        fingerprint
    );
    Ok(())
}

pub(crate) fn run_unified_command(arguments: &[String]) -> Result<(), String> {
    let action = arguments
        .first()
        .map(String::as_str)
        .ok_or_else(|| run_usage("需要入口参数"))?;
    match action {
        "backtest" => {
            let positional: Vec<String> = arguments[1..]
                .iter()
                .filter(|value| !value.starts_with('-'))
                .cloned()
                .collect();
            // 收下却不处理等于对用户撒谎：旗标与多余位置参数都报用法，而不是静默丢弃。
            if arguments.len() - 1 > positional.len() || positional.len() > 3 {
                return Err(run_usage(
                    "backtest 不接受旗标，位置参数至多三个：<runtime.json> [bar-frame.json] [market-spec.json]",
                ));
            }
            run_unified_backtest(
                positional.first().map(PathBuf::from).as_deref(),
                positional.get(1).map(PathBuf::from).as_deref(),
                positional.get(2).map(PathBuf::from).as_deref(),
            )
        }
        "paper" => {
            let (path, _) = run_entry_arguments(arguments, action, false, || {
                repository_deploy_path("qianxing.runtime.paper-strategy.example.json")
            })?;
            run_paper_pipeline_once(&path)
        }
        "paper-check" => {
            let (path, json) = run_entry_arguments(arguments, action, true, || {
                repository_deploy_path("qianxing.runtime.paper-strategy.example.json")
            })?;
            run_paper_check(&path, json)
        }
        "doctor" => {
            // 数据面三入口共用 `unified_default_runtime_path`：缺省路径若与 `run backtest` 的
            // 写侧不同，链条就在这里断（见该函数的文档）。
            let (path, json) =
                run_entry_arguments(arguments, action, true, unified_default_runtime_path)?;
            run_doctor(&path, json)
        }
        "live-check" => {
            let (path, json) = run_entry_arguments(arguments, action, true, || {
                repository_deploy_path("qianxing.runtime.production.example.json")
            })?;
            run_live_check(&path, json)
        }
        "runtime-check" => {
            let (path, json) =
                run_entry_arguments(arguments, action, true, unified_default_runtime_path)?;
            run_runtime_check(&path, json)
        }
        "report" => run_unified_report_entry(arguments, unified_default_runtime_path),
        _ => Err(run_usage(&format!("不支持 {action}"))),
    }
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
    let data_dir = Path::new(&config.storage.data_dir).to_path_buf();
    let summaries = list_backtest_summary_paths(&data_dir.join("runs"))?;
    summaries
        .last()
        .cloned()
        .ok_or_else(|| format!("未找到回测摘要: {}", data_dir.join("runs").display()))
}

pub(crate) fn run_status(path: &Path, as_json: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let data_dir = Path::new(&config.storage.data_dir).to_path_buf();
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
            "runtime_version": build_identity::RUNTIME_VERSION,
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
        // 与 `report` 共用 `report_readout.rs` 的读法：没写的格子印 absent（V12 R1）。
        for line in latest_backtest_readout_lines(&summary) {
            println!("{line}");
        }
    } else {
        println!("[Latest Backtest] 暂无已保存回测摘要");
    }
    Ok(())
}

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
