use crate::*;

/// 为 Paper 账户写入一次可恢复、可幂等的初始资金事实。
/// 资金只通过 AccountCashflow 进入 Ledger，不直接修改内存余额。
pub(crate) fn seed_paper_initial_cash(
    pipeline: &mut LiveEventPipeline,
    worker: &WorkerConfig,
    now: u64,
) -> Result<(), String> {
    let Some(amount_raw) = worker.paper_initial_cash_raw else {
        return Ok(());
    };
    let account_id = worker
        .account_id
        .as_deref()
        .ok_or_else(|| format!("Paper worker {} 缺少 account_id", worker.id))?;
    let currency = worker_settlement_currency(worker);
    let external_id = format!(
        "paper-initial-cash:{}:{}:{}",
        worker.id, account_id, currency
    );
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: account_id.into(),
                    venue_id: "paper".into(),
                    currency,
                    kind: CashflowKind::Transfer,
                    amount: Money::from_raw(amount_raw),
                    external_id: external_id.clone(),
                },
            },
            now,
            now,
            0,
            external_id,
        ))
        .map_err(|error| format!("写入 Paper 初始资金失败: {error:?}"))?;
    Ok(())
}

/// 使用同一控制面/队列/EventLog 语义跑一笔完全本地的 Paper 下单闭环。
/// 该入口用于验收执行编排，不连接网络，也不把 Paper 结果当作真实 Venue 结果。
pub(crate) fn run_paper_submit_order(path: &Path, command_path: &Path) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let command: ControlCommand = serde_json::from_str(
        &std::fs::read_to_string(command_path)
            .map_err(|error| format!("读取 Paper SubmitOrder 命令失败: {error}"))?,
    )
    .map_err(|error| format!("Paper SubmitOrder 命令 JSON 无效: {error}"))?;
    order_from_submit_command(&command)
        .map_err(|error| format!("Paper SubmitOrder 订单载荷非法: {error:?}"))?;
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let store = configured_control_store(&config)?;
    let queue = configured_command_queue(&config, &root)?;
    let now = runtime_timestamp_ms();
    let paper_worker = config
        .workers
        .iter()
        .find(|worker| {
            worker.enabled
                && worker.role == WorkerRole::Execution
                && worker
                    .venue_id
                    .as_deref()
                    .is_some_and(|venue| venue.eq_ignore_ascii_case("paper"))
        })
        .cloned();
    if let Some(worker) = paper_worker.as_ref() {
        let mut pipeline = open_runtime_pipeline(
            &config,
            &root,
            "paper-events",
            worker_settlement_currency(worker),
        )
        .map_err(|error| format!("打开 Paper 初始资金 EventLog 失败: {error}"))?;
        seed_paper_initial_cash(&mut pipeline, worker, now)?;
    }
    let (_, accepted_result) = store
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, now))
        .map_err(|error| format!("持久化 Paper SubmitOrder Accepted 失败: {error}"))?;
    let accepted = accepted_result
        .map_err(|error| format!("Paper SubmitOrder 未通过控制面校验: {error:?}"))?;
    queue
        .enqueue_command(command.clone(), now)
        .map_err(|error| format!("写入 Paper SubmitOrder 队列失败: {error:?}"))?;
    let lease = queue
        .claim_command(command.command_id, "paper-execution", now, 30)
        .map_err(|error| format!("领取 Paper SubmitOrder 租约失败: {error:?}"))?;

    let action = if command.dry_run {
        Ok("DRY_RUN_VALIDATED".into())
    } else if let Some(worker) = paper_worker
        .as_ref()
        .filter(|worker| worker.instrument_spec_path.is_some())
    {
        // 跨腿屏障已下沉 `qx-execution` 网关：这里只把存储根解析出的组快照注入提交，
        // 带 `spread_group_id` 的腿若拿不到组存储会在网关内以 FAIL_CLOSED 被拒绝。
        let spread_store = open_spread_group_store(&root)?;
        let mut pipeline = open_runtime_pipeline(
            &config,
            &root,
            "paper-events",
            worker_settlement_currency(worker),
        )
        .map_err(|error| format!("打开 Paper 风控 EventLog 失败: {error}"))?;
        let order = order_from_submit_command(&command)
            .map_err(|error| format!("Paper 订单载荷非法: {error:?}"))?;
        let (risk, position) = worker_risk_context(worker, &order, &pipeline, Some(path))?
            .ok_or_else(|| "Paper worker 风控配置缺少 RiskContext".to_string())?;
        // fail-closed：撮合行情必须来自同一 EventLog 的行情事实，缺行情拒绝执行。
        let market_quote = pipeline
            .latest_quote_with_depth(&order.instrument)
            .ok_or_else(|| {
                format!(
                    "FAIL_CLOSED: Paper SubmitOrder 缺少 {} 的最新行情事实，等待 MarketData worker 注入后重试",
                    order.instrument
                )
            })?;
        execute_paper_submit_effect(
            &command,
            &mut pipeline,
            now,
            Some(risk),
            Some(position),
            Some(market_quote),
            false,
            Some(&spread_store),
        )
    } else {
        Err(
            "FAIL_CLOSED: Paper worker 缺少风控配置（instrument_spec_path），拒绝提交订单"
                .to_string(),
        )
    };
    let (_, record_result) = store
        .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
        .map_err(|error| format!("持久化 Paper SubmitOrder 终态失败: {error}"))?;
    let record = record_result.map_err(|error| format!("Paper 执行控制命令失败: {error:?}"))?;
    queue
        .ack_command_at(
            command.command_id,
            "paper-execution",
            lease.fencing_token,
            now,
        )
        .map_err(|error| format!("确认 Paper SubmitOrder 队列失败: {error:?}"))?;
    println!(
        "[Paper · SubmitOrder] accepted={:?} final={:?} command_id={} result={}",
        accepted.status, record.status, command.command_id, record.result_code
    );
    if record.status == qx_control::CommandStatus::Failed {
        return Err(record.result_code);
    }
    Ok(())
}

pub(crate) fn paper_submit_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
    let expected_account = worker.account_id.as_deref().unwrap_or_default();
    order_from_submit_command(command)
        .map(|order| {
            order.account_id == expected_account
                && worker
                    .venue_id
                    .as_deref()
                    .map(|venue| venue.eq_ignore_ascii_case("paper"))
                    .unwrap_or(false)
                // Paper 是虚拟执行域，订单 instrument 可以来自 Binance、OKX、
                // Bybit 或自定义市场；不能把虚拟账户误绑死在某一个真实 Venue。
                && (worker.symbols.is_empty()
                    || worker.symbols.iter().any(|symbol| {
                        InstrumentId::parse(symbol)
                            .as_ref()
                            == Some(&order.instrument)
                    }))
        })
        .unwrap_or(false)
}
