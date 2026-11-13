//! Read-only runtime and backtest status projection.

use super::*;

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
