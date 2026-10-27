use super::{build_strategy_contract_intent_order, build_strategy_target_order};
use crate::{
    claimable_by, command_is_final, complete_strategy_job_run, configured_command_queue,
    configured_control_store, configured_job_queue, evaluate_strategy_contract,
    fail_strategy_job_run, lease_clock, live_strategy_digest_state_path,
    live_strategy_input_digest, live_strategy_job, live_strategy_job_is_stale_with_config,
    load_c_abi_strategy, load_live_strategy_digest, load_scheduler_state,
    persist_strategy_spread_group, persist_strategy_submit, read_runtime_config,
    reject_ashare_rules_on_submit_path, resolve_all_worker_runtime_paths,
    resolve_strategy_runtime_paths, runtime_timestamp_ms, sandbox_audit_path,
    save_live_strategy_digest, spread_group_id, strategy_current_qty_exact_for,
    strategy_current_qty_for, strategy_job_lease_owner, strategy_run_is_final,
    strategy_submit_command, verify_strategy_artifact, PythonStrategyClient,
    SandboxExecutionPolicy, StrategyContractClients, StrategyJobLease, StrategyProcessClient,
};
use qx_control::CommandKind;
use qx_core::InstrumentId;
use qx_runtime::WorkerRole;
use qx_storage::StorageError;
use qx_zhenlu::StrategyRuntime;
use std::path::Path;

mod execution_updates;

macro_rules! worker_log {
    ($quiet:expr, $($arg:tt)*) => {
        if !$quiet {
            println!($($arg)*);
        }
    };
}

fn native_strategy_supports_execution_updates(
    supports_order_status: bool,
    supports_fill: bool,
    supports_account: bool,
) -> bool {
    supports_order_status || supports_fill || supports_account
}

