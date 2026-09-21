//! 配置化 API 服务：装配只读 API、加载查询模型与账户快照投影。
//!
//! 由 `main.rs` 的 crate 根职责簇拆出（Phase 4p），条目经根部的
//! `pub(crate) use api_service::*;` 再导出，行为与拆分前逐字相同。

use super::*;

pub(crate) fn build_configured_api_service(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
) -> Result<ApiService, String> {
    std::fs::create_dir_all(&config.storage.data_dir)
        .map_err(|error| format!("创建运行时 data_dir 失败: {error}"))?;
    let control_root = Path::new(&config.storage.data_dir).to_path_buf();
    let control_store = configured_control_store(config)?;
    let control = control_store.load()?;
    let command_queue = configured_command_queue(config, &control_root)?;
    let mut state = ApiState::default();
    state.control = control;
    let (job_runs, ledger_entries, reconcile_reports) = load_api_query_models(config)?;
    state.job_runs = job_runs;
    state.ledger_entries = ledger_entries;
    state.reconcile_reports = reconcile_reports
        .into_iter()
        .map(|report| (report.worker_id.clone(), report))
        .collect();
    for (account_id, venue_id, log_name, currency) in configured_account_event_logs(config) {
        let root = Path::new(&config.storage.data_dir);
        if event_log_exists(config, root, &log_name)? {
            let pipeline = open_runtime_pipeline(config, root, log_name, currency)
                .map_err(|error| format!("读取 API 事件投影失败: {error}"))?;
            state
                .project_account_event_log(&account_id, &venue_id, pipeline.log())
                .map_err(|error| format!("初始化 API 账户事件投影失败: {error}"))?;
        }
    }
    for snapshot in load_api_account_snapshots(config)? {
        state
            .publish_snapshot(snapshot)
            .map_err(|error| format!("装载 API 账户查询快照失败: {error}"))?;
    }
    let mut policy = ApiPolicy::new();
    for (operator_id, operator) in &config.api.operators {
        policy = policy.grant(operator_id.clone(), operator.permission);
    }
    let metrics_dir = worker_metrics_dir(config);
    let worker_metrics_stale_after_ms = config.messaging.worker_stale_after_ms;
    let readiness_store = control_store.clone();
    let readiness_config = config.clone();
    let readiness_runtime_config_path = runtime_config_path.to_path_buf();
    let readiness_metrics_dir = metrics_dir.clone();
    let service = if config.api.operators.is_empty() {
        ApiService::new(state)
    } else {
        ApiService::with_policy(state, policy)
    }
    .with_worker_metrics_provider(move || {
        read_worker_metrics(
            &metrics_dir,
            runtime_timestamp_ms(),
            worker_metrics_stale_after_ms,
        )
    })
    .with_readiness_provider(move || {
        configured_api_readiness(
            &readiness_config,
            &readiness_runtime_config_path,
            &readiness_store,
            &readiness_metrics_dir,
            runtime_timestamp_ms(),
            worker_metrics_stale_after_ms,
        )
    })
    .with_control_submitter({
        let store = control_store.clone();
        move |command, granted, ts| {
            if matches!(&command.kind, CommandKind::SubmitOrder) {
                order_from_submit_command(&command).map_err(|error| {
                    ControlSubmitError::Rejected(qx_control::ControlError::Invalid(format!(
                        "SubmitOrder 载荷非法: {error:?}"
                    )))
                })?;
            }
            let (plane, result) = store
                .transact(|plane| {
                    plane
                        .submit_as(command, granted, ts)
                        .map_err(ControlSubmitError::Rejected)
                })
                .map_err(ControlSubmitError::Unavailable)?;
            result.map(|audit| (plane, audit))
        }
    })
    .with_command_enqueuer({
        let queue = Arc::clone(&command_queue);
        move |command, ts| {
            queue
                .enqueue_command(command, ts)
                .map(|_| ())
                .map_err(|error| format!("写入控制命令队列失败: {error:?}"))
        }
    });
    if config.storage.backend == StorageBackend::Sqlite {
        #[cfg(not(feature = "sqlite"))]
        return Err("当前 qx-cli 未启用 sqlite feature；请使用 cargo run -p qx-cli --features sqlite -- serve ...".into());
        #[cfg(feature = "sqlite")]
        {
            let path = config
                .storage
                .sqlite_path
                .as_deref()
                .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
            let bucket = SqliteTokenBucket::new(path, "api", 100, 100)
                .map_err(|error| format!("初始化 SQLite API 限流失败: {error:?}"))?;
            let service = service.with_sqlite_rate_limit(bucket);
            return Ok(service);
        }
    }
    Ok(service)
}

