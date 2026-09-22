//! 配置与运维查询命令面：`config` / `status` / `doctor` / `report` / `live-check`，以及 `run` 的统一派发。
//!
//! 本模块只做配置读取、引用校验与人读/机读输出，不改变运行时语义；
//! 真实执行链路仍复用 crate 根上的同一批 Runtime/Storage 辅助函数。
//! 帮助文本本身不在这里，见 `cli_help.rs`（它与派发分支由架构门禁做集合相等校验）。
//! 项目初始化一族见 `init_project.rs`。

use super::*;

pub(crate) fn default_runtime_path() -> PathBuf {
    repository_deploy_path("qianxing.runtime.example.json")
}

pub(crate) fn run_config_explain(path: &Path, as_json: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    if as_json {
        // RuntimeConfig 只包含凭据引用（环境变量名/投影文件路径），不包含
        // secret 内容；输出可直接交给脚本或配置审计工具继续处理。
        println!("{}", config.to_json()?);
        return Ok(());
    }
    let strategy_count = if config.strategies.is_empty() {
        1
    } else {
        config.strategies.len()
    };
    println!("[配置 · 有效] path={}", path.display());
    println!(
        "  schema={} environment={} profile={:?}",
        config.schema_version, config.environment, config.profile
    );
    println!(
        "  storage={:?} data_dir={} api={} transport={:?}",
        config.storage.backend,
        // 与 doctor 同口径：相对 data_dir 有两个落点（可写运行态按进程当前目录，
        // 回测产物按 runtime.json 同级），报告真正在被使用的那个。
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

/// `run` 的可用入口清单：help 文案、缺参提示与"不支持"提示共用同一份。
/// 三处各写一遍正是 V10 §4.3 第 4 项"文案自称支持 backtest 而 match 没有该分支"的来源；
/// `tools/check_architecture.py` 会把本清单与 help 入口行、下面的 match 分支做集合相等校验。
pub(crate) const RUN_ENTRY_POINTS: [&str; 7] = [
    "backtest",
    "paper",
    "paper-check",
    "doctor",
    "live-check",
    "runtime-check",
    "report",
];

fn run_usage(phrase: &str) -> String {
    format!("run {phrase}；可用入口：{}", RUN_ENTRY_POINTS.join("、"))
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
        "paper" | "paper-check" => {
            let path = arguments.get(1).map(PathBuf::from).unwrap_or_else(|| {
                repository_deploy_path("qianxing.runtime.paper-strategy.example.json")
            });
            run_paper_pipeline_once(&path)
        }
        "doctor" => {
            let path = arguments
                .iter()
                .skip(1)
                .find(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(default_runtime_path);
            run_doctor(&path, arguments.iter().any(|argument| argument == "--json"))
        }
        "live-check" => {
            let path = arguments
                .iter()
                .skip(1)
                .find(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    repository_deploy_path("qianxing.runtime.production.example.json")
                });
            run_live_check(&path, arguments.iter().any(|argument| argument == "--json"))
        }
        "runtime-check" => {
            let path = arguments
                .iter()
                .skip(1)
                .find(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(default_runtime_path);
            run_runtime_check(&path, arguments.iter().any(|argument| argument == "--json"))
        }
        "report" => {
            let path = arguments
                .iter()
                .skip(1)
                .find(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(default_runtime_path);
            run_report(&path, arguments.iter().any(|argument| argument == "--json"))
        }
        _ => Err(run_usage(&format!("不支持 {action}"))),
    }
}

pub(crate) fn list_backtest_summary_paths(runs_dir: &Path) -> Result<Vec<PathBuf>, String> {
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
    // 报告不是只把摘要念一遍：摘要声明"跑的是哪一份输入"，这里就按它写的路径重读、重算，
    // 对不上直接拒绝出报告。旧 schema 没有 `input` 块，那是"这份产物没作过声明"，
    // 必须显式说出来而不是印一个 `-` 让人以为核对过了（V11 Q66 / Q1b）。
    let declared_input = recompute_declared_backtest_input(&summary)?;
    if as_json {
        let report = serde_json::json!({
            "schema_version": 1,
            "summary_path": summary_path.display().to_string(),
            "input_check": match &declared_input {
                Some(input) => serde_json::json!({
                    "verdict": "verified",
                    "declared_and_recomputed_match": true,
                    "kind": input.kind,
                    "path": input.path,
                    "dataset_id": input.dataset_id,
                    "dataset_version": input.dataset_version,
                    "fingerprint": input.fingerprint,
                }),
                None => serde_json::json!({
                    "verdict": "not_declared",
                    "declared_and_recomputed_match": false,
                }),
            },
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
        "  input_data_hash={} result_hash={} replay_log_digest={}",
        text("/input_data_hash", "-"),
        text("/result_hash", "-"),
        text("/replay/log_digest", "-")
    );
    match &declared_input {
        Some(input) => println!(
            "  input_verified={} input_kind={} input_id={} input_fingerprint={}",
            input.path, input.kind, input.dataset_id, input.fingerprint
        ),
        None => println!("  input_verified=not_declared（该摘要没有 input 块，输入身份未经核对）"),
    }
    println!(
        "  replay_events={} replay_ledger_entries={}/{}",
        integer("/replay/events"),
        integer("/replay/ledger_entries"),
        integer("/replay/run_ledger_entries")
    );
    println!(
        "  risk_rule_set_version={}",
        text("/risk_rules/rule_set_version", "-")
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
    check_account_log_settlement(&config, &mut checks, &mut failures);
    check_orphan_event_logs(path, &config, &mut checks, &mut warnings);

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