pub(crate) fn run_strategy_worker(
    path: &Path,
    worker_id: &str,
    once: bool,
    quiet: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker =
        qx_service::worker_host::configured_worker(&config, worker_id, WorkerRole::Strategy)?
            .clone();
    // 本 worker 自己不碰 Venue，却会把 SubmitOrder 投进队列；A 股规则配在这里却不改变订单，等于让配置说假话。
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
    if strategy_config.external_executable.is_some()
        && strategy_config.strategy_artifact_sha256.is_none()
    {
        return Err(
            "持久 external_executable 策略必须配置 strategy_artifact_sha256，才能绑定恢复状态"
                .into(),
        );
    }
    // A Python request may occupy the strategy worker for its configured timeout.
    // Remote providers additionally need a bounded startup/rotation window to
    // create the sandbox, stage inputs and validate the remote package. The job lease must
    // outlive that whole request so another worker cannot claim and replay it.
    let execution_policy = if strategy_config.python_module.is_some() {
        Some(SandboxExecutionPolicy::from_env()?)
    } else {
        None
    };
    let remote_strategy = execution_policy
        .as_ref()
        .is_some_and(SandboxExecutionPolicy::sandbox_enabled);
    let request_budget_seconds = strategy_config.python_timeout_ms.saturating_add(999) / 1000;
    let sandbox_start_budget_seconds = if remote_strategy { 240 } else { 0 };
    let strategy_job_lease_seconds = request_budget_seconds
        .saturating_add(sandbox_start_budget_seconds)
        .saturating_add(60)
        .max(30);
    let mut strategy_runtime_config = config.clone();
    resolve_all_worker_runtime_paths(&mut strategy_runtime_config, path);
    // 将选中的实例投影到兼容的单策略执行路径，保留所有既有订单、目标仓位和审计逻辑。
    strategy_runtime_config.strategy = strategy_config.clone();
    let live_digest_path = live_strategy_digest_state_path(&root, worker_id);
    let persisted_live_digest = load_live_strategy_digest(&live_digest_path)?;
    let execution_cursor_path = execution_updates::cursor_path(&root, worker_id);
    let persisted_execution_cursor = execution_updates::load_cursor(&execution_cursor_path)?;
    // Load native code before validating the durable cursor so checkpoint capability is based
    // on the actual verified plugin exports, not on configuration guesses.
    let initial_native_client = strategy_config
        .c_abi_library
        .as_ref()
        .map(|_| load_c_abi_strategy(&strategy_config))
        .transpose()?;
    let durable_checkpoint_enabled = strategy_config.python_module.is_some()
        || strategy_config.external_executable.is_some()
        || initial_native_client
            .as_ref()
            .is_some_and(|client| client.supports_state_checkpoint());
    execution_updates::validate_checkpoint_binding_for_mode(
        &persisted_execution_cursor,
        &strategy_config,
        durable_checkpoint_enabled,
    )?;
    let sandbox_audit_path = sandbox_audit_path(&root, worker_id);
    let registered_id = worker.id.clone();
    let strategy_lease_owner = strategy_job_lease_owner(&registered_id);
    let runtime_config_path = path.to_path_buf();
    qx_service::worker_host::run_worker(
        config,
        &registered_id,
        WorkerRole::Strategy,
        "Strategy",
        move |context| {
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
                    PythonStrategyClient::start_with_strategy_config_and_audit_path(
                        // ring 参数只在 shared_memory_json/shared_memory_columnar 生效，其余协议保持兼容。
                        module,
                        strategy_config.python_timeout_ms,
                        strategy_config.transport,
                        qx_strategy::SharedRingConfig {
                            capacity: strategy_config.shared_memory_capacity,
                            slot_bytes: strategy_config.shared_memory_slot_bytes,
                        },
                        strategy_config.strategy_artifact_sha256.as_deref(),
                        strategy_config.python_strategy_spec.as_ref(),
                        strategy_config.account_id.as_deref(),
                        strategy_config.venue_id.as_deref(),
                        Some(&sandbox_audit_path),
                        Some(context.id()),
                    )
                })
                .transpose()?;
            if let Some(client) = python_client.as_mut() {
                client.require_strategy_checkpoint();
            }
            let mut external_client = strategy_config
                .external_executable
                .as_deref()
                .map(|executable| {
                    StrategyProcessClient::start_process_with_ring_directory(
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
                        None,
                    )
                })
                .transpose()?;
            if let Some(client) = external_client.as_mut() {
                client.require_strategy_checkpoint();
            }
            let mut native_client = initial_native_client;
            let mut native_initialized = false;
            let mut last_live_digest = persisted_live_digest;
            let mut execution_cursor = persisted_execution_cursor;
            context.mark(
                qx_runtime::ServiceStatus::Ready,
                format!("strategy version={}", strategy.version),
                Some(runtime_timestamp_ms()),
            )?;
            qx_service::worker_host::run_polling_worker(&context, once, || {
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
                    let claim = command_queue.claim_command(
                        command.command_id,
                        context.id(),
                        lease_now,
                        30,
                    );
                    let lease = match claim {
                        Ok(lease) => lease,
                        Err(StorageError::LeaseHeld { .. }) => continue,
                        Err(error) => {
                            return Err(format!("领取 Strategy 控制命令租约失败: {error:?}"));
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
                            .map_err(|error| {
                                format!("清理已终态 Strategy 控制命令失败: {error:?}")
                            })?;
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
                        .transact(|plane| {
                            plane.execute(command.command_id, now, |_| action.clone())
                        })
                        .map_err(|error| format!("回写 Strategy 控制命令终态失败: {error}"))?;
                    record_result
                        .map_err(|error| format!("Strategy 控制命令执行失败: {error:?}"))?;
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
                // 只有 Running 状态的策略才读队列：暂停/停止时这一轮没有任何待办。
                let pending = if matches!(strategy.state, qx_zhenlu::StrategyState::Running) {
                    if let Some(data_fingerprint) = live_strategy_input_digest(
                        &strategy_runtime_config,
                        &root,
                        &strategy_runtime_config.strategy,
                        now,
                    )? {
                        let can_receive_execution_updates =
                            strategy_runtime_config.strategy.builtin_strategy.is_none()
                                && (strategy_runtime_config.strategy.python_module.is_some()
                                    || strategy_runtime_config
                                        .strategy
                                        .external_executable
                                        .is_some()
                                    || native_client.as_ref().is_some_and(|client| {
                                        native_strategy_supports_execution_updates(
                                            client.supports_order_status_updates(),
                                            client.supports_fill_updates(),
                                            client.supports_account_updates(),
                                        )
                                    }));
                        let strategy_id = strategy_runtime_config
                            .strategy
                            .id
                            .clone()
                            .unwrap_or_else(|| strategy_runtime_config.strategy.version.clone());
                        let batch = if can_receive_execution_updates {
                            execution_updates::collect_pending(
                                &strategy_runtime_config,
                                &root,
                                &strategy_id,
                                &execution_cursor,
                            )?
                        } else {
                            execution_updates::PendingExecutionBatch::default()
                        };
                        // Event ranges with no order transitions for this strategy still need
                        // a durable acknowledgement, but they do not need a strategy job.
                        let has_python_module =
                            strategy_runtime_config.strategy.python_module.is_some();
                        let has_native_checkpoint = native_client
                            .as_ref()
                            .is_some_and(|client| client.supports_state_checkpoint());
                        let has_external_checkpoint = external_client
                            .as_ref()
                            .is_some_and(StrategyProcessClient::expects_strategy_checkpoint);
                        let needs_strategy_checkpoint_bootstrap = (has_python_module
                            || has_native_checkpoint
                            || has_external_checkpoint)
                            && execution_updates::strategy_checkpoint(&execution_cursor).is_none();
                        if batch.updates.is_empty()
                            && batch.fill_updates.is_empty()
                            && batch.market_ticks.is_empty()
                            && batch.market_order_books.is_empty()
                            && batch.account_updates.is_empty()
                            && !batch.ranges.is_empty()
                            && !needs_strategy_checkpoint_bootstrap
                        {
                            execution_updates::advance_cursor(
                                &mut execution_cursor,
                                &batch.ranges,
                            )?;
                            execution_updates::save_cursor(
                                &execution_cursor_path,
                                &execution_cursor,
                            )?;
                        }
                        if last_live_digest != Some(data_fingerprint)
                            || !batch.updates.is_empty()
                            || !batch.fill_updates.is_empty()
                            || !batch.market_ticks.is_empty()
                            || !batch.market_order_books.is_empty()
                            || !batch.account_updates.is_empty()
                            || needs_strategy_checkpoint_bootstrap
                        {
                            let (job, run) = live_strategy_job(
                                &strategy_runtime_config.strategy,
                                context.id(),
                                data_fingerprint,
                                now,
                                &strategy_runtime_config.environment,
                            );
                            let (mut job, mut run) = (job, run);
                            if !batch.ranges.is_empty()
                                && (!batch.updates.is_empty()
                                    || !batch.fill_updates.is_empty()
                                    || !batch.market_ticks.is_empty()
                                    || !batch.market_order_books.is_empty()
                                    || !batch.account_updates.is_empty()
                                    || has_python_module
                                    || has_native_checkpoint
                                    || has_external_checkpoint)
                            {
                                execution_updates::attach_ranges_to_job(
                                    &mut job,
                                    &mut run,
                                    &batch.ranges,
                                )?;
                            }
                            queue.enqueue(job, run, lease_now).map_err(|error| {
                                format!("写入实时 Strategy JobQueue 失败: {error:?}")
                            })?;
                            save_live_strategy_digest(&live_digest_path, data_fingerprint)?;
                            last_live_digest = Some(data_fingerprint);
                        }
                    }
                    queue
                        .available(lease_now)
                        .map_err(|error| format!("读取 Strategy JobQueue 失败: {error:?}"))?
                } else {
                    Vec::new()
                };
                for queued in pending {
                    // 领取判据与装配端 `validate_job_owners` 共用 `qx_scheduler::claimable_by`（V11 §41 E7）。
                    if !claimable_by(&queued.job.owner, context.id()) {
                        continue;
                    }
                    let mut lease = match queue.claim(
                        queued.run.run_id,
                        &strategy_lease_owner,
                        lease_now,
                        strategy_job_lease_seconds,
                    ) {
                        Ok(lease) => lease,
                        Err(StorageError::LeaseHeld { .. }) => continue,
                        Err(error) => {
                            return Err(format!("领取 Strategy JobQueue 租约失败: {error:?}"));
                        }
                    };
                    // 队列条目可能在回写终态之后、确认之前掉电：条目还在，运行却已经收口。
                    // 再执行一次就是二次提交，所以领取租约后先认 Scheduler 的终态（V13 R2 第十二遍 #190）。
                    if strategy_run_is_final(&state_store, &state_path, queued.run.run_id)? {
                        // 这句钉住不交给 rustfmt：门禁 LEASE_CALL_SITES 按剥掉空白的字面量认「租约域时钟」，
                        // 而四个参数合计 63 列超出 rustfmt 的参数预算，它总会竖排并补一个尾逗号。
                        #[rustfmt::skip]
                    queue
                        .ack_at(queued.run.run_id, &strategy_lease_owner, lease.fencing_token, lease_clock(runtime_timestamp_ms()))
                        .map_err(|error| format!("确认已终态 Strategy Job 失败: {error:?}"))?;
                        continue;
                    }
                    // Cursor/checkpoint commit is the durable boundary for class strategies.
                    // If the process stopped before scheduler finalization, finish this run
                    // without invoking Python again or repeating its order effects.
                    if execution_updates::is_committed_run(&execution_cursor, queued.run.run_id) {
                        let finalize_now = lease_clock(runtime_timestamp_ms());
                        lease = queue
                            .claim(
                                queued.run.run_id,
                                &strategy_lease_owner,
                                finalize_now,
                                strategy_job_lease_seconds,
                            )
                            .map_err(|error| {
                                format!("恢复已提交 Strategy Job 前租约已丢失: {error:?}")
                            })?;
                        if !queued.job.job_id.starts_with("live-strategy:") {
                            complete_strategy_job_run(
                                &state_store,
                                &state_path,
                                queued.run.run_id,
                                finalize_now,
                            )?;
                        }
                        queue
                            .ack_at(
                                queued.run.run_id,
                                &strategy_lease_owner,
                                lease.fencing_token,
                                lease_clock(runtime_timestamp_ms()),
                            )
                            .map_err(|error| format!("确认已提交 Strategy Job 失败: {error:?}"))?;
                        processed += 1;
                        continue;
                    }
                    // 一次作业的结局只有落进闭包才谈得上收口：抛出去的错先写成 Failed，
                    // 否则这条运行只剩「被下一轮 tick 升级成 TIMEOUT」一条出口。
                    let mut class_effects_may_be_durable = false;
                    let outcome = (|| -> Result<(), String> {
                        let live_job_digest = if queued.job.job_id.starts_with("live-strategy:") {
                            Some(queued.run.manifest_digest.ok_or_else(|| {
                                "实时 Strategy Job 缺少 manifest_digest，拒绝执行".to_string()
                            })?)
                        } else {
                            None
                        };
                        // 执行前与执行中两处闸门共用同一份现场：队列、条目、worker 身份与租约。
                        let job_lease = StrategyJobLease {
                            queue: &queue,
                            queued: &queued,
                            worker_id: context.id(),
                            lease_owner: &strategy_lease_owner,
                            lease_seconds: strategy_job_lease_seconds,
                        };
                        if live_strategy_job_is_stale_with_config(
                            &job_lease,
                            &strategy_runtime_config,
                            &root,
                            &strategy_runtime_config.strategy,
                            live_job_digest,
                            now,
                            "stale-market-digest",
                            "确认过期实时 Strategy Job 失败",
                        )? {
                            processed += 1;
                            return Ok(());
                        }
                        let execution_ranges = execution_updates::ranges_from_job(&queued.job)?;
                        let strategy_id = strategy_runtime_config
                            .strategy
                            .id
                            .clone()
                            .unwrap_or_else(|| strategy_runtime_config.strategy.version.clone());
                        let execution_updates = execution_updates::execution_updates_for_ranges(
                            &strategy_runtime_config,
                            &root,
                            &strategy_id,
                            &execution_ranges,
                        )?;
                        let instrument = strategy_runtime_config
                            .strategy
                            .instrument
                            .as_deref()
                            .and_then(InstrumentId::parse)
                            .ok_or_else(|| "Strategy instrument 非法或未配置".to_string())?;
                        let (target_qty, contract_output, strategy_checkpoint) =
                            evaluate_strategy_contract(
                                &root,
                                &strategy_runtime_config,
                                &instrument,
                                queued.run.run_id,
                                now,
                                &execution_updates.order_updates,
                                &execution_updates.fill_updates,
                                &execution_updates.market_ticks,
                                &execution_updates.market_order_books,
                                &execution_updates.account_updates,
                                execution_updates::strategy_checkpoint(&execution_cursor),
                                StrategyContractClients {
                                    python_client: &mut python_client,
                                    external_client: &mut external_client,
                                    native_client: &mut native_client,
                                    native_initialized: &mut native_initialized,
                                    runtime_config_path: &runtime_config_path,
                                },
                            )?;
                        let orders = if let Some(output) = contract_output.as_ref() {
                            if output.intents.is_empty() {
                                let current_qty = strategy_current_qty_for(
                                    &root,
                                    &strategy_runtime_config,
                                    &instrument,
                                )?;
                                let current_qty_exact = output
                                    .target_qty_exact
                                    .as_ref()
                                    .map(|_| {
                                        strategy_current_qty_exact_for(
                                            &root,
                                            &strategy_runtime_config,
                                            &instrument,
                                        )
                                    })
                                    .transpose()?;
                                build_strategy_target_order(
                                    &strategy_runtime_config,
                                    &runtime_config_path,
                                    context.id(),
                                    queued.run.run_id,
                                    now,
                                    current_qty,
                                    target_qty,
                                    current_qty_exact.as_deref(),
                                    Some(output),
                                )?
                                .into_iter()
                                .collect::<Vec<_>>()
                            } else {
                                output
                                    .intents
                                    .iter()
                                    .map(|intent| {
                                        build_strategy_contract_intent_order(
                                            &root,
                                            &strategy_runtime_config,
                                            &runtime_config_path,
                                            context.id(),
                                            output.signal_id,
                                            &output.request_id,
                                            intent,
                                            now,
                                        )
                                    })
                                    .collect::<Result<Vec<_>, String>>()?
                            }
                        } else {
                            let current_qty = strategy_current_qty_for(
                                &root,
                                &strategy_runtime_config,
                                &instrument,
                            )?;
                            build_strategy_target_order(
                                &strategy_runtime_config,
                                &runtime_config_path,
                                context.id(),
                                queued.run.run_id,
                                now,
                                current_qty,
                                target_qty,
                                None,
                                None,
                            )?
                            .into_iter()
                            .collect::<Vec<_>>()
                        };
                        let evaluation_lease_now = lease_clock(runtime_timestamp_ms());
                        lease = queue
                            .claim(
                                queued.run.run_id,
                                &strategy_lease_owner,
                                evaluation_lease_now,
                                strategy_job_lease_seconds,
                            )
                            .map_err(|error| {
                                format!("Strategy Job 在策略评估后失去租约: {error:?}")
                            })?;
                        if live_strategy_job_is_stale_with_config(
                            &job_lease,
                            &strategy_runtime_config,
                            &root,
                            &strategy_runtime_config.strategy,
                            live_job_digest,
                            runtime_timestamp_ms(),
                            "market-changed-during-evaluation",
                            "确认执行期间过期实时 Strategy Job 失败",
                        )? {
                            processed += 1;
                            return Ok(());
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
                        if durable_checkpoint_enabled && !queued.job.dry_run && !commands.is_empty()
                        {
                            // Even a failed submit response can mean a command was persisted.
                            // Keep this run retryable until its strategy checkpoint commits.
                            class_effects_may_be_durable = true;
                        }
                        if !queued.job.dry_run {
                            if let Some(group_id) = spread_group_id.as_deref() {
                                persist_strategy_spread_group(
                                    &root,
                                    group_id,
                                    context.id(),
                                    &orders,
                                )?;
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
                        let commit_lease_now = lease_clock(runtime_timestamp_ms());
                        lease = queue
                            .claim(
                                queued.run.run_id,
                                &strategy_lease_owner,
                                commit_lease_now,
                                strategy_job_lease_seconds,
                            )
                            .map_err(|error| {
                                format!(
                                    "Strategy Job 在提交 cursor/checkpoint 前失去租约: {error:?}"
                                )
                            })?;
                        if durable_checkpoint_enabled {
                            let checkpoint = strategy_checkpoint.ok_or_else(|| {
                                "Strategy did not return a durable checkpoint".to_string()
                            })?;
                            execution_updates::commit_strategy_checkpoint(
                                &mut execution_cursor,
                                &execution_cursor_path,
                                &strategy_runtime_config.strategy,
                                queued.run.run_id,
                                &execution_ranges,
                                checkpoint,
                            )?;
                        } else if !execution_ranges.is_empty() {
                            // Non-class process strategies retain the legacy durable cursor.
                            execution_updates::advance_cursor(
                                &mut execution_cursor,
                                &execution_ranges,
                            )?;
                            execution_updates::save_cursor(
                                &execution_cursor_path,
                                &execution_cursor,
                            )?;
                        }
                        if !queued.job.job_id.starts_with("live-strategy:") {
                            complete_strategy_job_run(
                                &state_store,
                                &state_path,
                                queued.run.run_id,
                                lease_clock(runtime_timestamp_ms()),
                            )?;
                        }
                        queue
                            .ack_at(
                                queued.run.run_id,
                                &strategy_lease_owner,
                                lease.fencing_token,
                                lease_clock(runtime_timestamp_ms()),
                            )
                            .map_err(|error| format!("确认 Strategy JobQueue 失败: {error:?}"))?;
                        processed += 1;
                        worker_log!(
                            quiet,
                            "[策略 · Strategy] worker={} job={} run_id={} result={}",
                            context.id(),
                            queued.job.job_id,
                            queued.run.run_id,
                            result
                        );
                        Ok(())
                    })();
                    if let Err(error) = outcome {
                        // Once the cursor/checkpoint marker is durable, leave scheduler recovery
                        // to the next lease owner; recording Failed here would contradict the
                        // already committed strategy effects and checkpoint.
                        if execution_updates::is_committed_run(&execution_cursor, queued.run.run_id)
                            || class_effects_may_be_durable
                        {
                            return Err(error);
                        }
                        let failure_lease_now = lease_clock(runtime_timestamp_ms());
                        if let Err(lease_error) = queue.claim(
                            queued.run.run_id,
                            &strategy_lease_owner,
                            failure_lease_now,
                            strategy_job_lease_seconds,
                        ) {
                            return Err(format!(
                                "Strategy Job 租约已转交，保留作业供当前租约持有人恢复: 原因={error}; 租约={lease_error:?}"
                            ));
                        }
                        fail_strategy_job_run(
                            &state_store,
                            &state_path,
                            &queued.job.job_id,
                            queued.run.run_id,
                            failure_lease_now,
                        )?;
                        return Err(error);
                    }
                }
                context.heartbeat(now)?;
                if once {
                    worker_log!(
                        quiet,
                        "[策略 · Strategy] worker={} READY processed={}",
                        context.id(),
                        processed
                    );
                }
                Ok(200)
            })?;
            Ok(())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{native_strategy_supports_execution_updates, strategy_job_lease_owner};
    use qx_storage::{FileJobQueue, StorageError};

    #[test]
    fn account_only_native_strategy_collects_execution_update_batches() {
        assert!(native_strategy_supports_execution_updates(
            false, false, true
        ));
    }

    #[test]
    fn native_strategy_without_update_callbacks_does_not_collect_update_batches() {
        assert!(!native_strategy_supports_execution_updates(
            false, false, false
        ));
    }

    #[test]
    fn existing_native_order_and_fill_callback_capabilities_still_collect_updates() {
        assert!(native_strategy_supports_execution_updates(
            true, false, false
        ));
        assert!(native_strategy_supports_execution_updates(
            false, true, false
        ));
    }

    #[test]
    fn parallel_instances_with_the_same_worker_id_cannot_share_a_job_lease() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "strategy-worker-lease-owner-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let queue = FileJobQueue::new(&directory);
        let worker_id = "strategy-worker-a";
        let (job, run) = qx_scheduler::live_strategy_job(qx_scheduler::LiveStrategyJobInput {
            job_version: "strategy-v1",
            worker_id,
            instrument: Some("BTCUSDT.OKX"),
            data_fingerprint: 99,
            trading_day: "2026-10-27".into(),
            lease_now: 10,
            environment: "paper",
        });
        let run_id = run.run_id;
        queue.enqueue(job, run, 10).unwrap();

        let first_owner = strategy_job_lease_owner(worker_id);
        let second_owner = strategy_job_lease_owner(worker_id);
        assert_ne!(first_owner, second_owner);
        assert!(first_owner.starts_with("strategy-worker-a@"));
        assert!(second_owner.starts_with("strategy-worker-a@"));
        let first = queue.claim(run_id, &first_owner, 10, 30).unwrap();
        assert!(matches!(
            queue.claim(run_id, &second_owner, 11, 30),
            Err(StorageError::LeaseHeld { .. })
        ));
        assert_eq!(first.owner, first_owner);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
