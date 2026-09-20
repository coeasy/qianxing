//! Paper venue 边界：初始资金播种、行情桥接、执行 worker 与主链路验收。

use super::*;

/// Paper execution 必须消费与市场数据相同的行情事实。
///
/// MarketData worker 自己的 EventLog 用于保留供应商来源与 BarFrame 快照，
/// Paper 账户则有独立的账户级 EventLog（订单、账簿、行情共同归约）。因此
/// 公共 CCXT/Binance 行情需要在这里显式镜像到匹配的 Paper 账户，而不是让
/// Paper execution worker 读取另一个 worker 的内存状态。镜像沿用来源 worker
/// 与 source sequence 组成幂等键；同一行情重试不会重复写入。
pub(crate) struct PaperMarketBridge {
    pub(crate) workers: Vec<WorkerConfig>,
    pub(crate) log_name: String,
    pub(crate) pipeline: LiveEventPipeline,
}

pub(crate) struct PaperMarketQuote<'a> {
    pub(crate) source_worker_id: &'a str,
    pub(crate) instrument: &'a InstrumentId,
    pub(crate) bid: Price,
    pub(crate) bid_qty: Quantity,
    pub(crate) ask: Price,
    pub(crate) ask_qty: Quantity,
    pub(crate) event_ts: u64,
    pub(crate) receive_ts: u64,
    pub(crate) source_seq: u64,
}

pub(crate) fn paper_market_worker_matches_instrument(
    worker: &WorkerConfig,
    instrument: &InstrumentId,
) -> bool {
    worker.enabled
        && worker.role == WorkerRole::Execution
        && worker
            .venue_id
            .as_deref()
            .is_some_and(|venue| venue.eq_ignore_ascii_case("paper"))
        && (worker.symbols.is_empty()
            || worker
                .symbols
                .iter()
                .any(|symbol| InstrumentId::parse(symbol).as_ref() == Some(instrument)))
}

pub(crate) fn open_paper_market_bridges(
    storage: &PipelineStorage,
    workers: &[WorkerConfig],
) -> Result<Vec<PaperMarketBridge>, String> {
    let mut bridges: Vec<PaperMarketBridge> = Vec::new();
    for worker in workers.iter().filter(|worker| {
        worker.enabled
            && worker.role == WorkerRole::Execution
            && worker
                .venue_id
                .as_deref()
                .is_some_and(|venue| venue.eq_ignore_ascii_case("paper"))
    }) {
        let log_name = format!(
            "paper-{}-{}-events",
            worker.account_id.as_deref().unwrap_or("unknown"),
            worker.venue_id.as_deref().unwrap_or("paper")
        );
        // 同一账户/venue 允许配置多个标的 worker，但它们共享一个账户级日志；
        // 合并 worker 的过滤条件，避免第一个 worker 的 symbols 遮蔽其它标的。
        if let Some(existing) = bridges
            .iter_mut()
            .find(|bridge| bridge.log_name == log_name)
        {
            existing.workers.push(worker.clone());
            continue;
        }
        let pipeline = storage.open(
            log_name.clone(),
            worker.settlement_currency.as_deref().unwrap_or("USDT"),
        )?;
        bridges.push(PaperMarketBridge {
            workers: vec![worker.clone()],
            log_name,
            pipeline,
        });
    }
    Ok(bridges)
}

