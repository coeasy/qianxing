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
    let (state_store, scheduler, state_path) = load_scheduler_state(&config, &root, path)?;
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
            // 生产循环里唯二不读停机令牌的两次（V11 S2）：其余十处 worker 循环都以
            // `context.should_stop()` 收口，`once` 之外的唯一出口是 `?` 抛错。
            if context.should_stop() {
                break;
            }
            let now = runtime_timestamp_ms();
            let (trading_day, tick) = utc_schedule_tick(now);
            let minute_key = now / 60_000;
            if last_minute != Some(minute_key) {
                let manifest = scheduler_manifest(context.id(), &trading_day, now);
                let dispatch = dispatch_scheduled_jobs(
                    &state_store,
                    &state_path,
                    &queue,
                    &tick,
                    &trading_day,
                    &manifest,
                    now,
                )?;
                if dispatch.queued > 0 || dispatch.timed_out > 0 || dispatch.skipped > 0 {
                    println!(
                        "[调度 · Scheduler] worker={} day={} queued={} timed_out={} skipped={}",
                        context.id(),
                        trading_day,
                        dispatch.queued,
                        dispatch.timed_out,
                        dispatch.skipped
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
    join_worker_handle(&supervisor, handle, "Scheduler", worker_id)
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
    // 本 worker 自己不碰 Venue，但它把生成的每一笔 SubmitOrder 投进队列：A 股段配在这里
    // 却不改变它生成的订单（T+1 可售量、整手都在生成时成立），等于让配置说假话。
    // 因此按提交入口同一道闸门当场拒（V12 R1 §4.20）。
    reject_ashare_rules_on_submit_path(path, None, "strategy-worker")?;
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let queue = configured_job_queue(&config, &root)?;
    let control_store = configured_control_store(&config)?;
    let command_queue = configured_command_queue(&config, &root)?;
    let (state_store, _, state_path) = load_scheduler_state(&config, &root, path)?;
    let mut strategy_config = config.strategy_for_worker(worker_id)?;
    resolve_strategy_runtime_paths(&mut strategy_config, path);
    verify_strategy_artifact(&strategy_config)?;
    let mut strategy_runtime_config = config.clone();
    // 将选中的实例投影到兼容的单策略执行路径，保留所有既有订单、目标仓位和审计逻辑。
    strategy_runtime_config.strategy = strategy_config.clone();
    let live_digest_path = live_strategy_digest_state_path(&root, worker_id);
    let persisted_live_digest = load_live_strategy_digest(&live_digest_path)?;
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
                    None,
                )
            })
            .transpose()?;
        let mut native_client = if strategy_config.c_abi_library.is_some() {
            Some(load_c_abi_strategy(&strategy_config)?)
        } else {
            None
        };
        let mut native_initialized = false;
        let mut last_live_digest = persisted_live_digest;
        context.mark(
            qx_runtime::ServiceStatus::Ready,
            format!("strategy version={}", strategy.version),
            Some(runtime_timestamp_ms()),
        )?;
        loop {
            // 与其余 worker 循环同一口径：停机令牌是唯一"非错误"的出口（V11 S2）。
            // 缺了它，Scheduler 与 Strategy 是生产里唯二永远不会自然结束的循环。
            if context.should_stop() {
                break;
            }
            let now = runtime_timestamp_ms();
            // 命令队列 / 作业队列的租约与 JobRun 状态都在秒域，见 `lease_clock`。
            let lease_now = lease_clock(now);
            let control = control_store.load()?;
            for command in control.pending().filter(|command| {
                matches!(
                    command.kind,
                    CommandKind::PauseStrategy | CommandKind::ResumeStrategy
                ) && (command.target == context.id() || command.target == "*")
            }) {
                command_queue
                    .enqueue_command(command.clone(), lease_now)
                    .map_err(|error| format!("补入 Strategy 控制命令队列失败: {error:?}"))?;
            }
            for queued in command_queue
                .available_commands(lease_now)
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
                let lease = match command_queue
                    .claim_command(command.command_id, context.id(), lease_now, 30)
                {
                    Ok(lease) => lease,
                    Err(StorageError::LeaseHeld { .. }) => continue,
                    Err(error) => {
                        return Err(format!("领取 Strategy 控制命令租约失败: {error:?}"))
                    }
                };
                if command_is_final(&control, command.command_id) {
                    command_queue
                        .ack_command_at(
                            command.command_id,
                            context.id(),
                            lease.fencing_token,
                            lease_now,
                        )
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
                    .ack_command_at(
                        command.command_id,
                        context.id(),
                        lease.fencing_token,
                        lease_now,
                    )
                    .map_err(|error| format!("确认 Strategy 控制命令失败: {error:?}"))?;
            }
            let mut processed = 0_usize;
            if matches!(strategy.state, qx_zhenlu::StrategyState::Running) {
                if let Some(data_fingerprint) =
                    live_strategy_snapshot_digest(&strategy_runtime_config.strategy, now)?
                {
                    if last_live_digest != Some(data_fingerprint) {
                        let (job, run) = live_strategy_job(
                            &strategy_runtime_config.strategy,
                            context.id(),
                            data_fingerprint,
                            now,
                            &strategy_runtime_config.environment,
                        );
                        queue.enqueue(job, run, lease_now).map_err(|error| {
                            format!("写入实时 Strategy JobQueue 失败: {error:?}")
                        })?;
                        save_live_strategy_digest(&live_digest_path, data_fingerprint)?;
                        last_live_digest = Some(data_fingerprint);
                    }
                }
                for queued in queue
                    .available(lease_now)
                    .map_err(|error| format!("读取 Strategy JobQueue 失败: {error:?}"))?
                {
                    if queued.job.owner != context.id() && queued.job.owner != "*" {
                        continue;
                    }
                    let lease =
                        match queue.claim(queued.run.run_id, context.id(), lease_now, 30) {
                        Ok(lease) => lease,
                        Err(StorageError::LeaseHeld { .. }) => continue,
                        Err(error) => {
                            return Err(format!("领取 Strategy JobQueue 租约失败: {error:?}"))
                        }
                    };
                    let live_job_digest = if queued.job.job_id.starts_with("live-strategy:") {
                        Some(queued.run.manifest_digest.ok_or_else(|| {
                            "实时 Strategy Job 缺少 manifest_digest，拒绝执行".to_string()
                        })?)
                    } else {
                        None
                    };
                    if let Some(expected_digest) = live_job_digest {
                        let current_digest = live_strategy_snapshot_digest(
                            &strategy_runtime_config.strategy,
                            now,
                        )?;
                        if current_digest != Some(expected_digest) {
                            queue
                                .ack_at(
                                    queued.run.run_id,
                                    context.id(),
                                    lease.fencing_token,
                                    lease_now,
                                )
                                .map_err(|error| {
                                    format!("确认过期实时 Strategy Job 失败: {error:?}")
                                })?;
                            processed += 1;
                            println!(
                                "[策略 · Strategy] worker={} job={} skipped=stale-market-digest expected={:016x} actual={}",
                                context.id(),
                                queued.job.job_id,
                                expected_digest,
                                current_digest
                                    .map(|digest| format!("{digest:016x}"))
                                    .unwrap_or_else(|| "none".into())
                            );
                            continue;
                        }
                    }
                    let instrument = strategy_runtime_config
                        .strategy
                        .instrument
                        .as_deref()
                        .and_then(InstrumentId::parse)
                        .ok_or_else(|| "Strategy instrument 非法或未配置".to_string())?;
                    let (target_qty, contract_output) = if strategy_runtime_config
                        .strategy
                        .builtin_strategy
                        .is_some()
                    {
                        let output = invoke_builtin_strategy(
                            &root,
                            &strategy_runtime_config,
                            &instrument,
                            &queued.run.run_id.to_string(),
                            now,
                        )?;
                        (output.target_qty, Some(output))
                    } else if let Some(module) =
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
                    if let Some(expected_digest) = live_job_digest {
                        let current_digest = live_strategy_snapshot_digest(
                            &strategy_runtime_config.strategy,
                            runtime_timestamp_ms(),
                        )?;
                        if current_digest != Some(expected_digest) {
                            queue
                                .ack_at(
                                    queued.run.run_id,
                                    context.id(),
                                    lease.fencing_token,
                                    lease_now,
                                )
                                .map_err(|error| {
                                    format!("确认执行期间过期实时 Strategy Job 失败: {error:?}")
                                })?;
                            processed += 1;
                            println!(
                                "[策略 · Strategy] worker={} job={} skipped=market-changed-during-evaluation expected={:016x} actual={}",
                                context.id(),
                                queued.job.job_id,
                                expected_digest,
                                current_digest
                                    .map(|digest| format!("{digest:016x}"))
                                    .unwrap_or_else(|| "none".into())
                            );
                            continue;
                        }
                    }
                    let spread_group_id = if orders.len() >= 2 {
                        Some(spread_group_id(
                            context.id(),
                            queued.run.run_id,
                            contract_output
                                .as_ref()
                                .map(|output| output.signal_id)
                                .unwrap_or(queued.run.run_id),
                        ))
                    } else {
                        None
                    };
                    // 先完成全部策略配额和命令构造校验，再落组快照；这样配额不足
                    // 或订单无法序列化时不会留下一个没有对应命令的孤儿套利组。
                    let commands = orders
                        .iter()
                        .map(|order| {
                            strategy
                                .reserve_order()
                                .map_err(|error| format!("Strategy 订单配额拒绝: {error:?}"))?;
                            strategy_submit_command(
                                context.id(),
                                order,
                                queued.job.dry_run,
                                spread_group_id.as_deref(),
                            )
                        })
                        .collect::<Result<Vec<_>, String>>()?;
                    if !queued.job.dry_run {
                        if let Some(group_id) = spread_group_id.as_deref() {
                            persist_strategy_spread_group(&root, group_id, context.id(), &orders)?;
                        }
                    }
                    let mut results = Vec::new();
                    for command in commands {
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
                    if !queued.job.job_id.starts_with("live-strategy:") {
                        state_store
                            .transact_scheduler_at(&state_path, |scheduler| {
                                scheduler
                                    .finish_run_with_code(
                                        queued.run.run_id,
                                        true,
                                        None,
                                        lease_now,
                                    )
                                    .map(|_| ())
                                    .map_err(|error| format!("完成 JobRun 失败: {error:?}"))
                            })
                            .map_err(|error| format!("回写 JobRun 状态失败: {error:?}"))?
                            .1
                            .map_err(|error| format!("完成 JobRun 被拒绝: {error}"))?;
                    }
                    queue
                        .ack_at(queued.run.run_id, context.id(), lease.fencing_token, lease_now)
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
    join_worker_handle(&supervisor, handle, "Strategy", worker_id)
}