/// 从持久化调度状态、账户 EventLog 和对账报告构造 API 读模型。
///
/// 这些数据只进入 QueryPort，不会被 API 反向写入 Scheduler、Ledger 或
/// Reconcile worker，避免查询层成为第二个事实拥有者。
pub(crate) type ApiQueryModels = (
    Vec<qx_scheduler::JobRun>,
    Vec<qx_core::LedgerEntry>,
    Vec<ReconcileReportSnapshot>,
);

pub(crate) fn load_api_query_models(config: &RuntimeConfig) -> Result<ApiQueryModels, String> {
    let root = Path::new(&config.storage.data_dir);
    let job_runs = {
        let state_path = runtime_path(root, &config.scheduler.state_path);
        if state_path.exists() {
            JsonStateStore::new(root)
                .load_scheduler_at(Path::new(&config.scheduler.state_path))
                .map_err(|error| format!("读取 API Scheduler 读模型失败: {error:?}"))?
                .runs()
        } else {
            Vec::new()
        }
    };

    let mut ledger_entries = Vec::new();
    if let Some(worker) = config
        .workers
        .iter()
        .find(|worker| owns_account_event_log(worker))
    {
        if let (Some(account_id), Some(venue_id)) =
            (worker.account_id.as_deref(), worker.venue_id.as_deref())
        {
            if let Some(log_name) = account_event_log_name(account_id, venue_id) {
                if event_log_exists(config, root, &log_name)? {
                    let pipeline = open_account_pipeline(config, root, &log_name)
                        .map_err(|error| format!("读取 API Ledger 读模型失败: {error}"))?;
                    ledger_entries = pipeline.ledger().entries().to_vec();
                }
            }
        }
    }

    let mut reconcile_reports = Vec::new();
    let report_root = root.join("reconcile");
    if report_root.exists() {
        for entry in std::fs::read_dir(&report_root)
            .map_err(|error| format!("读取 API 对账报告目录失败: {error}"))?
        {
            let path = entry
                .map_err(|error| format!("读取 API 对账报告目录项失败: {error}"))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let report: ReconcileReportSnapshot = serde_json::from_str(
                &std::fs::read_to_string(&path)
                    .map_err(|error| format!("读取对账报告失败 {}: {error}", path.display()))?,
            )
            .map_err(|error| format!("对账报告 JSON 无效 {}: {error}", path.display()))?;
            report
                .validate()
                .map_err(|error| format!("对账报告校验失败 {}: {error}", path.display()))?;
            reconcile_reports.push(report);
        }
        reconcile_reports.sort_by(|left, right| left.worker_id.cmp(&right.worker_id));
    }
    Ok((job_runs, ledger_entries, reconcile_reports))
}

/// 从已持久化的账户 EventLog 构造 API 查询快照。
///
/// 该快照只是 QueryPort 的读模型：所有余额、持仓、订单和成交仍由 EventLog
/// 重放得到，不会反向写入 Ledger，也不会把柜台观察当成交易事实。
pub(crate) fn load_api_account_snapshots(
    config: &RuntimeConfig,
) -> Result<Vec<AccountSnapshot>, String> {
    let mut seen = BTreeSet::new();
    let mut snapshots = Vec::new();
    for worker in config
        .workers
        .iter()
        .filter(|worker| owns_account_event_log(worker))
    {
        let key = (
            worker.account_id.clone().unwrap_or_default(),
            worker.venue_id.clone().unwrap_or_default(),
        );
        if !seen.insert(key) {
            continue;
        }
        if let Some(snapshot) = load_api_account_snapshot_for_worker(config, worker)? {
            snapshots.push(snapshot);
        }
    }
    Ok(snapshots)
}

/// 保留单快照兼容入口给旧 CLI 校验；生产 API 启动使用上面的多账户入口。
#[cfg(test)]
pub(crate) fn load_api_account_snapshot(
    config: &RuntimeConfig,
) -> Result<Option<AccountSnapshot>, String> {
    for worker in config
        .workers
        .iter()
        .filter(|worker| owns_account_event_log(worker))
    {
        if let Some(snapshot) = load_api_account_snapshot_for_worker(config, worker)? {
            return Ok(Some(snapshot));
        }
    }
    Ok(None)
}

