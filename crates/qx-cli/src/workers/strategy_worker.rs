use crate::{
    claimable_by, configured_command_queue, configured_control_store, configured_job_queue,
    lease_clock, live_strategy_digest_state_path, live_strategy_job_is_stale_with_config,
    load_c_abi_strategy, load_live_strategy_digest, load_scheduler_state,
    persist_strategy_spread_group, read_runtime_config, reject_ashare_rules_on_submit_path,
    resolve_all_worker_runtime_paths, resolve_strategy_runtime_paths, runtime_timestamp_ms,
    sandbox_audit_path, strategy_job_lease_owner, verify_strategy_artifact, PythonStrategyClient,
    SandboxExecutionPolicy, StrategyJobLease, StrategyProcessClient,
};
use qx_core::EventKind;
use qx_runtime::WorkerRole;
use qx_storage::StorageError;
use qx_zhenlu::StrategyRuntime;
use sha2::Digest;
use std::path::Path;

pub(super) const MAX_JOBS_PER_POLL: usize = 256;

mod commands;
use self::commands::process_strategy_control_commands;
mod admission;
mod execution_preparation;
mod execution_updates;
mod job_recovery;
mod lifecycle;
mod live_input;
mod order_intents;
mod prepared_command_replay;
mod prepared_finalize;
mod prepared_recovery;
mod strategy_evaluation;
mod strategy_outcome;
mod strategy_submission;
use self::admission::{
    live_command_outcome_is_retryable, native_strategy_supports_execution_updates,
    require_live_intent_reconciliation, require_live_strategy_commands_resolved,
};
#[cfg(test)]
use self::admission::{
    require_live_strategy_command_status, require_live_venue_order_observation,
    runtime_python_warmup_complete, VenueOrderObservation,
};

