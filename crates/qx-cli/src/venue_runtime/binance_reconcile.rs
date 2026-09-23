use crate::*;

pub(crate) fn reconcile_issue_json(issue: &AdapterReconcileIssue) -> serde_json::Value {
    // kind 与单号复用适配器内唯一的一份枚举翻译（reason_code），这里只展开维度值。
    let mut value = serde_json::json!({
        "kind": issue.reason_code(),
        "client_order_id": issue.client_order_id(),
    });
    let object = value.as_object_mut().expect("json object");
    match issue {
        AdapterReconcileIssue::MissingLocally { .. }
        | AdapterReconcileIssue::MissingAtVenue { .. } => {}
        AdapterReconcileIssue::StatusMismatch { local, venue, .. } => {
            object.insert("local".into(), serde_json::json!(local));
            object.insert("venue".into(), serde_json::json!(venue));
        }
        AdapterReconcileIssue::FilledMismatch { local, venue, .. } => {
            object.insert("local_raw".into(), serde_json::json!(local.raw()));
            object.insert("venue_raw".into(), serde_json::json!(venue.raw()));
        }
    }
    value
}

pub(crate) struct ReconcileReportInput<'a> {
    pub(crate) pipeline_root: &'a Path,
    pub(crate) worker_id: &'a str,
    pub(crate) account_id: &'a str,
    pub(crate) venue_id: &'a str,
    pub(crate) observed_ts: u64,
    pub(crate) issues: &'a [AdapterReconcileIssue],
    pub(crate) additional_order_issues: &'a [serde_json::Value],
    pub(crate) balances_count: usize,
    pub(crate) balance_discrepancies: &'a [RuntimeBalanceDiscrepancy],
    /// `None` = 这条对账链本轮没有去取该项，`Some(0)` = 取了且为空（V11 Q69）。
    pub(crate) position_snapshots_count: Option<usize>,
    pub(crate) funding_rate_snapshots_count: Option<usize>,
    pub(crate) cashflow_count: Option<usize>,
}

pub(crate) fn persist_reconcile_report(input: ReconcileReportInput<'_>) -> Result<(), String> {
    let report = ReconcileReportSnapshot {
        schema_version: 1,
        worker_id: input.worker_id.into(),
        account_id: input.account_id.into(),
        venue_id: input.venue_id.into(),
        observed_ts: input.observed_ts,
        order_issues: input
            .issues
            .iter()
            .map(reconcile_issue_json)
            .chain(input.additional_order_issues.iter().cloned())
            .collect(),
        balances_count: input.balances_count,
        balance_discrepancies: input
            .balance_discrepancies
            .iter()
            .map(|value| serde_json::to_value(value).expect("balance discrepancy is serializable"))
            .collect(),
        position_snapshots_count: input.position_snapshots_count,
        funding_rate_snapshots_count: input.funding_rate_snapshots_count,
        cashflow_count: input.cashflow_count,
    };
    report.validate()?;
    JsonStateStore::new(input.pipeline_root)
        .save_json_at(format!("reconcile/{}.json", input.worker_id), &report)
        .map(|_| ())
        .map_err(|error| format!("保存对账报告失败: {error:?}"))
}

