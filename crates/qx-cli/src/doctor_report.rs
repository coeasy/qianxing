//! doctor 的报告主体与它的存储侧检查（V11 R5-2）。
//!
//! 报告只组装、命令只排版，三颗存储侧检查也只读现成的落盘事实：不创建文件、不连数据库、
//! 不启动 worker。因此体检里"这条没扫"与"这条扫过且没问题"必须是两句话（V11 R6 的口径）——
//! 够不着的后端要说清够不着，不能省成一句通过。

use super::*;

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
    check_audit_chain(path, &config, &mut checks, &mut warnings, &mut failures);

    // 判的是"这份配置能否构建出监督器"（`new` = 已判过的 validate + 按启用 worker 注册
    // 服务），不是运行健康：此刻一个 worker 都没启动，原名 `runtime_topology: pass` 加
    // `overall=Starting` 会让读报告的人以为拓扑被判成了健康。
    let enabled_workers = config
        .workers
        .iter()
        .filter(|worker| worker.enabled)
        .count();
    let (status, message) = match RuntimeSupervisor::new(config.clone()) {
        Ok(_) => (
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

/// `storage.data_dir` 的两个候选落点：进程当前目录口径与 runtime 配置文件目录口径。
/// 两者可能是同一个目录，按 canonical 去重后再扫，避免重复报告。
fn storage_root_candidates(runtime_path: &Path, configured: &str) -> Vec<PathBuf> {
    let process_root = Path::new(configured).to_path_buf();
    let config_root = resolve_runtime_relative_path(runtime_path, configured);
    let mut seen = Vec::new();
    let mut roots = Vec::new();
    for root in [process_root, config_root] {
        let key = root.canonicalize().unwrap_or_else(|_| root.clone());
        if !seen.contains(&key) {
            seen.push(key);
            roots.push(root);
        }
    }
    roots
}

/// doctor 的 `storage.data_dir` 检查：可用性与提示按**两个**落点口径共同判断。
///
/// 只按其中一种折算会让 doctor 指向一个没有写入者使用的目录——真实账本已存在却被提示成
/// "将在首次运行时创建"，两处都还没创建时又会把可创建的落点误判成父目录不可用。报告给出
/// 真正在被使用的落点（[`effective_storage_root`]），并在两处都已落盘时提示配置分家。
pub(crate) fn check_storage_data_dir(
    runtime_path: &Path,
    configured: &str,
    checks: &mut Vec<serde_json::Value>,
    warnings: &mut Vec<String>,
    failures: &mut Vec<String>,
) {
    let process_root = Path::new(configured);
    let config_relative_root = resolve_runtime_relative_path(runtime_path, configured);
    let candidates = [process_root, config_relative_root.as_path()];
    let data_dir = effective_storage_root(runtime_path, configured);
    let canonical = candidates
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .collect::<Vec<_>>();
    if canonical.len() == 2 && canonical[0] != canonical[1] {
        let message = format!(
            "storage.data_dir 有两个已存在的落点: {}（进程目录口径，运行态写在这里）与 {}（配置目录口径，回测产物写在这里）；请固定启动目录或改用绝对路径，否则同一配置的账本与回测互不可见",
            process_root.display(),
            config_relative_root.display()
        );
        checks.push(serde_json::json!({
            "name": "storage.data_dir.split",
            "status": "warn",
            "message": message
        }));
        warnings.push(message);
    }
    if let Some(blocked) = candidates
        .iter()
        .find(|root| root.exists() && !root.is_dir())
    {
        let message = format!("storage.data_dir 不是目录: {}", blocked.display());
        checks.push(serde_json::json!({
            "name": "storage.data_dir",
            "status": "fail",
            "message": message
        }));
        failures.push(message);
    } else if candidates.iter().any(|root| root.is_dir()) {
        checks.push(serde_json::json!({
            "name": "storage.data_dir",
            "status": "pass",
            "message": data_dir.display().to_string()
        }));
    } else if candidates
        .iter()
        .any(|root| root.parent().is_some_and(|parent| parent.is_dir()))
    {
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
}

/// 从 EventLog 落盘文件名还原日志名：单文件 `{name}.json`，分段后端另有
/// `{name}.manifest.json`。只认 `-events` 结尾，`control-plane.json` 之类的
/// 相邻运行态文件不归这条检查管。
fn event_log_name_of(path: &Path) -> Option<String> {
    if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("json") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let name = stem.strip_suffix(".manifest").unwrap_or(stem);
    name.ends_with("-events").then(|| name.to_string())
}

/// doctor 的孤儿 EventLog 检查：`data_dir` 里没有任何身份引用的 `*-events` 账本。
///
/// 账户日志命名口径切到 `(account_id, venue_id)` 派生身份是硬切的：旧文件既不自动
/// 改名也不删除。让它静静躺在运行目录里，下一次只会以"这本账怎么不再增长"的形式被
/// 重新发现，而那时没人记得它属于切换前的哪一套名字。所以 doctor 点名，但只警告不
/// 失败——归档与否是运维决定，不是启动前置条件。
///
/// 扫描口径是 Files 后端的目录；其它后端没有可翻的目录，只能报告"未覆盖"，
/// 不能把缺席当成通过。
pub(crate) fn check_orphan_event_logs(
    runtime_path: &Path,
    config: &RuntimeConfig,
    checks: &mut Vec<serde_json::Value>,
    warnings: &mut Vec<String>,
) {
    if config.storage.backend != StorageBackend::Files {
        // 扫描靠翻 `data_dir` 目录，只对 Files 后端成立。这里必须留下检查记录：
        // 静默 return 让 doctor 的输出看起来"这项查过且干净"，非 Files 后端上那些
        // 没人引用的账本就此隐身。
        let backend = format!("{:?}", config.storage.backend).to_ascii_lowercase();
        let message = format!(
            "孤儿 EventLog 扫描只覆盖 files 后端，当前 backend={backend} 未扫描；\
             该后端的在册/遗留账本需按存储侧自行确认"
        );
        checks.push(serde_json::json!({
            "name": "event_logs.orphan",
            "status": "warn",
            "message": message
        }));
        warnings.push(message);
        return;
    }
    let owned = configured_event_log_names(config);
    let mut orphans = Vec::new();
    for root in storage_root_candidates(runtime_path, &config.storage.data_dir) {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            if let Some(name) = event_log_name_of(&path) {
                if !owned.contains(&name) {
                    orphans.push(path);
                }
            }
        }
    }
    orphans.sort();
    orphans.dedup();
    if orphans.is_empty() {
        checks.push(serde_json::json!({
            "name": "event_logs.orphan",
            "status": "pass",
            "message": "data_dir 中的 EventLog 都被当前配置引用"
        }));
        return;
    }
    let listed = orphans
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let message = format!(
        "{} 个 EventLog 没有被当前配置的任何身份引用: {listed}；账户日志按 (account_id, venue_id) \
         派生名字（见 deploy/README.md），请确认后归档或删除",
        orphans.len()
    );
    checks.push(serde_json::json!({
        "name": "event_logs.orphan",
        "status": "warn",
        "message": message
    }));
    warnings.push(message);
}

/// doctor 的审计链核对：读回整条链逐环重算，再确认控制面状态里的检查点正指着链尾。
///
/// 写入侧把"链先落、状态后落"绑在同一笔事务里（V11 R5-2），这两侧因此必须永远对齐；
/// 对不齐就是有人绕过事务边界写过状态，或链被截短、整段替换过。那不是运维提示而是事实
/// 错误，所以判 fail 而不是 warn——只有扫描覆盖不到的后端才降级成"未扫描"警告。
///
/// 覆盖口径与 `event_logs.orphan` 同源：能离线读的后端才扫。PostgreSQL 的链在库里，
/// 读它必须连库，而 doctor 的口径是绝不触网（`network_accessed:false`），所以只报告没扫，
/// 不能把"没扫"念成"扫过且干净"。
pub(crate) fn check_audit_chain(
    runtime_path: &Path,
    config: &RuntimeConfig,
    checks: &mut Vec<serde_json::Value>,
    warnings: &mut Vec<String>,
    failures: &mut Vec<String>,
) {
    let mut record = |status: &str, message: String| {
        checks.push(serde_json::json!({
            "name": "audit_chain",
            "status": status,
            "message": message.clone()
        }));
        match status {
            "warn" => warnings.push(message),
            "fail" => failures.push(message),
            _ => {}
        }
    };
    match config.storage.backend {
        StorageBackend::Postgres => {
            let message =
                "审计链核对要读 qx_audit_entries 与 qx_control_state，PostgreSQL 后端需连库、\
                 doctor 不触网故未扫描；请按存储侧自行校验链的连续性与检查点绑定"
                    .to_string();
            record("warn", message);
        }
        StorageBackend::Files => {
            let mut verified = 0usize;
            let mut broken = false;
            for root in storage_root_candidates(runtime_path, &config.storage.data_dir) {
                let state = JsonStateStore::new(root.clone()).load_control_if_exists();
                let chain = AuditFileStore::new(root.clone()).read_entries();
                let (plane, chain) = match (state, chain) {
                    (Ok(plane), Ok(chain)) => (plane, chain),
                    (Err(error), _) => {
                        broken = true;
                        record(
                            "fail",
                            format!("落点 {} 的控制面状态读不回来: {error:?}", root.display()),
                        );
                        continue;
                    }
                    (_, Err(error)) => {
                        broken = true;
                        record(
                            "fail",
                            format!("落点 {} 的审计链读不回来: {error:?}", root.display()),
                        );
                        continue;
                    }
                };
                let persisted_state = plane.is_some();
                let plane = plane.unwrap_or_default();
                if !persisted_state && chain.is_empty() {
                    // 这个落点还什么都没写过：不是断链，也没有可验的东西。
                    continue;
                }
                verified += 1;
                match verify_audit_chain(&plane, &chain) {
                    Ok(tip) => record(
                        "pass",
                        format!(
                            "落点 {} 的审计链 {} 条逐环连续，控制面检查点正指向链尾 {:016x}",
                            root.display(),
                            tip.entries,
                            tip.head_hash
                        ),
                    ),
                    Err(error) => {
                        broken = true;
                        record(
                            "fail",
                            format!("落点 {} 的审计链不可信: {error:?}", root.display()),
                        )
                    }
                }
            }
            if verified == 0 && !broken {
                record(
                    "pass",
                    "data_dir 的两个落点都还没有控制面状态与审计链（首次受理命令前为空）"
                        .to_string(),
                );
            }
        }
        StorageBackend::Sqlite => {
            #[cfg(not(feature = "sqlite"))]
            {
                let _ = runtime_path;
                record(
                    "warn",
                    "当前 qx-cli 未启用 sqlite feature，审计链未扫描；请用 --features sqlite 构建后核对"
                        .to_string(),
                );
            }
            #[cfg(feature = "sqlite")]
            {
                // 路径口径与写入侧一致：`configured_control_store` 原样使用 `sqlite_path`，
                // 这里也原样打开，不做第二种折算——否则 doctor 验的可能是另一本账。
                let Some(configured) = config.storage.sqlite_path.as_deref() else {
                    record(
                        "fail",
                        "SQLite backend 缺少 sqlite_path，审计链无处可读".to_string(),
                    );
                    return;
                };
                let path = Path::new(configured);
                if !path.exists() {
                    record(
                        "pass",
                        format!(
                            "sqlite 数据文件 {} 尚未创建，暂无审计链可验",
                            path.display()
                        ),
                    );
                    return;
                }
                let chain = match SqliteAuditStore::new(path).and_then(|store| store.read_entries())
                {
                    Ok(chain) => chain,
                    Err(error) => {
                        record(
                            "fail",
                            format!("审计链读不回来: {error:?}（{}）", path.display()),
                        );
                        return;
                    }
                };
                let plane = match configured_control_store(config).and_then(|store| store.load()) {
                    Ok(plane) => plane,
                    Err(error) => {
                        record("fail", format!("控制面状态读不回来: {error}"));
                        return;
                    }
                };
                match verify_audit_chain(&plane, &chain) {
                    Ok(tip) => record(
                        "pass",
                        format!(
                            "审计链 {} 条逐环连续，控制面检查点正指向链尾 {:016x}",
                            tip.entries, tip.head_hash
                        ),
                    ),
                    Err(error) => record(
                        "fail",
                        format!("审计链不可信: {error:?}（{}）", path.display()),
                    ),
                }
            }
        }
    }
}
