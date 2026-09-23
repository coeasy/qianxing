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
    let models = load_api_query_models(config)?;
    state.job_runs = models.job_runs;
    state.ledger_entries = models.ledger_entries;
    state.reconcile_reports = models
        .reconcile_reports
        .into_iter()
        .map(|report| (report.worker_id.clone(), report))
        .collect();
    for (account_id, venue_id, log_name, currency) in configured_account_event_logs(config)? {
        let root = Path::new(&config.storage.data_dir);
        if event_log_exists(config, root, &log_name)? {
            let pipeline = open_runtime_pipeline(config, root, log_name, currency)
                .map_err(|error| format!("读取 API 事件投影失败: {error}"))?;
            state
                .project_account_event_log(&account_id, &venue_id, pipeline.log())
                .map_err(|error| format!("初始化 API 账户事件投影失败: {error}"))?;
        }
    }
    // 只有默认账户能占用无键端点读到的那格全局兼容快照；其余账户按身份进各自的投影，
    // 带 account_id/venue_id 的查询照旧读得到（V11 R12，与上面的 Ledger 共用同一个判据）。
    let default_log = default_account_event_log(config, Path::new(&config.storage.data_dir))?;
    for snapshot in load_api_account_snapshots(config)? {
        let identity =
            account_event_log_name(&snapshot.header.account_id, &snapshot.header.venue_id);
        let loaded = if identity.is_some() && identity == default_log {
            state.publish_snapshot(snapshot)
        } else {
            state.publish_snapshot_for(
                snapshot.header.account_id.clone(),
                snapshot.header.venue_id.clone(),
                snapshot,
            )
        };
        loaded.map_err(|error| format!("装载 API 账户查询快照失败: {error}"))?;
    }
    let mut policy = ApiPolicy::new();
    for (operator_id, operator) in &config.api.operators {
        policy = policy.grant(operator_id.clone(), operator.permission);
    }
    let metrics_dir = worker_metrics_dir(config);
    let worker_metrics_stale_after_ms = config.messaging.worker_stale_after_ms;
    let readiness_store = control_store.clone();
    let readiness_config = config.clone();
    // 三个只读运维端点的现读出口：与 boot 用同一份配置、同一个读点，差别只在"每次请求都读"。
    let query_models_config = config.clone();
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
    .with_query_models_provider(move || load_api_query_models(&query_models_config))
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

/// 无键端点（`/account/ledger`、不带查询串的 `/account/snapshot`）的"默认账户"：
/// 配置顺序里第一个**真有 EventLog** 的账户 worker。
///
/// 两处必须问同一个问题（V11 R12）：账簿此前取第一个账户 worker，全局兼容快照却被最后一个
/// 发布的账户覆盖，于是多账户部署下两个端点各讲一个账户，而调用方没有任何选账户的余地。
/// "第一个有日志的"而不是"第一个"：首账户尚未落盘时静默改读另一个账户，等于把 A 的账
/// 说成 B 的账。
pub(crate) fn default_account_event_log(
    config: &RuntimeConfig,
    root: &Path,
) -> Result<Option<String>, String> {
    for worker in config
        .workers
        .iter()
        .filter(|worker| owns_account_event_log(worker))
    {
        let Some(log_name) = worker_account_event_log(worker) else {
            continue;
        };
        if event_log_exists(config, root, &log_name)? {
            return Ok(Some(log_name));
        }
    }
    Ok(None)
}

/// 从持久化调度状态、账户 EventLog 和对账报告构造三份只读运维读模型。
///
/// 同一个读点服务两条路：启动时的那一次装载，以及 `/scheduler/runs`、`/account/ledger`、
/// `/reconcile/reports` 每次请求的现读（`with_query_models_provider`，V11 S3）。这些数据只进
/// QueryPort，API 不反向写 Scheduler、Ledger 或 Reconcile worker，查询层不是第二个事实拥有者。
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
    if let Some(log_name) = default_account_event_log(config, root)? {
        let pipeline = open_account_pipeline(config, root, &log_name)
            .map_err(|error| format!("读取 API Ledger 读模型失败: {error}"))?;
        ledger_entries = pipeline.ledger().entries().to_vec();
    }

    let reconcile_reports = load_reconcile_reports(root)?;
    Ok(qx_api::ApiQueryModels {
        job_runs,
        ledger_entries,
        reconcile_reports,
    })
}

/// 读取 `data_dir/reconcile/*.json`：对账 worker 每轮落一份报告，API 侧只读不写。
pub(crate) fn load_reconcile_reports(root: &Path) -> Result<Vec<ReconcileReportSnapshot>, String> {
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
    Ok(reconcile_reports)
}