pub(crate) fn run_binance_reconcile_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_root: &Path,
    pipeline_storage: PipelineStorage,
    runtime_config_path: &Path,
    once: bool,
) -> Result<(), String> {
    let reconcile_symbols = worker
        .symbols
        .iter()
        .map(|symbol| {
            InstrumentId::parse(symbol)
                .filter(|instrument| instrument.venue.as_str().eq_ignore_ascii_case("BINANCE"))
                .map(|instrument| instrument.symbol)
                .ok_or_else(|| format!("worker {} 对账 symbol 非法: {symbol}", worker.id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut pipeline = pipeline_storage
        .open(
            binance_event_log_name(&worker)?,
            account_worker_currency_from_path(runtime_config_path, &worker)?,
        )
        .map_err(|error| format!("创建对账事件管线失败: {error}"))?;
    let mut source_seq = 0_u64;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        "reconciler starting",
        Some(runtime_timestamp_ms()),
    )?;
    while !context.should_stop() {
        // 每一轮重新读取凭据并创建连接器，使 Secret Manager 对投影文件的
        // 原子替换在下一轮对账生效；本轮请求仍使用同一认证上下文。
        let auth = load_binance_worker_auth(&worker)?;
        let mut venue = new_binance_venue(&worker, auth)?;
        venue
            .set_reconcile_symbols(reconcile_symbols.iter())
            .map_err(|error| format!("设置 Binance 对账 symbol 失败: {error:?}"))?;
        // fetch_and_reconcile 只允许在 Snapshotting 阶段执行；每一轮先显式
        // 进入对账态，避免把 Live 状态下的快照查询误当成已完成对账。
        pipeline
            .refresh()
            .map_err(|error| format!("刷新共享对账 EventLog 失败: {error:?}"))?;
        venue
            .restore_orders(pipeline.orders())
            .map_err(|error| format!("刷新对账订单状态失败: {error:?}"))?;
        venue.reconnect();
        let issues = venue
            .fetch_and_reconcile()
            .map_err(|error| format!("Binance 订单对账失败: {error:?}"))?;
        let balances = venue
            .fetch_account_balances(runtime_timestamp_ms())
            .map_err(|error| format!("Binance 账户余额同步失败: {error:?}"))?;
        let received_ts = runtime_timestamp_ms();
        let account_id = worker
            .account_id
            .clone()
            .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))?;
        let venue_id = worker
            .venue_id
            .clone()
            .ok_or_else(|| format!("worker {} 缺少 venue_id", worker.id))?;
        let balance_facts = balances
            .iter()
            .map(|balance| AccountBalance {
                asset: balance.asset.clone(),
                free: balance.free,
                locked: balance.locked,
                borrowed: Money::ZERO,
            })
            .collect::<Vec<_>>();
        let balance_discrepancies = pipeline
            .settlement_balance_discrepancies(&account_id, &venue_id, &balance_facts)
            .map_err(|error| format!("账户余额对账失败: {error:?}"))?;
        persist_reconcile_report(ReconcileReportInput {
            pipeline_root,
            worker_id: &worker.id,
            account_id: &account_id,
            venue_id: &venue_id,
            observed_ts: received_ts,
            issues: &issues,
            additional_order_issues: &[],
            balances_count: balances.len(),
            balance_discrepancies: &balance_discrepancies,
            // Binance Spot 这条链只取余额与订单：持仓 / 资金费率 / 账单三项从不查询，
            // 因此报"没取"而不是 0（写 0 等于替账户宣称"没有持仓、没有资金费"）。
            position_snapshots_count: None,
            funding_rate_snapshots_count: None,
            cashflow_count: None,
        })?;
        source_seq = source_seq.saturating_add(1);
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountBalanceSnapshot {
                    account_id: account_id.clone(),
                    venue_id: venue_id.clone(),
                    balances: balance_facts,
                },
                received_ts,
                received_ts,
                source_seq,
                format!("{}:balances:{}", worker.id, source_seq),
            ))
            .map_err(|error| format!("账户余额事实归约失败: {error:?}"))?;
        // 待对账事实的归类只复用适配器对裁决口径的投影（client_order_id/reason_code），
        // 调用点不复制差异判定分支。
        for issue in &issues {
            EventLogReconcilePort::new(&mut pipeline, &worker.id, received_ts, &mut source_seq)
                .require_reconcile(issue.client_order_id(), issue.reason_code())
                .map_err(|error| format!("对账事实归约失败: {error}"))?;
        }
        context.heartbeat(received_ts)?;
        let balance_detail = balance_discrepancies
            .iter()
            .map(|discrepancy| {
                format!(
                    "{}:{}->{}",
                    discrepancy.asset, discrepancy.ledger_raw, discrepancy.venue_raw
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let status = if issues.is_empty() && balance_discrepancies.is_empty() {
            qx_runtime::ServiceStatus::Ready
        } else {
            qx_runtime::ServiceStatus::Degraded
        };
        context.mark(
            status,
            format!(
                "reconcile_issues={} balances={} balance_discrepancies={}{}",
                issues.len(),
                balances.len(),
                balance_discrepancies.len(),
                if balance_detail.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", balance_detail)
                }
            ),
            Some(runtime_timestamp_ms()),
        )?;
        if once {
            break;
        }
        for _ in 0..300 {
            if context.should_stop() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}