pub(crate) fn bridge_market_quote_to_paper(
    bridges: &mut [PaperMarketBridge],
    quote: PaperMarketQuote<'_>,
) -> Result<usize, String> {
    let mut mirrored = 0;
    for bridge in bridges.iter_mut().filter(|bridge| {
        bridge
            .workers
            .iter()
            .any(|worker| paper_market_worker_matches_instrument(worker, quote.instrument))
    }) {
        bridge
            .pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                quote.instrument.clone(),
                QuoteTick::new(
                    quote.event_ts,
                    quote.bid,
                    quote.bid_qty,
                    quote.ask,
                    quote.ask_qty,
                    quote.source_seq,
                ),
                quote.receive_ts,
                quote.source_seq,
                format!(
                    "market-bridge:{}:{}:{}",
                    quote.source_worker_id, quote.instrument, quote.source_seq
                ),
            ))
            .map_err(|error| {
                format!(
                    "镜像公共行情到 Paper EventLog 失败 {} {}: {error:?}",
                    bridge.log_name, quote.instrument
                )
            })?;
        mirrored += 1;
    }
    Ok(mirrored)
}

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
    let currency = worker
        .settlement_currency
        .as_deref()
        .unwrap_or("USDT")
        .to_ascii_uppercase();
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
    let postgres_dsn = configured_postgres_dsn(&config)?;
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
        let mut pipeline = open_runtime_pipeline(&config, &root, "paper-events", "USDT")
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
    } else if let Some(worker) = paper_worker.as_ref() {
        let pipeline = open_runtime_pipeline(&config, &root, "paper-events", "USDT")
            .map_err(|error| format!("打开 Paper 风控 EventLog 失败: {error}"))?;
        let order = order_from_submit_command(&command)
            .map_err(|error| format!("Paper 订单载荷非法: {error:?}"))?;
        let (risk, position) = worker_risk_context(worker, &order, &pipeline, Some(path))?;
        execute_paper_submit_effect_with_storage_backend_and_pool(
            &command,
            &root,
            "paper-events",
            now,
            config.storage.event_log_segment_events,
            postgres_dsn.as_deref(),
            config.storage.postgres_pool_size,
            Some(risk),
            Some(position),
            Some(paper_fee_model(&config)?),
        )
    } else {
        eprintln!(
            "警告: 拓扑中没有启用的 paper execution worker，本次一次性 Paper 成交未经过账户级风控"
        );
        execute_paper_submit_effect_with_storage_backend_and_pool(
            &command,
            &root,
            "paper-events",
            now,
            config.storage.event_log_segment_events,
            postgres_dsn.as_deref(),
            config.storage.postgres_pool_size,
            None,
            None,
            Some(paper_fee_model(&config)?),
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

fn paper_submit_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
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

/// 独立的 Paper 多腿恢复 worker。它只扫描并推进 `HedgeRequired` 快照，
/// 不消费普通 SubmitOrder 队列，使恢复任务可以单独扩缩容、重启和观测。
pub(crate) fn run_paper_spread_recovery_worker(
    path: &Path,
    worker_id: &str,
    once: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    if !worker.enabled
        || worker.role != WorkerRole::SpreadRecovery
        || worker
            .venue_id
            .as_deref()
            .map(|venue| !venue.eq_ignore_ascii_case("paper"))
            .unwrap_or(true)
    {
        return Err(format!(
            "worker {worker_id} 不是启用的 Paper SpreadRecovery worker"
        ));
    }
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let log_name = format!(
        "paper-{}-{}-events",
        worker.account_id.as_deref().unwrap_or("unknown"),
        worker.venue_id.as_deref().unwrap_or("paper")
    );
    let runtime_config = config.clone();
    let runtime_config_path = path.to_path_buf();
    let supervisor = RuntimeSupervisor::new(config)?;
    let registered_id = worker.id.clone();
    let handle = supervisor.spawn_worker(&registered_id, move |context| {
        context.mark(
            qx_runtime::ServiceStatus::Ready,
            "paper spread recovery scanning",
            Some(runtime_timestamp_ms()),
        )?;
        while !context.should_stop() {
            let now = runtime_timestamp_ms();
            let mut pipeline = open_runtime_pipeline(&runtime_config, &root, &log_name, "USDT")
                .map_err(|error| format!("打开 Paper 多腿恢复 EventLog 失败: {error}"))?;
            let validator =
                recovery_order_validator(&worker, &pipeline, Some(&runtime_config_path));
            for message in recover_paper_spread_groups(
                &root,
                &mut pipeline,
                context.id(),
                now,
                Some(&validator),
                paper_fee_model(&runtime_config)?,
            )? {
                eprintln!("[HedgeRecovery] {message}");
            }
            context.heartbeat(now)?;
            if once {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    })?;
    handle
        .join()
        .map_err(|_| format!("Paper SpreadRecovery worker {worker_id} panic"))?
}

pub(crate) fn run_paper_execution_worker(
    path: &Path,
    worker_id: &str,
    once: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    if !worker.enabled
        || !matches!(
            worker.role,
            WorkerRole::Execution | WorkerRole::SpreadRecovery
        )
        || worker
            .venue_id
            .as_deref()
            .map(|venue| !venue.eq_ignore_ascii_case("paper"))
            .unwrap_or(true)
    {
        return Err(format!(
            "worker {worker_id} 不是启用的 Paper Execution worker"
        ));
    }
    if worker.role == WorkerRole::SpreadRecovery {
        return run_paper_spread_recovery_worker(path, worker_id, once);
    }
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let store = configured_control_store(&config)?;
    let queue = configured_command_queue(&config, &root)?;
    let log_name = format!(
        "paper-{}-{}-events",
        worker.account_id.as_deref().unwrap_or("unknown"),
        worker.venue_id.as_deref().unwrap_or("paper")
    );
    let segment_events = config.storage.event_log_segment_events;
    let runtime_config = config.clone();
    let dedicated_spread_recovery = dedicated_spread_recovery_configured(&config, &worker);
    let postgres_dsn = configured_postgres_dsn(&config)?;
    let supervisor = RuntimeSupervisor::new(config)?;
    let registered_id = worker.id.clone();
    let runtime_config_path = path.to_path_buf();
    let handle = supervisor.spawn_worker(&registered_id, move |context| {
        let mut pipeline = open_runtime_pipeline(&runtime_config, &root, &log_name, "USDT")
            .map_err(|error| format!("打开 Paper 初始资金 EventLog 失败: {error}"))?;
        seed_paper_initial_cash(&mut pipeline, &worker, runtime_timestamp_ms())?;
        context.mark(
            qx_runtime::ServiceStatus::Ready,
            "paper execution queue polling",
            Some(runtime_timestamp_ms()),
        )?;
        while !context.should_stop() {
            let now = runtime_timestamp_ms();
            let control = store.load()?;
            for command in control.pending().filter(|command| {
                matches!(command.kind, CommandKind::SubmitOrder)
                    && paper_submit_matches_worker(command, &worker)
            }) {
                queue
                    .enqueue_command(command.clone(), now)
                    .map_err(|error| format!("补入 Paper SubmitOrder 队列失败: {error:?}"))?;
            }
            let mut processed = 0_usize;
            for queued in queue
                .available_commands(now)
                .map_err(|error| format!("读取 Paper SubmitOrder 队列失败: {error:?}"))?
            {
                let command = queued.command.clone();
                if !paper_submit_matches_worker(&command, &worker) {
                    continue;
                }
                let lease = match queue.claim_command(command.command_id, context.id(), now, 30) {
                    Ok(lease) => lease,
                    Err(StorageError::LeaseHeld { .. }) => continue,
                    Err(error) => {
                        return Err(format!("领取 Paper SubmitOrder 租约失败: {error:?}"))
                    }
                };
                if command_is_final(&control, command.command_id) {
                    queue
                        .ack_command_at(command.command_id, context.id(), lease.fencing_token, now)
                        .map_err(|error| format!("清理已终态 Paper SubmitOrder 失败: {error:?}"))?;
                    continue;
                }
                let action = if command.dry_run {
                    Ok("DRY_RUN_VALIDATED".into())
                } else {
                    let market_pipeline =
                        open_runtime_pipeline(&runtime_config, &root, &log_name, "USDT")
                            .map_err(|error| format!("打开 Paper 行情 EventLog 失败: {error}"))?;
                    let order = order_from_submit_command(&command)
                        .map_err(|error| format!("Paper 订单载荷非法: {error:?}"))?;
                    let market_quote = market_pipeline
                        .latest_quote_with_depth(&order.instrument)
                        .ok_or_else(|| {
                        format!(
                            "Paper 订单 {} 缺少 {} 的最新行情事实，等待 MarketData 后重试",
                            order.client_id, order.instrument
                        )
                    })?;
                    let (risk, position) = worker_risk_context(
                        &worker,
                        &order,
                        &market_pipeline,
                        Some(&runtime_config_path),
                    )?;
                    let result =
                        execute_paper_submit_effect_with_storage_backend_and_pool_with_quote(
                            &command,
                            &root,
                            &log_name,
                            now,
                            segment_events,
                            postgres_dsn.as_deref(),
                            runtime_config.storage.postgres_pool_size,
                            Some(risk),
                            Some(position),
                            Some(market_quote),
                            Some(paper_fee_model(&runtime_config)?),
                        );
                    let latest_pipeline =
                        open_runtime_pipeline(&runtime_config, &root, &log_name, "USDT").map_err(
                            |error| format!("刷新 Paper 多腿订单组 EventLog 失败: {error}"),
                        )?;
                    sync_spread_group_after_order(&root, &latest_pipeline, &command)?;
                    result
                };
                let (_, record_result) = store
                    .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
                    .map_err(|error| format!("回写 Paper SubmitOrder 终态失败: {error}"))?;
                let record = record_result
                    .map_err(|error| format!("Paper SubmitOrder 执行失败: {error:?}"))?;
                queue
                    .ack_command_at(command.command_id, context.id(), lease.fencing_token, now)
                    .map_err(|error| format!("确认 Paper SubmitOrder 失败: {error:?}"))?;
                processed += 1;
                context.mark(
                    if record.status == qx_control::CommandStatus::Executed {
                        qx_runtime::ServiceStatus::Ready
                    } else {
                        qx_runtime::ServiceStatus::Degraded
                    },
                    format!(
                        "command_id={} status={:?}",
                        command.command_id, record.status
                    ),
                    Some(now),
                )?;
            }
            if !dedicated_spread_recovery {
                let mut recovery_pipeline =
                    open_runtime_pipeline(&runtime_config, &root, &log_name, "USDT")
                        .map_err(|error| format!("打开 Paper 多腿恢复 EventLog 失败: {error}"))?;
                let validator = recovery_order_validator(
                    &worker,
                    &recovery_pipeline,
                    Some(&runtime_config_path),
                );
                for message in recover_paper_spread_groups(
                    &root,
                    &mut recovery_pipeline,
                    context.id(),
                    now,
                    Some(&validator),
                    paper_fee_model(&runtime_config)?,
                )? {
                    eprintln!("[HedgeRecovery] {message}");
                }
            }
            context.heartbeat(now)?;
            if once {
                println!(
                    "[Paper · Execution] worker={} processed={}",
                    context.id(),
                    processed
                );
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    })?;
    handle
        .join()
        .map_err(|_| format!("Paper worker {worker_id} panic"))?
}

/// 按真实 worker 边界顺序执行一轮本地 Paper 主链路。
///
/// 该入口不是新的业务分支，而是把 Scheduler、Strategy 和 Execution 的
/// `--once` 验收顺序固定下来，便于 CI、部署检查和故障恢复测试复用。
pub(crate) fn run_paper_pipeline_once(path: &Path) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let scheduler_id = config
        .workers
        .iter()
        .find(|worker| worker.enabled && worker.role == WorkerRole::Scheduler)
        .map(|worker| worker.id.clone())
        .ok_or_else(|| "Paper 主链路缺少启用的 Scheduler worker".to_string())?;
    let strategy_id = config
        .workers
        .iter()
        .find(|worker| worker.enabled && worker.role == WorkerRole::Strategy)
        .map(|worker| worker.id.clone())
        .ok_or_else(|| "Paper 主链路缺少启用的 Strategy worker".to_string())?;
    let execution_id = config
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
        .map(|worker| worker.id.clone())
        .ok_or_else(|| "Paper 主链路缺少启用的 Paper Execution worker".to_string())?;

    run_scheduler_worker(path, &scheduler_id, true)?;
    run_strategy_worker(path, &strategy_id, true)?;

    // Paper 执行必须消费已经进入 EventLog 的市场事实。验收 fixture 不连接
    // 网络，因此在执行 worker 前显式注入一条可审计 L1 报价；真实部署由
    // MarketData worker 写入同一账户/运行时日志，不再使用固定价格兜底。
    let execution_worker = config
        .workers
        .iter()
        .find(|worker| worker.id == execution_id)
        .ok_or_else(|| "Paper Execution worker 配置在注入行情前消失".to_string())?;
    let root = Path::new(&config.storage.data_dir);
    let log_name = format!(
        "paper-{}-{}-events",
        execution_worker.account_id.as_deref().unwrap_or("unknown"),
        execution_worker.venue_id.as_deref().unwrap_or("paper")
    );
    let mut market_pipeline = open_runtime_pipeline(&config, root, &log_name, "USDT")
        .map_err(|error| format!("打开 Paper 行情 EventLog 失败: {error}"))?;
    let instrument = config
        .strategy
        .instrument
        .as_deref()
        .and_then(InstrumentId::parse)
        .ok_or_else(|| "Paper 验收策略缺少合法 instrument".to_string())?;
    let market_ts = runtime_timestamp_ms();
    market_pipeline
        .ingest(RuntimeEventEnvelope::market_quote(
            instrument,
            QuoteTick::new(
                market_ts,
                Price::from_i64(99),
                Quantity::from_i64(1_000),
                Price::from_i64(100),
                Quantity::from_i64(1_000),
                market_ts,
            ),
            market_ts,
            market_ts,
            format!("{log_name}:paper-check-market:{market_ts}"),
        ))
        .map_err(|error| format!("注入 Paper 验收行情失败: {error:?}"))?;
    run_paper_execution_worker(path, &execution_id, true)?;

    let root = Path::new(&config.storage.data_dir);
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == execution_id)
        .ok_or_else(|| "Paper Execution worker 配置在验收期间消失".to_string())?;
    let log_name = format!(
        "paper-{}-{}-events",
        worker.account_id.as_deref().unwrap_or("unknown"),
        worker.venue_id.as_deref().unwrap_or("paper")
    );
    let pipeline = open_runtime_pipeline(&config, root, log_name, "USDT")
        .map_err(|error| format!("打开 Paper 主链路 EventLog 失败: {error}"))?;
    if pipeline.orders().is_empty() || pipeline.ledger().entries().is_empty() {
        return Err("Paper 主链路验收未产生订单或 Ledger 事实".into());
    }
    let queue = ControlCommandQueue::new(root.join("control-queue"));
    if !queue
        .pending()
        .map_err(|error| format!("读取 Paper 主链路命令队列失败: {error:?}"))?
        .is_empty()
    {
        return Err("Paper 主链路验收结束后仍有未确认命令".into());
    }
    let control = load_control_state(root)?;
    let executed = control.audit().iter().any(|record| {
        record.status == qx_control::CommandStatus::Executed
            && record.command_id == pipeline.orders()[0].client_id
    });
    if !executed {
        return Err("Paper 主链路验收缺少 SubmitOrder Executed 审计记录".into());
    }
    println!(
        "[Paper · E2E] scheduler={} strategy={} execution={} orders={} ledger_entries={} ✓",
        scheduler_id,
        strategy_id,
        execution_id,
        pipeline.orders().len(),
        pipeline.ledger().entries().len()
    );
    Ok(())
}

pub(crate) fn paper_submit_order_command(argv: &[String]) {
    let path = argv
        .get(2)
        .cloned()
        .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
    let command_path = match argv.get(3).cloned() {
        Some(command_path) => command_path,
        None => {
            eprintln!("paper-submit-order 需要 command.json");
            std::process::exit(2);
        }
    };
    if let Err(error) = run_paper_submit_order(Path::new(&path), Path::new(&command_path)) {
        eprintln!("Paper SubmitOrder 执行失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn paper_worker_command(argv: &[String]) {
    let path = argv
        .get(2)
        .cloned()
        .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
    let worker_id = match argv.get(3).cloned() {
        Some(worker_id) => worker_id,
        None => {
            eprintln!("paper-worker 需要 worker-id");
            std::process::exit(2);
        }
    };
    let once = argv.iter().any(|argument| argument == "--once");
    if let Err(error) = run_paper_execution_worker(Path::new(&path), &worker_id, once) {
        eprintln!("Paper Execution worker 启动/运行失败: {error}");
        std::process::exit(2);
    }
}

/// `paper-e2e` 与 `paper-check` 是同一条主链路验收的两个入口名，实现只留一份：
/// 保留两个名字是为了兼容既有文档与 CI 调用，避免两份副本各自漂移。
pub(crate) fn paper_pipeline_command(argv: &[String]) {
    let path = argv
        .get(2)
        .cloned()
        .unwrap_or_else(|| "deploy/qianxing.runtime.paper-strategy.example.json".into());
    if let Err(error) = run_paper_pipeline_once(Path::new(&path)) {
        eprintln!("Paper 主链路验收失败: {error}");
        std::process::exit(2);
    }
}