pub(crate) fn load_api_account_snapshot_for_worker(
    config: &RuntimeConfig,
    worker: &WorkerConfig,
) -> Result<Option<AccountSnapshot>, String> {
    let account_id = worker.account_id.as_deref().unwrap_or_default();
    let venue_id = worker.venue_id.as_deref().unwrap_or_default();
    let Some(log_name) = account_event_log_name(account_id, venue_id) else {
        return Ok(None);
    };
    let root = Path::new(&config.storage.data_dir);
    if !event_log_exists(config, root, &log_name)? {
        return Ok(None);
    }
    let pipeline =
        open_runtime_pipeline(config, root, log_name, worker_settlement_currency(worker))
            .map_err(|error| format!("打开 API 账户 EventLog 失败: {error}"))?;
    let runtime_snapshot = pipeline.snapshot();
    let as_of = runtime_snapshot.last_engine_ts.max(1);
    let mut snapshot = AccountSnapshot::new(1, account_id, "default", venue_id, as_of);
    snapshot.header.event_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.seq)
        .unwrap_or(0);
    snapshot.cash_raw = pipeline.ledger().cash_balances_for(account_id);
    snapshot.equity_raw = pipeline
        .ledger()
        .equity_for(account_id, pipeline.marks(), pipeline.settlement_currency())
        .unwrap_or_else(|| {
            pipeline
                .ledger()
                .cash_for(account_id, pipeline.settlement_currency())
        });
    snapshot.available_raw = snapshot.equity_raw;
    snapshot.orders = runtime_snapshot
        .orders
        .iter()
        .map(|order| (order.client_id, qx_protocol::OrderSnapshot::from(order)))
        .collect();
    snapshot.fills = pipeline
        .log()
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Filled { fill } => Some((
                event.seq,
                qx_protocol::FillSnapshot::from_fact(event.seq, fill),
            )),
            _ => None,
        })
        .collect();

    let mut instruments = BTreeSet::new();
    instruments.extend(
        worker
            .symbols
            .iter()
            .filter_map(|symbol| InstrumentId::parse(symbol)),
    );
    instruments.extend(
        snapshot
            .orders
            .values()
            .map(|order| order.instrument.clone()),
    );
    instruments.extend(
        runtime_snapshot
            .account_positions
            .get(&(account_id.to_string(), venue_id.to_string()))
            .into_iter()
            .flat_map(|positions| positions.iter().map(|position| position.instrument.clone())),
    );
    for instrument in instruments {
        let position = pipeline.ledger().position_for(account_id, &instrument);
        let venue_position = runtime_snapshot
            .account_positions
            .get(&(account_id.to_string(), venue_id.to_string()))
            .and_then(|positions| positions.iter().find(|item| item.instrument == instrument));
        // 内核持仓观察 → 线格式只经由 `qx-protocol` 的唯一折算层；本函数不再
        // 手抄 `free/locked/…raw()` 折算，只补齐线格式没有的 Ledger 回退值。
        let mut wire = venue_position
            .map(qx_protocol::PositionSnapshot::from)
            .unwrap_or_else(|| qx_protocol::PositionSnapshot {
                instrument: instrument.clone(),
                quantity_raw: position.quantity.raw(),
                today_quantity_raw: position.quantity.raw(),
                average_price_raw: position.average_entry.raw(),
                mark_price_raw: pipeline
                    .marks()
                    .get(&instrument)
                    .map(|price| price.raw())
                    .unwrap_or(0),
                unrealized_pnl_raw: 0,
                margin_raw: 0,
            });
        if wire.quantity_raw == 0 && venue_position.is_none() {
            continue;
        }
        if wire.average_price_raw == 0 {
            wire.average_price_raw = position.average_entry.raw();
        }
        if wire.mark_price_raw == 0 {
            wire.mark_price_raw = pipeline
                .marks()
                .get(&instrument)
                .map(|price| price.raw())
                .unwrap_or(0);
        }
        snapshot.positions.insert(instrument.clone(), wire);
    }
    snapshot.reconcile.recovery_state = "eventlog-replayed".into();
    Ok(Some(snapshot))
}
