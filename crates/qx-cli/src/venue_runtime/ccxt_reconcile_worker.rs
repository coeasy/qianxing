use crate::*;

pub(crate) fn run_ccxt_reconcile_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_root: &Path,
    pipeline_storage: PipelineStorage,
    ccxt_config_path: String,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    if worker.role != WorkerRole::Reconciler {
        return Err(format!("worker {} 不是 CCXT Reconciler worker", worker.id));
    }
    let account_id = worker
        .account_id
        .clone()
        .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))?;
    let venue_id = worker
        .venue_id
        .clone()
        .ok_or_else(|| format!("worker {} 缺少 venue_id", worker.id))?;
    let python = python_interpreter();
    let settlement_currency = worker
        .settlement_currency
        .clone()
        .unwrap_or_else(|| "USDT".into());
    let mut pipeline = pipeline_storage
        .open(ccxt_event_log_name(&worker), settlement_currency)
        .map_err(|error| format!("打开 CCXT 对账 EventLog 失败: {error}"))?;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!("ccxt reconcile polling venue={venue_id}"),
        Some(runtime_timestamp_ms()),
    )?;
    while !context.should_stop() {
        pipeline
            .refresh()
            .map_err(|error| format!("刷新 CCXT 对账 EventLog 失败: {error:?}"))?;
        let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
            .map_err(|error| format!("启动公共 CCXT Reconcile Worker 失败: {error}"))?;
        let balance_result = client
            .call(serde_json::json!({"op": "fetch_balance"}))
            .map_err(|error| format!("CCXT balance 对账失败: {error}"))?;
        let balances = ccxt_balance_facts(&balance_result)?;
        let received_ts = runtime_timestamp_ms();
        let mut source_seq = pipeline
            .log()
            .events()
            .last()
            .map(|event| event.source_seq)
            .unwrap_or(0);
        source_seq = source_seq.saturating_add(1);
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountBalanceSnapshot {
                    account_id: account_id.clone(),
                    venue_id: venue_id.clone(),
                    balances: balances.clone(),
                },
                received_ts,
                received_ts,
                source_seq,
                format!("{}:balances:{}", worker.id, received_ts),
            ))
            .map_err(|error| format!("CCXT 余额事实归约失败: {error:?}"))?;

        let cashflow_since_ms = pipeline
            .log()
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::AccountCashflow { .. } => Some(event.ts),
                _ => None,
            })
            .max();

        // 费率只是预估输入，真实资金费/利息/交割必须读取带外部 ID 的账单
        // 增量事实后才能进入 Ledger。优先使用统一 fetch_ledger；若交易所
        // 只暴露合约 funding history，则明确走该能力，不把缺失能力伪装成空账单。
        let mut cashflow_count = 0_usize;
        match client.call(serde_json::json!({
            "op": "fetch_ledger",
            "code": null,
            "since_ms": cashflow_since_ms,
        })) {
            Ok(cashflow_result) => {
                cashflow_count = cashflow_count.saturating_add(ingest_ccxt_cashflows(
                    &mut pipeline,
                    &cashflow_result,
                    &account_id,
                    &venue_id,
                    &worker.id,
                    received_ts,
                    &mut source_seq,
                )?);
            }
            Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {}
            Err(error) => return Err(format!("CCXT 资金流水对账失败: {error}")),
        }
        // 部分交易所的 fetch_ledger 不包含 funding 账单；合约市场再读取
        // 专用 funding history。与 ledger 同一 external_id 会由流水事实幂等去重。
        for instrument in &worker.symbols {
            match client.call(serde_json::json!({
                "op": "fetch_funding_history",
                "instrument": instrument,
                "since_ms": cashflow_since_ms,
            })) {
                Ok(history_result) => {
                    cashflow_count = cashflow_count.saturating_add(ingest_ccxt_cashflows(
                        &mut pipeline,
                        &history_result,
                        &account_id,
                        &venue_id,
                        &worker.id,
                        received_ts,
                        &mut source_seq,
                    )?);
                }
                Err(history_error)
                    if ccxt_error_is_optional_derivatives_capability(&history_error) => {}
                Err(history_error) => {
                    return Err(format!(
                        "CCXT 资金费账单对账失败 {instrument}: {history_error}"
                    ));
                }
            }
        }

        // 合约账户的持仓、保证金和未实现盈亏必须进入同一可恢复事实流。
        // 现货交易所通常不支持 fetch_positions；这类能力缺失只跳过该类快照，
        // 不能把“未支持”误判成空仓。
        let position_request = if worker.symbols.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::json!(worker.symbols)
        };
        match client.call(serde_json::json!({
            "op": "fetch_positions",
            "instruments": position_request,
        })) {
            Ok(position_result) => {
                let positions = ccxt_position_facts(&position_result, &venue_id)?;
                source_seq = source_seq.saturating_add(1);
                pipeline
                    .ingest(RuntimeEventEnvelope::venue(
                        RuntimeExternalEvent::AccountPositionSnapshot {
                            account_id: account_id.clone(),
                            venue_id: venue_id.clone(),
                            positions,
                        },
                        received_ts,
                        received_ts,
                        source_seq,
                        format!("{}:positions:{}", worker.id, received_ts),
                    ))
                    .map_err(|error| format!("CCXT 持仓事实归约失败: {error:?}"))?;
            }
            Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {}
            Err(error) => return Err(format!("CCXT 持仓对账失败: {error}")),
        }

        // 资金费率是风险输入，按 instrument 保存最新观察值；它不直接修改
        // Ledger，实际扣款已经由上面的 Cashflow 账单事实负责。
        for instrument in &worker.symbols {
            match client.call(serde_json::json!({
                "op": "fetch_funding_rate",
                "instrument": instrument,
            })) {
                Ok(funding_result) => {
                    let (snapshot, event_ts) = ccxt_funding_fact(&funding_result, &venue_id)?;
                    source_seq = source_seq.saturating_add(1);
                    pipeline
                        .ingest(RuntimeEventEnvelope::venue(
                            RuntimeExternalEvent::FundingRateSnapshot { snapshot },
                            event_ts,
                            received_ts,
                            source_seq,
                            format!("{}:funding:{}:{}", worker.id, instrument, received_ts),
                        ))
                        .map_err(|error| format!("CCXT 资金费率事实归约失败: {error:?}"))?;
                }
                Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {}
                Err(error) => {
                    return Err(format!("CCXT 资金费率对账失败 {instrument}: {error}"));
                }
            }
        }

        // 订单逐笔 fetch_order 只能覆盖本地已经知道的订单；进程在写入
        // Accepted 前崩溃、人工下单或其他系统下单，都会只存在于交易所。
        // 先拉取远端活动订单做只读发现，再交给下面的本地订单同步流程。
        let local_orders = pipeline.orders();
        let known_remote_orders = local_orders
            .iter()
            .filter_map(|order| {
                pipeline
                    .venue_order_id(order.client_id)
                    .map(|remote_id| (remote_id, (order.client_id, order.status)))
            })
            .collect::<BTreeMap<_, _>>();
        let open_order_requests = if worker.symbols.is_empty() {
            vec![None]
        } else {
            worker.symbols.iter().map(Some).collect::<Vec<_>>()
        };
        let mut open_order_issues = Vec::new();
        let mut seen_open_order_keys = BTreeSet::new();
        for instrument in open_order_requests {
            let open_orders_result = client.call(serde_json::json!({
                "op": "fetch_open_orders",
                "instrument": instrument,
            }));
            let open_orders_result = match open_orders_result {
                Ok(value) => value,
                Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => continue,
                Err(error) => return Err(format!("CCXT 活动订单对账失败: {error}")),
            };
            for issue in ccxt_open_order_issues(
                &open_orders_result,
                &local_orders,
                &known_remote_orders,
                &venue_id,
            )? {
                let key = format!(
                    "{}:{}",
                    issue
                        .get("remote_order_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown"),
                    issue
                        .get("instrument")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                );
                if seen_open_order_keys.insert(key) {
                    open_order_issues.push(issue);
                }
            }
        }

        let mut venue = CcxtProcessVenue::new(venue_id.clone(), Box::new(client));
        let orders = local_orders;
        let mut updates = 0_usize;
        for order in orders
            .into_iter()
            .filter(|order| !order.status.is_terminal())
        {
            let Some(remote_id) = pipeline.venue_order_id(order.client_id) else {
                EventLogReconcilePort::new(&mut pipeline, &worker.id, received_ts, &mut source_seq)
                    .with_tag("missing-remote")
                    .require_reconcile(order.client_id, "CCXT 对账缺少远端订单号")
                    .map_err(|error| format!("CCXT 缺失远端订单对账失败: {error}"))?;
                continue;
            };
            venue
                .restore_order(order.clone(), remote_id)
                .map_err(|error| format!("恢复 CCXT 本地订单失败: {error:?}"))?;
            match venue.sync_order(order.client_id, received_ts) {
                Ok(events) => {
                    updates += if let Some(spec) =
                        load_worker_instrument_spec(&worker, &order, Some(&runtime_config_path))?
                    {
                        ingest_venue_events_with_spec(
                            &mut pipeline,
                            events,
                            &worker.id,
                            received_ts,
                            &mut source_seq,
                            &spec,
                        )?
                    } else {
                        ingest_venue_events(
                            &mut pipeline,
                            events,
                            &worker.id,
                            received_ts,
                            &mut source_seq,
                        )?
                    };
                }
                Err(error) => {
                    let reason = format!("{error:?}");
                    EventLogReconcilePort::new(&mut pipeline, &worker.id, received_ts, &mut source_seq)
                        .with_tag("order-error")
                        .require_reconcile(order.client_id, &reason)
                        .map_err(|ingest_error| {
                            format!(
                                "CCXT 订单对账失败 {error:?}，且无法写入 ReconcileRequired: {ingest_error}"
                            )
                        })?;
                }
            }
        }
        let snapshot = pipeline.snapshot();
        let balance_discrepancies = pipeline
            .settlement_balance_discrepancies(&account_id, &venue_id, &balances)
            .map_err(|error| format!("CCXT 账户余额对账失败: {error:?}"))?;
        let position_snapshots_count = snapshot
            .account_positions
            .get(&(account_id.clone(), venue_id.clone()))
            .map(Vec::len)
            .unwrap_or(0);
        persist_reconcile_report(ReconcileReportInput {
            pipeline_root,
            worker_id: &worker.id,
            account_id: &account_id,
            venue_id: &venue_id,
            observed_ts: received_ts,
            issues: &[],
            additional_order_issues: &open_order_issues,
            balances_count: balances.len(),
            balance_discrepancies: &balance_discrepancies,
            position_snapshots_count,
            funding_rate_snapshots_count: snapshot.funding_rates.len(),
            cashflow_count,
        })?;
        context.heartbeat(received_ts)?;
        let service_status = if open_order_issues.is_empty() && balance_discrepancies.is_empty() {
            qx_runtime::ServiceStatus::Ready
        } else {
            qx_runtime::ServiceStatus::Degraded
        };
        context.mark(
            service_status,
            format!(
                "ccxt reconciled order_updates={updates} open_order_issues={} balance_discrepancies={}",
                open_order_issues.len(),
                balance_discrepancies.len()
            ),
            Some(received_ts),
        )?;
        if once {
            break;
        }
        thread::sleep(Duration::from_secs(5));
    }
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        "ccxt reconciler stopped",
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}