/// 从已持久化的账户 EventLog 构造 API 查询快照。
///
/// 该快照只是 QueryPort 的读模型：所有余额、持仓、订单和成交仍由 EventLog
/// 重放得到，不会反向写入 Ledger，也不会把柜台观察当成交易事实。
pub(crate) fn load_api_account_snapshots(
    config: &RuntimeConfig,
) -> Result<Vec<AccountSnapshot>, String> {
    let reports = load_reconcile_reports(Path::new(&config.storage.data_dir))?;
    let mut seen = BTreeSet::new();
    let mut snapshots = Vec::new();
    for worker in config
        .workers
        .iter()
        .filter(|worker| owns_account_event_log(worker))
    {
        // 去重键取规范化后的账户身份（即日志名），不取配置原文：`main/paper` 与
        // `" main "/Paper` 是同一本账，按原文各投影一份就等于把同一账户报两次。
        let Some(identity) = worker_account_event_log(worker) else {
            continue;
        };
        if !seen.insert(identity) {
            continue;
        }
        if let Some(snapshot) = load_api_account_snapshot_for_worker(config, worker, &reports)? {
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
    let reports = load_reconcile_reports(Path::new(&config.storage.data_dir))?;
    for worker in config
        .workers
        .iter()
        .filter(|worker| owns_account_event_log(worker))
    {
        if let Some(snapshot) = load_api_account_snapshot_for_worker(config, worker, &reports)? {
            return Ok(Some(snapshot));
        }
    }
    Ok(None)
}

/// 把磁盘上的对账报告投影进账户快照的对账侧字段。
///
/// 每条对账链每轮覆盖写自己那一份 `reconcile/<worker-id>.json`，所以磁盘上的集合就是
/// "各链最新一轮"：本账户身份下的差异相加、观察时间戳取最晚。此前这两个字段在全仓没有
/// 任何写入点，报告里躺着待对账项时快照仍长期报"从未对账、零差异"。
fn apply_reconcile_reports(
    snapshot: &mut AccountSnapshot,
    log_name: &str,
    reports: &[ReconcileReportSnapshot],
) {
    // 报告与快照按同一本账的身份对上：`account_event_log_name` 是唯一的规范化点，
    // 这里不再自己发明一套 account/venue 比较口径。
    let mine = reports.iter().filter(|report| {
        account_event_log_name(&report.account_id, &report.venue_id).as_deref() == Some(log_name)
    });
    let last_reconcile_ts = mine.clone().map(|report| report.observed_ts).max();
    snapshot.reconcile.last_reconcile_ts = last_reconcile_ts;
    // 两格由同一个问题决定有没有：一份报告都没有时它们一起缺席，而不是留下一个读侧无法与
    // "对过且干净"区分的 0（V11 R10，与 Q67 的钱字段同一条纪律）。
    snapshot.reconcile.discrepancy_count = last_reconcile_ts.map(|_| {
        mine.fold(0_u32, |total, report| {
            let items = report
                .order_issues
                .len()
                .saturating_add(report.balance_discrepancies.len());
            total.saturating_add(u32::try_from(items).unwrap_or(u32::MAX))
        })
    });
}

pub(crate) fn load_api_account_snapshot_for_worker(
    config: &RuntimeConfig,
    worker: &WorkerConfig,
    reports: &[ReconcileReportSnapshot],
) -> Result<Option<AccountSnapshot>, String> {
    // 账簿键跟着日志身份的规范化走：用未 trim 的账户号查 Ledger 会读到空账簿，
    // 权益报 0 而没人报错。Venue 大小写不改，因为事实里的 venue 拼写由上报方决定。
    let account_id = worker.account_id.as_deref().unwrap_or_default().trim();
    let venue_id = worker.venue_id.as_deref().unwrap_or_default().trim();
    let Some(log_name) = account_event_log_name(account_id, venue_id) else {
        return Ok(None);
    };
    let root = Path::new(&config.storage.data_dir);
    if !event_log_exists(config, root, &log_name)? {
        return Ok(None);
    }
    let pipeline = open_account_pipeline(config, root, &log_name)
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
    // 等权的权益不是"可用资金"：持仓那段已压在标的上，抄 equity 等于宣布持仓可自由花掉。
    // 币种取本条快照记账的那一本，与上面的 cash/equity 同一口径；`LiveEventPipeline::open`
    // 已经拒绝空结算币种，所以这里没有"账簿读不出"的第三种状态。
    snapshot.available_raw = Some(
        pipeline
            .ledger()
            .cash_for(account_id, pipeline.settlement_currency()),
    );
    let fill_facts = pipeline
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
        .collect::<Vec<_>>();
    // 费用不用另找来源：同一条快照的逐笔成交已经带着 fee_raw。让账户级 fees 报 0，
    // 等于在同一份 JSON 里一边说"这些成交收了这么多费"、一边说"这账户费用为零"。
    // 溢出时宁可报"读不出这份快照"，也不发布一个回绕过的费用合计。
    let mut fees_raw = 0_i128;
    for (_, fill) in &fill_facts {
        fees_raw = fill.fee_raw.checked_add(fees_raw).ok_or_else(|| {
            format!("账户 {account_id} 的成交费用合计溢出，拒绝发布费用口径错误的快照")
        })?;
    }
    snapshot.fees_raw = Some(fees_raw);
    snapshot.orders = runtime_snapshot
        .orders
        .iter()
        .map(|order| (order.client_id, qx_protocol::OrderSnapshot::from(order)))
        .collect();
    snapshot.fills = fill_facts.into_iter().collect();

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
                // 这一行只由自己的账本拼出来：Ledger 里没有"未实现盈亏"和"占用的保证金"
                // 这两个量（它们要按冻结的 market spec 逐标的算，读侧没有规格来源），
                // 所以只能缺席。写死 0 会让一个上涨 5% 的现货仓位长期报着"没有浮亏"。
                unrealized_pnl_raw: None,
                margin_raw: None,
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
    apply_reconcile_reports(&mut snapshot, &log_name, reports);
    snapshot.reconcile.recovery_state = "eventlog-replayed".into();
    Ok(Some(snapshot))
}
