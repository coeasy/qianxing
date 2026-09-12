//! CLI Worker 实现边界。
//!
//! 这里保留 CLI 的配置/进程入口兼容性，但把 Scheduler 和 Strategy 的业务循环
//! 从 `main.rs` 抽出。具体执行仍复用 Runtime、Storage、Control 和 Execution
//! 公共契约，不在模块内创建第二套队列或订单状态。

use super::*;

pub(crate) fn run_scheduler_worker(path: &Path, worker_id: &str, once: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    if !worker.enabled || worker.role != WorkerRole::Scheduler {
        return Err(format!("worker {worker_id} 不是启用的 Scheduler worker"));
    }
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let queue = configured_job_queue(&config, &root)?;
    let (state_store, scheduler, state_path) = load_scheduler_state(&config, &root)?;
    let interval_ms = config.scheduler.tick_interval_ms;
    let supervisor = RuntimeSupervisor::new(config)?;
    let registered_id = worker.id.clone();
    let handle = supervisor.spawn_worker(&registered_id, move |context| {
        let scheduler = scheduler;
        let mut last_minute = None;
        context.mark(
            qx_runtime::ServiceStatus::Ready,
            format!("scheduler jobs={}", scheduler.len()),
            Some(runtime_timestamp_ms()),
        )?;
        loop {
            let now = runtime_timestamp_ms();
            let (trading_day, tick) = utc_schedule_tick(now);
            let minute_key = now / 60_000;
            if last_minute != Some(minute_key) {
                let manifest = scheduler_manifest(context.id(), &trading_day, now);
                let queued = dispatch_scheduled_jobs(
                    &state_store,
                    &state_path,
                    &queue,
                    &tick,
                    &trading_day,
                    &manifest,
                    now,
                )?;
                if queued > 0 {
                    println!(
                        "[调度 · Scheduler] worker={} day={} queued={}",
                        context.id(),
                        trading_day,
                        queued
                    );
                }
                last_minute = Some(minute_key);
            }
            context.heartbeat(now)?;
            if once {
                println!(
                    "[调度 · Scheduler] worker={} READY jobs={}",
                    context.id(),
                    scheduler.len()
                );
                break;
            }
            thread::sleep(Duration::from_millis(interval_ms.min(1_000)));
        }
        Ok(())
    })?;
    handle
        .join()
        .map_err(|_| format!("Scheduler worker {worker_id} panic"))?
}