macro_rules! worker_log {
    ($quiet:expr, $($arg:tt)*) => {
        if !$quiet {
            println!($($arg)*);
        }
    };
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
    let strategy_lease_owner = strategy_job_lease_owner(&worker.id);
    let mut observation = qx_service::strategy_deployments::StrategyObservationReporter::new(
        &config,
        &worker.id,
        &strategy_lease_owner,
    )?;
    let queue = configured_job_queue(&config, &root)?;
    let control_store = configured_control_store(&config)?;
    let command_queue = configured_command_queue(&config, &root)?;
    let (state_store, _, state_path) = load_scheduler_state(&config, &root, path)?;
    let mut strategy_config = config.strategy_for_worker(worker_id)?;
    if let Some(path) = std::env::var_os(qx_runtime::STRATEGY_DEPLOYMENT_BUNDLE_ENV) {
        let path = std::path::PathBuf::from(path)
            .to_str()
            .ok_or_else(|| "supervisor strategy bundle path is not valid Unicode".to_string())?
            .to_string();
        if strategy_config.c_abi_library.is_some() {
            strategy_config.c_abi_library = Some(path);
        } else if strategy_config.external_executable.is_some() {
            strategy_config.external_executable = Some(path);
        } else if strategy_config.python_module.is_some() {
            strategy_config.python_module = Some(path);
        } else if strategy_config.builtin_strategy.is_none() {
            return Err("supervisor supplied a bundle for an unsupported strategy artifact".into());
        }
    }
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
    #[cfg(any(feature = "bench-prepared-evaluation", test))]
    let strategy_job_lease_seconds = std::env::var("QX_BENCH_STRATEGY_LEASE_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(strategy_job_lease_seconds);
    let mut strategy_runtime_config = config.clone();
    resolve_all_worker_runtime_paths(&mut strategy_runtime_config, path);
    // 将选中的实例投影到兼容的单策略执行路径，保留所有既有订单、目标仓位和审计逻辑。
    strategy_runtime_config.strategy = strategy_config.clone();
    let live_digest_path = live_strategy_digest_state_path(&root, worker_id);
    let persisted_live_digest = load_live_strategy_digest(&live_digest_path)?;
    let execution_cursor_path =
        execution_updates::configured_cursor_path(&root, worker_id, &config)?;
    let prepared_evaluation_path = execution_updates::prepared_evaluation_path(&root, worker_id);
    let mut persisted_execution_cursor = execution_updates::load_cursor(&execution_cursor_path)?;
    // Load native code before validating the durable cursor so checkpoint capability is based
    // on the actual verified plugin exports, not on configuration guesses.
    let initial_native_client = strategy_config
        .c_abi_library
        .as_ref()
        .map(|_| load_c_abi_strategy(&strategy_config))
        .transpose()?;
    let host_capabilities = if strategy_config.builtin_strategy.is_some() {
        Some(qx_strategy::capabilities::StrategyHostCapabilitiesV1::builtin_rust())
    } else if strategy_config.python_module.is_some() {
        Some(match strategy_config.python_strategy_spec.as_ref() {
            Some(spec) => qx_strategy::capabilities::StrategyHostCapabilitiesV1::runtime_python_class(
                spec.get("data_level")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("bar"),
                strategy_config.account_id.is_some() && strategy_config.venue_id.is_some(),
            )?,
            None => qx_strategy::capabilities::StrategyHostCapabilitiesV1::runtime_python_module_function(
                strategy_config.account_id.is_some() && strategy_config.venue_id.is_some(),
            ),
        })
    } else if strategy_config.external_executable.is_some() {
        Some(
            qx_strategy::capabilities::StrategyHostCapabilitiesV1::runtime_external_process(
                strategy_config.account_id.is_some() && strategy_config.venue_id.is_some(),
            ),
        )
    } else if let Some(client) = initial_native_client.as_ref() {
        Some(
            qx_strategy::capabilities::StrategyHostCapabilitiesV1::c_abi(
                client.supports_order_status_updates(),
                client.supports_fill_updates(),
                client.supports_account_updates(),
                client.supports_state_checkpoint(),
                strategy_config.account_id.is_some() && strategy_config.venue_id.is_some(),
            ),
        )
    } else {
        // The default StrategyRuntime is also a concrete Rust host. Omitting its
        // catalog leaves observations indistinguishable from an unreported host,
        // which keeps live readiness blocked even after construction/init succeed.
        Some(qx_strategy::capabilities::StrategyHostCapabilitiesV1::builtin_rust())
    };
    if let Some(kind) = strategy_config.builtin_strategy.as_deref() {
        validate_builtin_strategy_capabilities(kind, &strategy_config.required_capabilities)?;
    }
    if let Some(capabilities) = host_capabilities.as_ref() {
        capabilities.validate_required(&strategy_config.required_capabilities)?;
    } else if !strategy_config.required_capabilities.is_empty() {
        return Err(format!(
            "strategy {} declares required_capabilities but no execution host can satisfy them",
            strategy_config
                .id
                .as_deref()
                .unwrap_or(&strategy_config.version)
        ));
    }
    let durable_checkpoint_enabled = strategy_config.python_module.is_some()
        || strategy_config.external_executable.is_some()
        || initial_native_client
            .as_ref()
            .is_some_and(|client| client.supports_state_checkpoint());
    let deployment_revision = match std::env::var(qx_runtime::STRATEGY_DEPLOYMENT_REVISION_ENV) {
        Ok(value) => Some(value.parse::<u64>().map_err(|error| {
            format!("invalid supervisor strategy deployment revision: {error}")
        })?),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => {
            return Err(format!(
                "cannot read supervisor deployment revision: {error}"
            ));
        }
    };
    execution_updates::bind_cursor_to_deployment_revision(
        &mut persisted_execution_cursor,
        &execution_cursor_path,
        &strategy_config,
        durable_checkpoint_enabled,
        deployment_revision,
    )?;
    execution_updates::validate_checkpoint_binding_for_mode(
        &persisted_execution_cursor,
        &strategy_config,
        durable_checkpoint_enabled,
    )?;
    let sandbox_audit_path = sandbox_audit_path(&root, worker_id);
    let registered_id = worker.id.clone();
    let runtime_config_path = path.to_path_buf();
    qx_service::worker_host::run_worker(
        config,
        &registered_id,
        WorkerRole::Strategy,
        "Strategy",
        move |context| {
            let persisted_control = control_store.load()?;
            let starts_paused =
                lifecycle::persisted_pause_state(&persisted_control, context.id()).unwrap_or(false);
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
            if starts_paused {
                strategy.pause().map_err(|error| {
                    format!("failed to restore paused strategy state: {error:?}")
                })?;
            }
            let mut python_client = strategy_config
                .python_module
                .as_deref()
                .map(|module| {
                    PythonStrategyClient::start_for_runtime_worker(
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
                    StrategyProcessClient::start_process_with_ring_directory_and_capabilities(
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
                        host_capabilities.as_ref().ok_or_else(|| {
                            "external Strategy host capability catalog was not initialized"
                                .to_string()
                        })?,
                        &strategy_config.required_capabilities,
                    )
                })
                .transpose()?;
            if let Some(client) = python_client.as_mut() {
                client.require_strategy_checkpoint();
            }
            if let Some(client) = external_client.as_mut() {
                client.require_strategy_checkpoint();
            }
            let mut native_client = initial_native_client;
            let mut native_initialized = false;
            let mut last_live_digest = persisted_live_digest;
            let mut execution_cursor = persisted_execution_cursor;
            let mut checkpoint_roundtrip_observed = false;
            let warmup_applicable = strategy_config.python_module.is_some()
                || strategy_config.external_executable.is_some()
                || strategy_config.c_abi_library.is_some();
            let host_id = host_capabilities
                .as_ref()
                .map(|capabilities| capabilities.host_id.clone())
                .or_else(|| {
                    strategy_config
                        .external_executable
                        .as_ref()
                        .map(|_| "external_process".into())
                })
                .or_else(|| {
                    strategy_config
                        .c_abi_library
                        .as_ref()
                        .map(|_| "c_abi".into())
                })
                .or_else(|| {
                    strategy_config
                        .python_module
                        .as_ref()
                        .map(|_| "python_module_function".into())
                })
                .unwrap_or_else(|| "builtin_rust_strategy_runtime".into());
            let initialization_deferred = strategy_config
                .python_module
                .as_ref()
                .is_some_and(|_| strategy_config.python_strategy_spec.is_some())
                || strategy_config.external_executable.is_some()
                || strategy_config.c_abi_library.is_some();
            let mut checkpoint_restore_required = strategy_config.external_executable.is_some()
                && execution_updates::strategy_checkpoint(&execution_cursor).is_some_and(
                    |checkpoint| {
                        checkpoint
                            .adapter_state
                            .get("state_mode")
                            .and_then(serde_json::Value::as_str)
                            == Some("external-process")
                    },
                );
            let mut host_lifecycle = qx_api::StrategyHostLifecycleEvidenceV2 {
                schema_version: 2,
                host_id,
                constructed: true,
                initialized: !initialization_deferred,
                checkpoint_input_supplied: execution_updates::strategy_checkpoint(
                    &execution_cursor,
                )
                .is_some(),
                checkpoint_restore_required: strategy_config
                    .external_executable
                    .as_ref()
                    .map(|_| checkpoint_restore_required),
                checkpoint_restore_acknowledged: checkpoint_restore_required.then_some(false),
                warmup_applicable,
                warmup_required_instruments: Vec::new(),
                warmup_completed_instruments: Vec::new(),
            };
            observation.record(
                if starts_paused {
                    qx_api::StrategyWorkerPhase::Paused
                } else {
                    qx_api::StrategyWorkerPhase::Running
                },
                host_capabilities.clone(),
                false,
            )?;
            observation.record_host_lifecycle(host_lifecycle.clone())?;
            let lifecycle = if starts_paused { "paused" } else { "running" };
            context.mark(
                qx_runtime::ServiceStatus::Ready,
                format!(
                    "strategy version={} lifecycle={} strategy_accepting_jobs={}",
                    strategy.version, lifecycle, !starts_paused
                ),
                Some(runtime_timestamp_ms()),
            )?;
            qx_service::worker_host::run_polling_worker(&context, once, || {
                let now = runtime_timestamp_ms();
                execution_updates::clear_committed_prepared_evaluation(
                    &execution_cursor,
                    &prepared_evaluation_path,
                )?;
                // 命令队列 / 作业队列的租约与 JobRun 状态都在秒域，见 `lease_clock`。
                let lease_now = lease_clock(now);
                process_strategy_control_commands(
                    &control_store,
                    &command_queue,
                    &mut strategy,
                    &context,
                    now,
                    lease_now,
                )?;
                let mut processed = 0_usize;
                // 只有 Running 状态的策略才读队列：暂停/停止时这一轮没有任何待办。
                let pending = if matches!(strategy.state, qx_zhenlu::StrategyState::Running) {
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
                    let has_python_module =
                        strategy_runtime_config.strategy.python_module.is_some();
                    let has_native_checkpoint = native_client
                        .as_ref()
                        .is_some_and(|client| client.supports_state_checkpoint());
                    let has_external_checkpoint = external_client
                        .as_ref()
                        .is_some_and(StrategyProcessClient::expects_strategy_checkpoint);
                    live_input::collect_and_enqueue_live_input(
                        &strategy_runtime_config,
                        &root,
                        context.id(),
                        now,
                        &queue,
                        &mut execution_cursor,
                        &execution_cursor_path,
                        &live_digest_path,
                        &mut last_live_digest,
                        can_receive_execution_updates,
                        has_python_module,
                        has_native_checkpoint,
                        has_external_checkpoint,
                    )?;
                    queue
                        .available_for_owner(lease_now, context.id(), MAX_JOBS_PER_POLL)
                        .map_err(|error| format!("读取 Strategy JobQueue 失败: {error:?}"))?
                } else {
                    Vec::new()
                };
                'pending_jobs: for queued in pending {
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
                    if job_recovery::recover_completed_strategy_job(
                        &queue,
                        &state_store,
                        &state_path,
                        &prepared_evaluation_path,
                        &execution_cursor,
                        &queued,
                        &strategy_lease_owner,
                        strategy_job_lease_seconds,
                        &mut lease,
                        &mut processed,
                    )? {
                        continue;
                    }
                    if let Some(mut prepared) = prepared_recovery::load_fenced_prepared_evaluation(
                        &prepared_evaluation_path,
                        &strategy_runtime_config,
                        &root,
                        context.id(),
                        &queue,
                        &queued,
                        &strategy_lease_owner,
                        strategy_job_lease_seconds,
                        &mut lease,
                    )? {
                        if queued.job.job_id.starts_with("live-strategy:") {
                            require_live_intent_reconciliation(
                                &strategy_runtime_config.strategy,
                                &root,
                                runtime_timestamp_ms(),
                            )?;
                            if durable_checkpoint_enabled
                                && execution_updates::strategy_checkpoint(&execution_cursor)
                                    .is_none()
                            {
                                if !prepared.commands.is_empty() {
                                    return Err(
                                        "LIVE_READINESS_CHECKPOINT_MISSING_WITH_PREPARED_COMMANDS; prepared intent is blocked for unknown-submission recovery".into(),
                                    );
                                }
                                prepared_finalize::finalize_strategy_run(
                                    prepared_finalize::FinalizeStrategyRun {
                                        strategy_config: &strategy_runtime_config.strategy,
                                        queued: &queued,
                                        queue: &queue,
                                        lease: &lease,
                                        lease_owner: &strategy_lease_owner,
                                        refresh_lease_before_commit: false,
                                        lease_seconds: strategy_job_lease_seconds,
                                        durable_checkpoint_enabled,
                                        checkpoint: prepared.checkpoint.clone(),
                                        checkpoint_missing_error:
                                            "LIVE_READINESS_CHECKPOINT_BOOTSTRAP_MISSING",
                                        ranges: &prepared.ranges,
                                        mark_run_committed: false,
                                        execution_cursor: &mut execution_cursor,
                                        execution_cursor_path: &execution_cursor_path,
                                        state_store: &state_store,
                                        state_path: &state_path,
                                        prepared_evaluation_path: &prepared_evaluation_path,
                                        journal_cleanup:
                                            prepared_finalize::PreparedJournalCleanup::RunJournalBefore,
                                        live_commands_resolved: true,
                                        ack_failure_context: "确认 checkpoint-only live bootstrap ",
                                        checkpoint_roundtrip_observed:
                                            &mut checkpoint_roundtrip_observed,
                                        processed: &mut processed,
                                    },
                                )?;
                                continue 'pending_jobs;
                            }
                        }
                        execution_updates::save_prepared_evaluation(
                            &prepared_evaluation_path,
                            &prepared,
                        )?;
                        if let Some(group_id) = prepared.spread_group_id.as_deref() {
                            persist_strategy_spread_group(
                                &root,
                                group_id,
                                context.id(),
                                &prepared.orders,
                            )?;
                        }
                        if let Err(error) = prepared_command_replay::replay_prepared_commands(
                            &prepared_evaluation_path,
                            &mut prepared,
                            &strategy_runtime_config.strategy,
                            &root,
                            &queued,
                            &queue,
                            &strategy_lease_owner,
                            strategy_job_lease_seconds,
                            &mut lease,
                            &control_store,
                            &command_queue,
                        ) {
                            if queued.job.job_id.starts_with("live-strategy:")
                                && live_command_outcome_is_retryable(&error)
                            {
                                worker_log!(
                                    quiet,
                                    "[策略 · Strategy] worker={} run_id={} intent recovery pending: {}",
                                    context.id(),
                                    queued.run.run_id,
                                    error
                                );
                                processed += 1;
                                continue 'pending_jobs;
                            }
                            return Err(error);
                        }
                        if queued.job.job_id.starts_with("live-strategy:") {
                            if let Err(error) = require_live_strategy_commands_resolved(
                                &strategy_runtime_config,
                                &root,
                                &control_store.load()?,
                                &prepared.commands,
                            ) {
                                if live_command_outcome_is_retryable(&error) {
                                    worker_log!(
                                        quiet,
                                        "[策略 · Strategy] worker={} run_id={} intent recovery pending: {}",
                                        context.id(),
                                        queued.run.run_id,
                                        error
                                    );
                                    processed += 1;
                                    continue 'pending_jobs;
                                }
                                return Err(error);
                            }
                        }
                        prepared_finalize::finalize_strategy_run(
                            prepared_finalize::FinalizeStrategyRun {
                                strategy_config: &strategy_runtime_config.strategy,
                                queued: &queued,
                                queue: &queue,
                                lease: &lease,
                                lease_owner: &strategy_lease_owner,
                                refresh_lease_before_commit: false,
                                lease_seconds: strategy_job_lease_seconds,
                                durable_checkpoint_enabled,
                                checkpoint: prepared.checkpoint.clone(),
                                checkpoint_missing_error:
                                    "PreparedEvaluation 缺少必需的 Strategy checkpoint",
                                ranges: &prepared.ranges,
                                mark_run_committed: !durable_checkpoint_enabled,
                                execution_cursor: &mut execution_cursor,
                                execution_cursor_path: &execution_cursor_path,
                                state_store: &state_store,
                                state_path: &state_path,
                                prepared_evaluation_path: &prepared_evaluation_path,
                                journal_cleanup:
                                    prepared_finalize::PreparedJournalCleanup::RunJournalAfter,
                                live_commands_resolved: true,
                                ack_failure_context: "恢复 PreparedEvaluation 后确认队列 ",
                                checkpoint_roundtrip_observed: &mut checkpoint_roundtrip_observed,
                                processed: &mut processed,
                            },
                        )?;
                        continue;
                    }
                    if let strategy_outcome::StrategyJobOutcomeDisposition::Retry =
                        strategy_outcome::execute_strategy_job_outcome(
                            strategy_outcome::StrategyJobOutcome {
                                runtime_config: &strategy_runtime_config,
                                strategy_config: &strategy_runtime_config.strategy,
                                root: &root,
                                worker_id: context.id(),
                                now,
                                queued: &queued,
                                queue: &queue,
                                lease: &mut lease,
                                lease_owner: &strategy_lease_owner,
                                lease_seconds: strategy_job_lease_seconds,
                                durable_checkpoint_enabled,
                                execution_cursor: &mut execution_cursor,
                                execution_cursor_path: &execution_cursor_path,
                                prepared_evaluation_path: &prepared_evaluation_path,
                                state_store: &state_store,
                                state_path: &state_path,
                                control_store: &control_store,
                                command_queue: command_queue.as_ref(),
                                strategy: &mut strategy,
                                checkpoint_restore_required: &mut checkpoint_restore_required,
                                host_lifecycle: &mut host_lifecycle,
                                python_client: &mut python_client,
                                external_client: &mut external_client,
                                native_client: &mut native_client,
                                native_initialized: &mut native_initialized,
                                runtime_config_path: &runtime_config_path,
                                checkpoint_roundtrip_observed: &mut checkpoint_roundtrip_observed,
                                processed: &mut processed,
                                quiet,
                            },
                        )?
                    {
                        continue 'pending_jobs;
                    }
                }
                observation.record(
                    if matches!(strategy.state, qx_zhenlu::StrategyState::Paused) {
                        qx_api::StrategyWorkerPhase::Paused
                    } else {
                        qx_api::StrategyWorkerPhase::Running
                    },
                    host_capabilities.clone(),
                    checkpoint_roundtrip_observed,
                )?;
                observation.record_host_lifecycle(host_lifecycle.clone())?;
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
            observation.finish()?;
            Ok(())
        },
    )
}

fn validate_builtin_strategy_capabilities(
    kind: &str,
    declared: &[qx_strategy::capabilities::StrategyCapability],
) -> Result<(), String> {
    let kind = qx_strategy::BuiltinStrategyKind::parse(kind)?;
    let capabilities = qx_strategy::capabilities::StrategyHostCapabilitiesV1::builtin_rust();
    capabilities.validate_required(kind.required_capabilities())?;
    capabilities.validate_required(declared)
}

#[cfg(feature = "bench-prepared-evaluation")]
pub(crate) fn run_prepared_evaluation_bench(args: &[String]) -> Result<(), String> {
    execution_updates::run_prepared_evaluation_bench(args)
}

#[cfg(test)]
mod tests {
    use super::{
        live_command_outcome_is_retryable, native_strategy_supports_execution_updates,
        require_live_strategy_command_status, require_live_venue_order_observation,
        runtime_python_warmup_complete, strategy_job_lease_owner,
        validate_builtin_strategy_capabilities, VenueOrderObservation,
    };
    use qx_storage::{FileJobQueue, StorageError};

    #[test]
    fn builtin_worker_validates_declared_and_algorithm_capabilities_before_dispatch() {
        use qx_strategy::capabilities::StrategyCapability::{Bar, MultiInstrument, Timer};

        assert!(validate_builtin_strategy_capabilities("sma_cross", &[Bar]).is_ok());
        assert!(
            validate_builtin_strategy_capabilities("sma_cross", &[Timer])
                .unwrap_err()
                .contains("timer")
        );
        assert!(
            validate_builtin_strategy_capabilities("pairs_arbitrage", &[])
                .unwrap_err()
                .contains("multi_instrument")
        );
        assert!(
            validate_builtin_strategy_capabilities("pairs_arbitrage", &[MultiInstrument])
                .unwrap_err()
                .contains("multi_instrument")
        );
    }

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
    fn live_strategy_checkpoint_requires_executed_control_commands() {
        assert!(require_live_strategy_command_status(
            41,
            Some(qx_control::CommandStatus::Executed),
        )
        .is_ok());
        assert!(require_live_strategy_command_status(
            41,
            Some(qx_control::CommandStatus::Accepted),
        )
        .unwrap_err()
        .contains("OUTCOME_PENDING"));
        assert!(
            require_live_strategy_command_status(41, Some(qx_control::CommandStatus::Failed),)
                .unwrap_err()
                .contains("venue reconciliation is required")
        );
        assert!(require_live_strategy_command_status(41, None)
            .unwrap_err()
            .contains("AUDIT_MISSING"));
    }

    #[test]
    fn live_strategy_checkpoint_requires_a_resolved_venue_order_fact() {
        for observation in [
            VenueOrderObservation::Accepted,
            VenueOrderObservation::Rejected,
            VenueOrderObservation::Cancelled,
            VenueOrderObservation::Filled,
        ] {
            assert!(require_live_venue_order_observation(41, Some(observation)).is_ok());
        }
        for observation in [
            VenueOrderObservation::Submitted,
            VenueOrderObservation::Unresolved,
        ] {
            assert!(require_live_venue_order_observation(41, Some(observation))
                .unwrap_err()
                .contains("OUTCOME_UNRESOLVED"));
        }
        assert!(require_live_venue_order_observation(41, None)
            .unwrap_err()
            .contains("FACT_MISSING"));
    }

    #[test]
    fn unresolved_live_intents_retry_after_the_job_lease_expires() {
        for error in [
            "LIVE_COMMAND_OUTCOME_PENDING:41",
            "LIVE_COMMAND_OUTCOME_FAILED:41; venue reconciliation is required",
            "LIVE_COMMAND_OUTCOME_UNKNOWN:41: storage outcome could not be confirmed",
            "LIVE_COMMAND_EVENTLOG_MISSING:paper-main-binance-events",
            "LIVE_VENUE_ORDER_OUTCOME_UNRESOLVED:41",
            "LIVE_VENUE_ORDER_FACT_MISSING:41",
            "LIVE_READINESS_RECONCILE_NOT_AFTER_FAILURE account=main",
        ] {
            assert!(live_command_outcome_is_retryable(error), "{error}");
        }
        for error in [
            "LIVE_COMMAND_OUTCOME_DIGEST_MISMATCH:41",
            "LIVE_COMMAND_AUDIT_MISSING:41",
            "LIVE_COMMAND_ORDER_ACCOUNT_MISMATCH:41",
        ] {
            assert!(!live_command_outcome_is_retryable(error), "{error}");
        }
    }

    #[test]
    fn runtime_python_live_admission_requires_warmup_for_the_configured_instrument() {
        let mut config = qx_runtime::RuntimeConfig::from_json(include_str!(
            "../../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .unwrap();
        let strategy = &mut config.strategy;
        strategy.python_module = Some("fixture_strategy".into());
        strategy.python_strategy_spec = Some(serde_json::json!({"data_level": "bar"}));
        strategy.instrument = Some("BTCUSDT.BINANCE".into());

        let checkpoint =
            |warmup_completed: serde_json::Value| qx_strategy::contract::StrategyCheckpointV1 {
                schema_version: 1,
                strategy_id: "fixture".into(),
                strategy_version: "v1".into(),
                adapter_state: serde_json::json!({
                    "warmup_completed": warmup_completed,
                }),
                strategy_state: serde_json::json!({}),
            };
        assert!(!runtime_python_warmup_complete(
            strategy,
            Some(&checkpoint(serde_json::json!([]))),
            &[],
        )
        .unwrap());
        assert!(runtime_python_warmup_complete(
            strategy,
            Some(&checkpoint(serde_json::json!(["BTCUSDT.BINANCE"]))),
            &[],
        )
        .unwrap());
        assert!(!runtime_python_warmup_complete(
            strategy,
            Some(&checkpoint(serde_json::json!(["ETHUSDT.BINANCE"]))),
            &[],
        )
        .unwrap());
        assert!(!runtime_python_warmup_complete(
            strategy,
            Some(&checkpoint(serde_json::json!(["BTCUSDT.BINANCE"]))),
            &["BTCUSDT.BINANCE".into(), "ETHUSDT.BINANCE".into()],
        )
        .unwrap());
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
