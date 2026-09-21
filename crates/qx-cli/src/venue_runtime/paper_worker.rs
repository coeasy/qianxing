use crate::*;

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
    let log_name = required_account_event_log(&worker)?;
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
            let mut pipeline = open_account_pipeline(&runtime_config, &root, &log_name)
                .map_err(|error| format!("打开 Paper 多腿恢复 EventLog 失败: {error}"))?;
            let validator =
                recovery_order_validator(&worker, &pipeline, Some(&runtime_config_path));
            for message in recover_paper_spread_groups(
                &root,
                &mut pipeline,
                context.id(),
                now,
                Some(&validator),
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
    let log_name = required_account_event_log(&worker)?;
    let runtime_config = config.clone();
    let dedicated_spread_recovery = dedicated_spread_recovery_configured(&config, &worker);
    let supervisor = RuntimeSupervisor::new(config)?;
    let registered_id = worker.id.clone();
    let runtime_config_path = path.to_path_buf();
    let handle = supervisor.spawn_worker(&registered_id, move |context| {
        let mut pipeline = open_account_pipeline(&runtime_config, &root, &log_name)
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
                    // 跨腿屏障由 `qx-execution` 网关在写入任何事实之前自行判定，
                    // 这里只注入由存储根解析出的组快照。
                    let spread_store = open_spread_group_store(&root)?;
                    let mut market_pipeline =
                        open_account_pipeline(&runtime_config, &root, &log_name)
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
                    let risk_snapshot = if worker.instrument_spec_path.is_some() {
                        worker_risk_context(
                            &worker,
                            &order,
                            &market_pipeline,
                            Some(&runtime_config_path),
                        )?
                    } else {
                        None
                    };
                    let result = match risk_snapshot {
                        Some((risk, position)) => execute_paper_submit_effect(
                            &command,
                            &mut market_pipeline,
                            now,
                            Some(risk),
                            Some(position),
                            Some(market_quote),
                            false,
                            Some(&spread_store),
                        ),
                        None => Err(
                            "FAIL_CLOSED: Paper worker 缺少风控配置（instrument_spec_path），拒绝提交订单"
                                .to_string(),
                        ),
                    };
                    if is_fail_closed_rejection(&result) {
                        // 网关未写入任何事实：归约会把真正的 FAIL_CLOSED 文案覆盖成
                        // "订单组缺少 EventLog 订单"，因此这条路径直接保留原拒绝原因。
                        result
                    } else {
                        // 执行事实已写入同一个 pipeline，多腿订单组归约无需再打开一次日志。
                        sync_spread_group_after_order(&root, &market_pipeline, &command, now)?;
                        result
                    }
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
                let mut recovery_pipeline = open_account_pipeline(&runtime_config, &root, &log_name)
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
    let log_name = required_account_event_log(execution_worker)?;
    let mut market_pipeline = open_account_pipeline(&config, root, &log_name)
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
    let log_name = required_account_event_log(worker)?;
    let pipeline = open_account_pipeline(&config, root, &log_name)
        .map_err(|error| format!("打开 Paper 主链路 EventLog 失败: {error}"))?;
    if pipeline.orders().is_empty() || pipeline.ledger().entries().is_empty() {
        let control = configured_control_store(&config)?
            .load()
            .map_err(|error| format!("读取 Paper 主链路审计状态失败: {error}"))?;
        let recent: Vec<String> = control
            .audit()
            .iter()
            .rev()
            .take(3)
            .map(|record| format!("{:?}:{}", record.status, record.result_code))
            .collect();
        return Err(format!(
            "Paper 主链路验收未产生订单或 Ledger 事实；最近命令审计: {recent:?}"
        ));
    }
    let queue = configured_command_queue(&config, root)?;
    if !queue
        .pending_commands()
        .map_err(|error| format!("读取 Paper 主链路命令队列失败: {error:?}"))?
        .is_empty()
    {
        return Err("Paper 主链路验收结束后仍有未确认命令".into());
    }
    let control = configured_control_store(&config)?.load()?;
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