pub(crate) fn run_strategy_worker(path: &Path, worker_id: &str, once: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    if !worker.enabled || worker.role != WorkerRole::Strategy {
        return Err(format!("worker {worker_id} 不是启用的 Strategy worker"));
    }
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let queue = configured_job_queue(&config, &root)?;
    let control_store = configured_control_store(&config)?;
    let command_queue = configured_command_queue(&config, &root)?;
    let (state_store, _, state_path) = load_scheduler_state(&config, &root)?;
    let mut strategy_config = config.strategy_for_worker(worker_id)?;
    resolve_strategy_runtime_paths(&mut strategy_config, path);
    verify_strategy_artifact(&strategy_config)?;
    let mut strategy_runtime_config = config.clone();
    // 将选中的实例投影到兼容的单策略执行路径，保留所有既有订单、目标仓位和审计逻辑。
    strategy_runtime_config.strategy = strategy_config.clone();
    let supervisor = RuntimeSupervisor::new(config)?;
    let registered_id = worker.id.clone();
    let handle = supervisor.spawn_worker(&registered_id, move |context| {
        let mut strategy = StrategyRuntime::new(
            context.id(),
            strategy_config.version.clone(),
            strategy_config.max_orders,
        );
        strategy
            .initialize()
            .map_err(|error| format!("策略初始化失败: {error:?}"))?;
        strategy
            .start()
            .map_err(|error| format!("策略启动失败: {error:?}"))?;
        let mut python_client = strategy_config
            .python_module
            .as_deref()
            .map(|module| {
                PythonStrategyClient::start_with_transport_config(
                    // ring 参数只在 shared_memory_json/shared_memory_columnar 生效，其余协议保持兼容。
                    module,
                    strategy_config.python_timeout_ms,
                    strategy_config.transport,
                    qx_strategy::SharedRingConfig {
                        capacity: strategy_config.shared_memory_capacity,
                        slot_bytes: strategy_config.shared_memory_slot_bytes,
                    },
                    strategy_config.strategy_artifact_sha256.as_deref(),
                )
            })
            .transpose()?;
        let mut external_client = strategy_config
            .external_executable
            .as_deref()
            .map(|executable| {
                StrategyProcessClient::start_process_with_transport_config(
                    executable,
                    &strategy_config.external_args,
                    &strategy_config.external_env,
                    strategy_config.python_timeout_ms,
                    "外部 Strategy",
                    strategy_config.transport,
                    qx_strategy::SharedRingConfig {
                        capacity: strategy_config.shared_memory_capacity,
                        slot_bytes: strategy_config.shared_memory_slot_bytes,
                    },
                )
            })
            .transpose()?;
        let mut native_client = if strategy_config.c_abi_library.is_some() {
            Some(load_c_abi_strategy(&strategy_config)?)
        } else {
            None
        };
        let mut native_initialized = false;
        context.mark(
            qx_runtime::ServiceStatus::Ready,
            format!("strategy version={}", strategy.version),
            Some(runtime_timestamp_ms()),
        )?;
        loop {
            let now = runtime_timestamp_ms();
            let control = control_store.load()?;
            for command in control.pending().filter(|command| {
                matches!(
                    command.kind,
                    CommandKind::PauseStrategy | CommandKind::ResumeStrategy
                ) && (command.target == context.id() || command.target == "*")
            }) {
                command_queue
                    .enqueue_command(command.clone(), now)
                    .map_err(|error| format!("补入 Strategy 控制命令队列失败: {error:?}"))?;
            }
            for queued in command_queue
                .available_commands(now)
                .map_err(|error| format!("读取 Strategy 控制命令队列失败: {error:?}"))?
            {
                let command = queued.command.clone();
                if !matches!(
                    command.kind,
                    CommandKind::PauseStrategy | CommandKind::ResumeStrategy
                ) || (command.target != context.id() && command.target != "*")
                {
                    continue;
                }
                let lease =
                    match command_queue.claim_command(command.command_id, context.id(), now, 30) {
                        Ok(lease) => lease,
                        Err(StorageError::LeaseHeld { .. }) => continue,
                        Err(error) => {
                            return Err(format!("领取 Strategy 控制命令租约失败: {error:?}"))
                        }
                    };
                if command_is_final(&control, command.command_id) {
                    command_queue
                        .ack_command_at(command.command_id, context.id(), lease.fencing_token, now)
                        .map_err(|error| format!("清理已终态 Strategy 控制命令失败: {error:?}"))?;
                    continue;
                }
                let action = if command.dry_run {
                    Ok("DRY_RUN_VALIDATED".into())
                } else {
                    match command.kind {
                        CommandKind::PauseStrategy => strategy
                            .pause()
                            .map(|_| "STRATEGY_PAUSED".to_string())
                            .map_err(|error| format!("策略暂停失败: {error:?}")),
                        CommandKind::ResumeStrategy => strategy
                            .start()
                            .map(|_| "STRATEGY_RESUMED".to_string())
                            .map_err(|error| format!("策略恢复失败: {error:?}")),
                        _ => Err("Strategy worker 不支持该控制命令".into()),
                    }
                };
                let (_, record_result) = control_store
                    .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
                    .map_err(|error| format!("回写 Strategy 控制命令终态失败: {error}"))?;
                record_result.map_err(|error| format!("Strategy 控制命令执行失败: {error:?}"))?;
                command_queue
                    .ack_command_at(command.command_id, context.id(), lease.fencing_token, now)
                    .map_err(|error| format!("确认 Strategy 控制命令失败: {error:?}"))?;
            }
            let mut processed = 0_usize;
            if matches!(strategy.state, qx_zhenlu::StrategyState::Running) {
                for queued in queue
                    .available(now)
                    .map_err(|error| format!("读取 Strategy JobQueue 失败: {error:?}"))?
                {
                    if queued.job.owner != context.id() && queued.job.owner != "*" {
                        continue;
                    }
                    let lease = match queue.claim(queued.run.run_id, context.id(), now, 30) {
                        Ok(lease) => lease,
                        Err(StorageError::LeaseHeld { .. }) => continue,
                        Err(error) => {
                            return Err(format!("领取 Strategy JobQueue 租约失败: {error:?}"))
                        }
                    };
                    let instrument = strategy_runtime_config
                        .strategy
                        .instrument
                        .as_deref()
                        .and_then(InstrumentId::parse)
                        .ok_or_else(|| "Strategy instrument 非法或未配置".to_string())?;
                    let (target_qty, contract_output) = if let Some(module) =
                        strategy_runtime_config.strategy.python_module.as_deref()
                    {
                        let input = build_strategy_contract_input(
                            &root,
                            &strategy_runtime_config,
                            &instrument,
                            &queued.run.run_id.to_string(),
                            now,
                        )?;
                        let output = if let Some(client) = python_client.as_mut() {
                            invoke_python_strategy_with_client(client, &input)?
                        } else {
                            invoke_python_strategy(module, &input)?
                        };
                        (output.target_qty, Some(output))
                    } else if let Some(client) = external_client.as_mut() {
                        let input = build_strategy_contract_input(
                            &root,
                            &strategy_runtime_config,
                            &instrument,
                            &queued.run.run_id.to_string(),
                            now,
                        )?;
                        let output = client.request(&input)?;
                        (output.target_qty, Some(output))
                    } else if let Some(client) = native_client.as_mut() {
                        let input = build_strategy_contract_input(
                            &root,
                            &strategy_runtime_config,
                            &instrument,
                            &queued.run.run_id.to_string(),
                            now,
                        )?;
                        let context =
                            native_strategy_context(&strategy_runtime_config.strategy, &input);
                        let event = qx_strategy::MarketEvent::Timer {
                            name: format!("job:{}", queued.run.run_id),
                            ts: now,
                        };
                        let output = invoke_c_abi_strategy(
                            client,
                            &mut native_initialized,
                            &context,
                            &input,
                            &event,
                        )?;
                        (output.target_qty, Some(output))
                    } else {
                        (
                            strategy_target_qty(&root, &strategy_runtime_config, &instrument, now)?,
                            None,
                        )
                    };
                    let orders = if let Some(output) = contract_output.as_ref() {
                        if output.intents.is_empty() {
                            let current_qty = strategy_current_qty_for(
                                &root,
                                &strategy_runtime_config,
                                &instrument,
                            )?;
                            build_strategy_order_with_signal(
                                &strategy_runtime_config,
                                context.id(),
                                queued.run.run_id,
                                now,
                                current_qty,
                                target_qty,
                                Some(output),
                            )?
                            .into_iter()
                            .collect::<Vec<_>>()
                        } else {
                            output
                                .intents
                                .iter()
                                .map(|intent| {
                                    let current_qty = strategy_current_qty_for(
                                        &root,
                                        &strategy_runtime_config,
                                        &InstrumentId::parse(&intent.instrument).ok_or_else(
                                            || {
                                                format!(
                                                    "Strategy intent instrument 非法: {}",
                                                    intent.instrument
                                                )
                                            },
                                        )?,
                                    )?;
                                    build_strategy_order_from_contract_intent(
                                        &strategy_runtime_config,
                                        context.id(),
                                        output.signal_id,
                                        intent,
                                        now,
                                        current_qty,
                                    )
                                })
                                .collect::<Result<Vec<_>, String>>()?
                        }
                    } else {
                        let current_qty =
                            strategy_current_qty_for(&root, &strategy_runtime_config, &instrument)?;
                        build_strategy_order_with_signal(
                            &strategy_runtime_config,
                            context.id(),
                            queued.run.run_id,
                            now,
                            current_qty,
                            target_qty,
                            None,
                        )?
                        .into_iter()
                        .collect::<Vec<_>>()
                    };
                    let mut results = Vec::new();
                    for order in orders {
                        strategy
                            .reserve_order()
                            .map_err(|error| format!("Strategy 订单配额拒绝: {error:?}"))?;
                        let command =
                            strategy_submit_command(context.id(), &order, queued.job.dry_run)?;
                        let order_result = if command.dry_run {
                            "DRY_RUN_SIGNAL_ORDER_INTENT_VALIDATED".to_string()
                        } else {
                            persist_strategy_submit(
                                &control_store,
                                command_queue.as_ref(),
                                &command,
                                now,
                            )?
                        };
                        results.push(order_result);
                    }
                    let result = if !results.is_empty() {
                        format!("{} orders: {}", results.len(), results.join(","))
                    } else if queued.job.dry_run {
                        "DRY_RUN_STRATEGY_TASK".to_string()
                    } else {
                        "STRATEGY_NO_REBALANCE".to_string()
                    };
                    state_store
                        .transact_scheduler_at(&state_path, |scheduler| {
                            scheduler
                                .finish_run_with_code(queued.run.run_id, true, Some(&result), now)
                                .map(|_| ())
                                .map_err(|error| format!("完成 JobRun 失败: {error:?}"))
                        })
                        .map_err(|error| format!("回写 JobRun 状态失败: {error:?}"))?
                        .1
                        .map_err(|error| format!("完成 JobRun 被拒绝: {error}"))?;
                    queue
                        .ack_at(queued.run.run_id, context.id(), lease.fencing_token, now)
                        .map_err(|error| format!("确认 Strategy JobQueue 失败: {error:?}"))?;
                    processed += 1;
                    println!(
                        "[策略 · Strategy] worker={} job={} run_id={} result={}",
                        context.id(),
                        queued.job.job_id,
                        queued.run.run_id,
                        result
                    );
                }
            }
            context.heartbeat(now)?;
            if once {
                println!(
                    "[策略 · Strategy] worker={} READY processed={}",
                    context.id(),
                    processed
                );
                break;
            }
            thread::sleep(Duration::from_millis(200));
        }
        Ok(())
    })?;
    handle
        .join()
        .map_err(|_| format!("Strategy worker {worker_id} panic"))?
}
