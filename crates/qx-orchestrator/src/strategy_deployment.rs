//! Runtime-owned execution of durable strategy deployment operations.

use crate::reap::ManagedChild;
use crate::supervisor_stop::{ManagedProcess, SupervisorStop};
use qx_runtime::strategy_deployment_store::StrategyDeploymentStore;
use qx_runtime::{
    runtime_config_digest, RuntimeConfig, StrategyWorkerObservationV1, StrategyWorkerObservationV3,
    StrategyWorkerPhase, WorkerRole, WORKER_STOP_CHANNEL_ENV, WORKER_STOP_CHANNEL_STDIN_EOF,
};
use qx_spec::{StrategyDeploymentActionV1, StrategyDeploymentOperationStateV1};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

type ActiveDeploymentState = (
    RuntimeConfig,
    std::collections::BTreeMap<String, u64>,
    std::collections::BTreeMap<String, PathBuf>,
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrategyLifecycleAction {
    Pause,
    Resume,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StrategyLifecycleCommandOutcome {
    Pending,
    Executed,
    Failed(String),
}

/// Service-owned adapter for the authenticated control plane and durable command queue.
pub trait StrategyLifecyclePort {
    fn submit(
        &mut self,
        operation_id: &str,
        worker_id: &str,
        operator_id: &str,
        action: StrategyLifecycleAction,
    ) -> Result<u64, String>;
    fn outcome(&mut self, command_id: u64) -> Result<StrategyLifecycleCommandOutcome, String>;
}

pub(crate) fn write_supervisor_snapshot(
    source_path: &Path,
    config: &RuntimeConfig,
) -> Result<PathBuf, String> {
    let snapshot =
        source_path.with_file_name(format!(".strategy-supervisor-{}.json", std::process::id()));
    write_atomic(&snapshot, config.to_json()?.as_bytes())?;
    Ok(snapshot)
}

pub(crate) fn recover_active_config(
    base: &RuntimeConfig,
    work_dir: &Path,
    executable: &Path,
) -> Result<ActiveDeploymentState, String> {
    let effective_data_dir = resolve(work_dir, &base.storage.data_dir);
    let store = StrategyDeploymentStore::configured(effective_data_dir, base)?;
    let mut effective = base.clone();
    let mut active_revisions = std::collections::BTreeMap::new();
    let mut active_bundles = std::collections::BTreeMap::new();
    let strategy_workers = base
        .workers
        .iter()
        .filter(|worker| worker.role == WorkerRole::Strategy)
        .map(|worker| worker.id.clone())
        .collect::<Vec<_>>();
    for worker_id in strategy_workers {
        let Some(operation) = store.latest_successful_operation(&worker_id)? else {
            continue;
        };
        let revision = store
            .revision(&worker_id, operation.target_revision)?
            .ok_or_else(|| "successful deployment references a missing revision".to_string())?;
        active_revisions.insert(worker_id.clone(), revision.revision);
        let bundle = store.verify_revision_bundle(&revision)?;
        active_bundles.insert(worker_id.clone(), bundle.clone());
        let revision_config =
            RuntimeConfig::from_json(revision.runtime_config_json.as_deref().ok_or_else(
                || "successful deployment has no frozen runtime config".to_string(),
            )?)?;
        let revision_worker = revision_config
            .workers
            .iter()
            .find(|worker| worker.id == worker_id && worker.role == WorkerRole::Strategy)
            .ok_or_else(|| format!("deployment config omits worker {worker_id}"))?;
        if revision_worker.role != WorkerRole::Strategy
            || revision_worker.enabled != revision.desired_running
        {
            return Err(format!(
                "stored deployment identity or desired state is invalid for {worker_id}"
            ));
        }
        let strategy = revision_config.strategy_for_worker(&worker_id)?;
        if strategy.builtin_strategy.is_some() {
            let host = std::fs::File::open(executable).map_err(|error| error.to_string())?;
            let digest =
                qx_strategy::file_digest::sha256_reader_hex_bounded(host, 256 * 1024 * 1024)?;
            if !digest.eq_ignore_ascii_case(&revision.bundle_sha256) {
                return Err(format!(
                    "stored built-in bundle for {worker_id} does not match this supervisor executable"
                ));
            }
        }
        effective = merge_revision_config(
            &effective,
            &revision_config,
            &worker_id,
            revision.desired_running,
        )?;
    }
    if effective.config_fingerprint.is_some() {
        effective.config_fingerprint = Some(effective.fingerprint()?);
    }
    effective.to_json()?;
    Ok((effective, active_revisions, active_bundles))
}

fn merge_revision_config(
    base: &RuntimeConfig,
    revision_config: &RuntimeConfig,
    worker_id: &str,
    desired_running: bool,
) -> Result<RuntimeConfig, String> {
    let mut merged = base.clone();
    let worker_index = merged
        .workers
        .iter()
        .position(|worker| worker.id == worker_id && worker.role == WorkerRole::Strategy)
        .ok_or_else(|| format!("configured Strategy worker {worker_id} disappeared"))?;
    let revision_worker = revision_config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id && worker.role == WorkerRole::Strategy)
        .ok_or_else(|| format!("deployment config omits Strategy worker {worker_id}"))?;
    if revision_worker.enabled != desired_running
        || merged.workers[worker_index].account_id != revision_worker.account_id
        || merged.workers[worker_index].venue_id != revision_worker.venue_id
    {
        return Err(format!(
            "deployment identity or desired state is invalid for {worker_id}"
        ));
    }
    merged.workers[worker_index] = revision_worker.clone();
    let revision_strategy = revision_config.strategy_for_worker(worker_id)?;
    if base.strategies.is_empty() {
        if base.strategy.account_id != revision_strategy.account_id
            || base.strategy.venue_id != revision_strategy.venue_id
        {
            return Err(format!(
                "deployment changes account or venue for {worker_id}"
            ));
        }
        merged.strategy = revision_strategy;
    } else {
        let strategy_index = merged
            .strategies
            .iter()
            .position(|strategy| strategy.id.as_deref() == Some(worker_id))
            .ok_or_else(|| format!("runtime has no strategy instance for {worker_id}"))?;
        let current = &merged.strategies[strategy_index];
        if current.account_id != revision_strategy.account_id
            || current.venue_id != revision_strategy.venue_id
        {
            return Err(format!(
                "deployment changes account or venue for {worker_id}"
            ));
        }
        merged.strategies[strategy_index] = revision_strategy;
    }
    if merged.config_fingerprint.is_some() {
        merged.config_fingerprint = Some(merged.fingerprint()?);
    }
    merged.to_json()?;
    Ok(merged)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn supervise_with_deployments(
    children: &mut Vec<ManagedChild>,
    config: &RuntimeConfig,
    config_path: &Path,
    executable: &Path,
    work_dir: &Path,
    log_dir: &Path,
    lifecycle_port: &mut dyn StrategyLifecyclePort,
    active_config_paths: &mut std::collections::BTreeMap<String, PathBuf>,
    active_revisions: &mut std::collections::BTreeMap<String, u64>,
    active_bundles: &mut std::collections::BTreeMap<String, PathBuf>,
) -> Result<SupervisorStop, String> {
    let data_dir = resolve(work_dir, &config.storage.data_dir);
    let store = StrategyDeploymentStore::configured(data_dir, config)?;
    let mut active_config = config.clone();
    loop {
        if qx_runtime::shutdown_signalled() {
            return stop_for_shutdown(children, config.shutdown_timeout_ms);
        }
        for child in children.iter_mut() {
            if let Some(exit) = child.poll_exit()? {
                return Ok(SupervisorStop::WorkerExited {
                    id: child.id.clone(),
                    code: exit.raw_code,
                    label: exit.label,
                });
            }
        }
        if let Some(operation) = store.accepted_operations()?.into_iter().next() {
            if let Err(error) = execute_operation(
                children,
                config,
                config_path,
                executable,
                work_dir,
                log_dir,
                &store,
                &operation,
                lifecycle_port,
                active_config_paths,
                &mut active_config,
                active_revisions,
                active_bundles,
            ) {
                if error.starts_with("FENCE_FAILED:") {
                    return Err(error);
                }
                if error.starts_with("RETRY_PENDING:") {
                    eprintln!(
                        "[策略部署] operation={} remains accepted: {error}",
                        operation.operation_id
                    );
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
                store.finish_operation(
                    &operation.operation_id,
                    StrategyDeploymentOperationStateV1::Failed,
                    now_ms(),
                    Some("STRATEGY_DEPLOYMENT_FAILED".into()),
                    Some(error.clone()),
                )?;
                eprintln!(
                    "[策略部署] operation={} failed: {error}",
                    operation.operation_id
                );
            }
            continue;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_operation(
    children: &mut Vec<ManagedChild>,
    base: &RuntimeConfig,
    config_path: &Path,
    executable: &Path,
    work_dir: &Path,
    log_dir: &Path,
    store: &StrategyDeploymentStore,
    operation: &qx_spec::StrategyDeploymentOperationV1,
    lifecycle_port: &mut dyn StrategyLifecyclePort,
    active_config_paths: &mut std::collections::BTreeMap<String, PathBuf>,
    active_config: &mut RuntimeConfig,
    active_revisions: &mut std::collections::BTreeMap<String, u64>,
    active_bundles: &mut std::collections::BTreeMap<String, PathBuf>,
) -> Result<(), String> {
    base.workers
        .iter()
        .find(|worker| worker.id == operation.worker_id && worker.role == WorkerRole::Strategy)
        .ok_or_else(|| {
            format!(
                "{} is not a configured Strategy worker",
                operation.worker_id
            )
        })?;
    let revision = store
        .revision(&operation.worker_id, operation.target_revision)?
        .ok_or_else(|| "deployment revision disappeared".to_string())?;
    let target_config = RuntimeConfig::from_json(
        revision
            .runtime_config_json
            .as_deref()
            .ok_or_else(|| "deployment revision has no frozen runtime config".to_string())?,
    )?;
    if target_config
        .workers
        .iter()
        .find(|candidate| candidate.id == operation.worker_id)
        .is_none_or(|candidate| {
            candidate.role != WorkerRole::Strategy || candidate.enabled != revision.desired_running
        })
    {
        return Err("deployment revision worker state does not match its desired state".into());
    }
    let index = children
        .iter()
        .position(|child| child.id == operation.worker_id);
    let mut active_config_update = None;
    let mut active_bundle_update = None;
    match operation.action {
        StrategyDeploymentActionV1::Pause | StrategyDeploymentActionV1::Resume => {
            if !revision.desired_running || index.is_none() {
                return Err("pause/resume requires a running strategy worker revision".into());
            }
            if active_revisions.get(&operation.worker_id) != Some(&revision.revision) {
                return Err(
                    "pause/resume must target the currently deployed strategy revision".into(),
                );
            }
            let data_root = resolve(work_dir, &base.storage.data_dir);
            let requested_at_ms = now_ms();
            let action = if operation.action == StrategyDeploymentActionV1::Pause {
                StrategyLifecycleAction::Pause
            } else {
                StrategyLifecycleAction::Resume
            };
            if action == StrategyLifecycleAction::Resume {
                let strategy = target_config.strategy_for_worker(&operation.worker_id)?;
                require_live_rollback_reconciliation(&strategy, &data_root)?;
            }
            let command_id = lifecycle_port.submit(
                &operation.operation_id,
                &operation.worker_id,
                &operation.requested_by,
                action,
            )?;
            let child_index = index
                .ok_or_else(|| "strategy worker stopped during lifecycle command".to_string())?;
            let pid = children[child_index].child.id();
            let expected_phase = if action == StrategyLifecycleAction::Pause {
                StrategyWorkerPhase::Paused
            } else {
                StrategyWorkerPhase::Running
            };
            wait_for_lifecycle_transition(
                lifecycle_port,
                command_id,
                &operation.worker_id,
                pid,
                expected_phase,
                requested_at_ms,
                base.messaging.worker_stale_after_ms,
                &data_root,
                &mut children[child_index],
            )?;
        }
        StrategyDeploymentActionV1::Stop => {
            if revision.desired_running {
                return Err("stop operation must target a stopped revision".into());
            }
            let stopped_config =
                merge_revision_config(active_config, &target_config, &operation.worker_id, false)?;
            if let Some(index) = index {
                drain_one(&mut children[index], base.shutdown_timeout_ms)
                    .map_err(|error| format!("FENCE_FAILED: {error}"))?;
                children.remove(index);
            }
            active_config_update = Some(stopped_config);
            active_bundle_update = Some(store.verify_revision_bundle(&revision)?);
        }
        StrategyDeploymentActionV1::Deploy | StrategyDeploymentActionV1::Rollback => {
            if !revision.desired_running {
                return Err("deploy/rollback operation must target a running revision".into());
            }
            if operation.action == StrategyDeploymentActionV1::Rollback {
                let active_revision = active_revisions
                    .get(&operation.worker_id)
                    .copied()
                    .ok_or_else(|| {
                        "rollback requires a currently deployed source revision".to_string()
                    })?;
                if revision.revision <= active_revision {
                    return Err(format!(
                        "rollback target must be a new revision greater than active revision {active_revision}; create a new revision from the selected prior configuration and bind any checkpoint migration to that transition"
                    ));
                }
            }
            let bundle = store.verify_revision_bundle(&revision)?;
            let next_config =
                merge_revision_config(active_config, &target_config, &operation.worker_id, true)?;
            let selected_strategy = next_config.strategy_for_worker(&operation.worker_id)?;
            if selected_strategy.builtin_strategy.is_some() {
                let host = std::fs::File::open(executable).map_err(|error| error.to_string())?;
                let host_digest =
                    qx_strategy::file_digest::sha256_reader_hex_bounded(host, 256 * 1024 * 1024)?;
                if !host_digest.eq_ignore_ascii_case(&revision.bundle_sha256) {
                    return Err(
                        "built-in deployment bundle does not match the supervisor executable"
                            .into(),
                    );
                }
            }
            let mut rollback_starts_paused = false;
            if operation.action == StrategyDeploymentActionV1::Rollback {
                let data_root = resolve(work_dir, &base.storage.data_dir);
                let active_strategy = active_config.strategy_for_worker(&operation.worker_id)?;
                if active_strategy.live_enabled {
                    if let Some(child_index) = index {
                        let pause_operation = format!("{}:rollback-pause", operation.operation_id);
                        transition_strategy_lifecycle(
                            lifecycle_port,
                            &pause_operation,
                            &operation.worker_id,
                            &operation.requested_by,
                            StrategyLifecycleAction::Pause,
                            base.messaging.worker_stale_after_ms,
                            &data_root,
                            &mut children[child_index],
                        )
                        .map_err(|error| {
                            format!("FENCE_FAILED: cannot confirm Live worker pause before rollback: {error}")
                        })?;
                        rollback_starts_paused = true;
                    }
                }
                let reconciliation_strategy = if active_strategy.live_enabled {
                    &active_strategy
                } else {
                    &selected_strategy
                };
                if let Err(reconciliation_error) =
                    require_live_rollback_reconciliation(reconciliation_strategy, &data_root)
                {
                    if active_strategy.live_enabled {
                        if let Some(child_index) = index {
                            let resume_operation = format!(
                                "{}:rollback-reconciliation-resume",
                                operation.operation_id
                            );
                            transition_strategy_lifecycle(
                                lifecycle_port,
                                &resume_operation,
                                &operation.worker_id,
                                &operation.requested_by,
                                StrategyLifecycleAction::Resume,
                                base.messaging.worker_stale_after_ms,
                                &data_root,
                                &mut children[child_index],
                            )
                            .map_err(|error| {
                                format!("FENCE_FAILED: rollback reconciliation failed ({reconciliation_error}) and previous worker could not be resumed: {error}")
                            })?;
                        }
                    }
                    return Err(reconciliation_error);
                }
            }
            let had_previous_worker = index.is_some();
            let previous_config_path = active_config_paths.get(&operation.worker_id).cloned();
            if let Some(index) = index {
                drain_one(&mut children[index], base.shutdown_timeout_ms)
                    .map_err(|error| format!("FENCE_FAILED: {error}"))?;
                children.remove(index);
            }
            let path = deployment_config_path(config_path, &operation.worker_id, revision.revision);
            let replacement = (|| {
                write_atomic(&path, next_config.to_json()?.as_bytes())?;
                launch_strategy_from_config_path(
                    children,
                    &path,
                    &operation.worker_id,
                    executable,
                    work_dir,
                    log_dir,
                    base.messaging.worker_stale_after_ms,
                    base.shutdown_timeout_ms,
                    Some(revision.revision),
                    Some(&bundle),
                    if rollback_starts_paused {
                        StrategyWorkerPhase::Paused
                    } else {
                        StrategyWorkerPhase::Running
                    },
                )
            })();
            if let Err(error) = replacement {
                if error.starts_with("FENCE_FAILED:") {
                    return Err(error);
                }
                if had_previous_worker {
                    let previous_path = previous_config_path.ok_or_else(|| {
                        "FENCE_FAILED: previous strategy config snapshot is unavailable".to_string()
                    })?;
                    launch_strategy_from_config_path(
                        children,
                        &previous_path,
                        &operation.worker_id,
                        executable,
                        work_dir,
                        log_dir,
                        base.messaging.worker_stale_after_ms,
                        base.shutdown_timeout_ms,
                        // The replacement may have advanced the durable strategy
                        // cursor before its host failed. Restore the previous config
                        // under the attempted revision so the cursor remains monotonic.
                        Some(revision.revision),
                        active_bundles.get(&operation.worker_id).map(PathBuf::as_path),
                        if rollback_starts_paused {
                            StrategyWorkerPhase::Paused
                        } else {
                            StrategyWorkerPhase::Running
                        },
                    )
                    .map_err(|restore| {
                        format!(
                            "FENCE_FAILED: replacement failed ({error}); previous strategy restoration failed ({restore})"
                        )
                    })?;
                    return Err(format!(
                        "replacement failed and previous strategy revision was restored: {error}"
                    ));
                }
                return Err(error);
            }
            active_config_paths.insert(operation.worker_id.clone(), path);
            active_config_update = Some(next_config);
            active_bundle_update = Some(bundle);
        }
    }
    store.finish_operation(
        &operation.operation_id,
        StrategyDeploymentOperationStateV1::Succeeded,
        now_ms(),
        None,
        None,
    )?;
    if let Some(next_config) = active_config_update {
        *active_config = next_config;
    }
    if matches!(
        operation.action,
        StrategyDeploymentActionV1::Deploy
            | StrategyDeploymentActionV1::Rollback
            | StrategyDeploymentActionV1::Stop
    ) {
        active_revisions.insert(operation.worker_id.clone(), revision.revision);
    }
    if let Some(bundle) = active_bundle_update {
        active_bundles.insert(operation.worker_id.clone(), bundle);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn launch_strategy_from_config_path(
    children: &mut Vec<ManagedChild>,
    config_path: &Path,
    worker_id: &str,
    executable: &Path,
    work_dir: &Path,
    log_dir: &Path,
    observation_timeout_ms: u64,
    shutdown_timeout_ms: u64,
    deployment_revision: Option<u64>,
    bundle_path: Option<&Path>,
    expected_phase: StrategyWorkerPhase,
) -> Result<(), String> {
    let config = RuntimeConfig::from_json(
        &std::fs::read_to_string(config_path).map_err(|error| error.to_string())?,
    )?;
    let launch = crate::plan_workers(&config, config_path, true)?
        .into_iter()
        .find(|launch| launch.worker_id == worker_id)
        .ok_or_else(|| format!("deployment config has no managed worker {worker_id}"))?;
    let stdout_path = log_dir.join(format!("{worker_id}.out.log"));
    let stderr_path = log_dir.join(format!("{worker_id}.err.log"));
    let stdout = crate::process_logs::prepare(&stdout_path).map_err(|error| error.to_string())?;
    let stderr = crate::process_logs::prepare(&stderr_path).map_err(|error| error.to_string())?;
    let mut process = Command::new(executable);
    process
        .args(launch.args)
        .current_dir(work_dir)
        .env(WORKER_STOP_CHANNEL_ENV, WORKER_STOP_CHANNEL_STDIN_EOF)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(revision) = deployment_revision {
        process.env(
            qx_runtime::STRATEGY_DEPLOYMENT_REVISION_ENV,
            revision.to_string(),
        );
    }
    if let Some(bundle_path) = bundle_path {
        process.env(qx_runtime::STRATEGY_DEPLOYMENT_BUNDLE_ENV, bundle_path);
    }
    let mut child = process
        .spawn()
        .map_err(|error| format!("cannot start strategy worker: {error}"))?;
    let pid = child.id();
    let spawned_at_ms = now_ms();
    let mut log_drainers = Vec::with_capacity(2);
    if let Some(pipe) = child.stdout.take() {
        log_drainers.push(crate::process_logs::pump(
            pipe,
            stdout_path,
            stdout,
            worker_id.to_string(),
            "stdout",
        ));
    }
    if let Some(pipe) = child.stderr.take() {
        log_drainers.push(crate::process_logs::pump(
            pipe,
            stderr_path,
            stderr,
            worker_id.to_string(),
            "stderr",
        ));
    }
    let stop_channel = child.stdin.take();
    children.push(ManagedChild {
        id: worker_id.to_string(),
        child,
        stop_channel,
        log_drainers,
    });
    if let Some(child) = children
        .last_mut()
        .filter(|child| child.stop_channel.is_none())
    {
        stop_one(child, shutdown_timeout_ms).map_err(|error| {
            format!("FENCE_FAILED: cannot recover a child without stop channel: {error}")
        })?;
        children.pop();
        return Err("strategy worker stdin stop channel is unavailable".into());
    }
    let index = children.len() - 1;
    if let Err(error) = wait_for_observation(
        &config,
        worker_id,
        pid,
        spawned_at_ms,
        observation_timeout_ms,
        &resolve(work_dir, &config.storage.data_dir),
        &mut children[index],
        expected_phase,
    ) {
        stop_one(&mut children[index], shutdown_timeout_ms)
            .map_err(|stop_error| format!("FENCE_FAILED: {stop_error}"))?;
        children.pop();
        return Err(error);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn wait_for_observation(
    config: &RuntimeConfig,
    worker_id: &str,
    pid: u32,
    spawned_at_ms: u64,
    timeout_ms: u64,
    data_dir: &Path,
    child: &mut ManagedChild,
    expected_phase: StrategyWorkerPhase,
) -> Result<(), String> {
    let digest = runtime_config_digest(config)?;
    let directory = data_dir
        .join("strategy-observations")
        .join(&qx_strategy::sha256_hex(worker_id.as_bytes())[..32]);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));
    loop {
        if directory.exists() {
            let entries = std::fs::read_dir(&directory).map_err(|error| error.to_string())?;
            for (count, entry) in entries.enumerate() {
                if count >= 1024 {
                    return Err("too many strategy observation owners".into());
                }
                let path = entry.map_err(|error| error.to_string())?.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
                if metadata.len() > 64 * 1024 {
                    return Err("strategy observation exceeds size limit".into());
                }
                let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
                let observation: StrategyWorkerObservationV3 =
                    serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                if observation.schema_version == 3
                    && observation
                        .host_lifecycle
                        .as_ref()
                        .is_some_and(|lifecycle| {
                            lifecycle.schema_version == 2 && lifecycle.constructed
                        })
                    && !observation.owner_id.trim().is_empty()
                    && observation.worker_id == worker_id
                    && observation.process_id == pid
                    && observation.config_sha256 == digest
                    && observation.observed_at_ms >= spawned_at_ms
                    && observation.phase == expected_phase
                {
                    return Ok(());
                }
            }
        }
        if let Some(exit) = child.poll_exit()? {
            return Err(format!(
                "replacement strategy worker exited before becoming ready ({})",
                exit.label
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "replacement strategy worker did not report {expected_phase:?} with its exact config digest"
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[allow(clippy::too_many_arguments)]
fn wait_for_lifecycle_transition(
    port: &mut dyn StrategyLifecyclePort,
    command_id: u64,
    worker_id: &str,
    pid: u32,
    phase: StrategyWorkerPhase,
    requested_at_ms: u64,
    timeout_ms: u64,
    data_dir: &Path,
    child: &mut ManagedChild,
) -> Result<(), String> {
    let directory = data_dir
        .join("strategy-observations")
        .join(&qx_strategy::sha256_hex(worker_id.as_bytes())[..32]);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));
    let mut command_executed = false;
    loop {
        match port.outcome(command_id)? {
            StrategyLifecycleCommandOutcome::Pending => {}
            StrategyLifecycleCommandOutcome::Executed => command_executed = true,
            StrategyLifecycleCommandOutcome::Failed(message) => {
                return Err(format!("strategy lifecycle command failed: {message}"));
            }
        }
        if command_executed && directory.exists() {
            let entries = std::fs::read_dir(&directory).map_err(|error| error.to_string())?;
            for (count, entry) in entries.enumerate() {
                if count >= 1024 {
                    return Err("too many strategy observation owners".into());
                }
                let path = entry.map_err(|error| error.to_string())?.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
                if metadata.len() > 64 * 1024 {
                    return Err("strategy observation exceeds size limit".into());
                }
                let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
                let version = serde_json::from_slice::<serde_json::Value>(&bytes)
                    .map_err(|error| error.to_string())?["schema_version"]
                    .as_u64()
                    .unwrap_or_default();
                let matches_transition = if version <= 2 {
                    let observation: StrategyWorkerObservationV1 =
                        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                    (1..=2).contains(&observation.schema_version)
                        && !observation.owner_id.trim().is_empty()
                        && observation.worker_id == worker_id
                        && observation.process_id == pid
                        && observation.observed_at_ms >= requested_at_ms
                        && observation.phase == phase
                } else {
                    let observation: StrategyWorkerObservationV3 =
                        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                    observation.schema_version == 3
                        && !observation.owner_id.trim().is_empty()
                        && observation.worker_id == worker_id
                        && observation.process_id == pid
                        && observation.observed_at_ms >= requested_at_ms
                        && observation.phase == phase
                };
                if matches_transition {
                    return Ok(());
                }
            }
        }
        if let Some(exit) = child.poll_exit()? {
            return Err(format!(
                "strategy worker exited before lifecycle state was observed ({})",
                exit.label
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "RETRY_PENDING: strategy worker did not observe {:?} after its control command",
                phase
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[allow(clippy::too_many_arguments)]
fn transition_strategy_lifecycle(
    port: &mut dyn StrategyLifecyclePort,
    operation_id: &str,
    worker_id: &str,
    operator_id: &str,
    action: StrategyLifecycleAction,
    timeout_ms: u64,
    data_dir: &Path,
    child: &mut ManagedChild,
) -> Result<(), String> {
    let requested_at_ms = now_ms();
    let command_id = port.submit(operation_id, worker_id, operator_id, action)?;
    let phase = match action {
        StrategyLifecycleAction::Pause => StrategyWorkerPhase::Paused,
        StrategyLifecycleAction::Resume => StrategyWorkerPhase::Running,
    };
    wait_for_lifecycle_transition(
        port,
        command_id,
        worker_id,
        child.child.id(),
        phase,
        requested_at_ms,
        timeout_ms,
        data_dir,
        child,
    )
}

fn stop_for_shutdown(
    children: &mut [ManagedChild],
    timeout_ms: u64,
) -> Result<SupervisorStop, String> {
    let started = Instant::now();
    for child in children.iter_mut() {
        child.request_stop()?;
    }
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let mut remaining = 0;
        for child in children.iter_mut() {
            if child.poll_exit()?.is_none() {
                remaining += 1;
            }
        }
        if remaining == 0 {
            for child in children.iter_mut() {
                child.finish_log_drainers()?;
            }
            return Ok(SupervisorStop::StoppedWithinBudget {
                waited_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            });
        }
        if Instant::now() >= deadline {
            return Ok(SupervisorStop::StopTimedOut {
                waited_ms: timeout_ms,
                remaining,
            });
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn stop_one(child: &mut ManagedChild, timeout_ms: u64) -> Result<(), String> {
    child.request_stop()?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if child.poll_exit()?.is_some() {
            child.finish_log_drainers()?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    child
        .child
        .kill()
        .map_err(|error| format!("cannot stop strategy worker: {error}"))?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));
    loop {
        if child.poll_exit()?.is_some() {
            child.finish_log_drainers()?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("strategy worker did not exit after kill".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn drain_one(child: &mut ManagedChild, timeout_ms: u64) -> Result<(), String> {
    child.request_stop()?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if child.poll_exit()?.is_some() {
            child.finish_log_drainers()?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "strategy worker {} did not complete graceful drain within {timeout_ms} ms",
                child.id
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn deployment_config_path(base: &Path, worker_id: &str, revision: u64) -> PathBuf {
    let safe_id = qx_strategy::sha256_hex(worker_id.as_bytes());
    base.with_file_name(format!(
        ".strategy-deploy-{}-{revision}.json",
        &safe_id[..24]
    ))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if path.exists() {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.file_type().is_file() {
            return Err("strategy deployment config target is not a regular file".into());
        }
        let existing = std::fs::read(path).map_err(|error| error.to_string())?;
        return if existing == bytes {
            Ok(())
        } else {
            Err("strategy deployment config path already contains different bytes".into())
        };
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("{}-{nonce}.tmp", std::process::id()));
    let result = (|| {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        match std::fs::rename(&temporary, path) {
            Ok(()) => Ok(()),
            Err(_)
                if path.exists() && std::fs::read(path).is_ok_and(|current| current == bytes) =>
            {
                let _ = std::fs::remove_file(&temporary);
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn resolve(work_dir: &Path, configured: &str) -> PathBuf {
    let path = Path::new(configured);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        work_dir.join(path)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn require_live_rollback_reconciliation(
    strategy: &qx_runtime::StrategyRuntimeConfig,
    data_root: &Path,
) -> Result<(), String> {
    if !strategy.live_enabled {
        return Ok(());
    }
    let account_id = strategy
        .account_id
        .as_deref()
        .ok_or_else(|| "LIVE_READINESS_ACCOUNT_MISSING".to_string())?;
    let venue_id = strategy
        .venue_id
        .as_deref()
        .ok_or_else(|| "LIVE_READINESS_VENUE_MISSING".to_string())?;
    let reports = qx_runtime::load_reconcile_reports(data_root)?;
    qx_runtime::require_fresh_clean_reconciliation(
        &reports,
        account_id,
        venue_id,
        now_ms(),
        qx_runtime::LIVE_RECONCILE_MAX_AGE_MS,
    )
}

#[cfg(test)]
mod reconciliation_gate_tests {
    use super::*;
    use qx_runtime::{ReconcileReportSnapshot, StrategyRuntimeConfig};

    fn isolated_root(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("r13-reconciliation-gate-{label}-{nonce}"));
        std::fs::create_dir_all(root.join("reconcile")).unwrap();
        root
    }

    fn live_strategy() -> StrategyRuntimeConfig {
        StrategyRuntimeConfig {
            live_enabled: true,
            account_id: Some("account-r13".into()),
            venue_id: Some("paper-r13".into()),
            ..StrategyRuntimeConfig::default()
        }
    }

    fn write_report(root: &Path, report: ReconcileReportSnapshot) {
        let path = root.join("reconcile").join("reconcile-r13.json");
        std::fs::write(path, serde_json::to_vec(&report).unwrap()).unwrap();
    }

    fn report(now: u64) -> ReconcileReportSnapshot {
        ReconcileReportSnapshot {
            schema_version: 1,
            worker_id: "reconcile-r13".into(),
            account_id: "account-r13".into(),
            venue_id: "PAPER-R13".into(),
            observed_ts: now,
            order_issues: Vec::new(),
            balances_count: 1,
            balance_discrepancies: Vec::new(),
            position_snapshots_count: Some(1),
            funding_rate_snapshots_count: None,
            cashflow_count: None,
        }
    }

    #[test]
    fn live_rollback_and_resume_gate_requires_matching_fresh_clean_reconciliation() {
        let root = isolated_root("matrix");
        let strategy = live_strategy();
        let missing = require_live_rollback_reconciliation(&strategy, &root).unwrap_err();
        assert!(
            missing.contains("LIVE_READINESS_RECONCILE_MISSING"),
            "{missing}"
        );

        let now = now_ms();
        let mut stale = report(now.saturating_sub(qx_runtime::LIVE_RECONCILE_MAX_AGE_MS + 1));
        stale.worker_id = "stale-r13".into();
        write_report(&root, stale);
        let stale_error = require_live_rollback_reconciliation(&strategy, &root).unwrap_err();
        assert!(
            stale_error.contains("LIVE_READINESS_RECONCILE_STALE"),
            "{stale_error}"
        );

        let mut wrong_identity = report(now);
        wrong_identity.account_id = "other-account".into();
        write_report(&root, wrong_identity);
        let identity_error = require_live_rollback_reconciliation(&strategy, &root).unwrap_err();
        assert!(
            identity_error.contains("LIVE_READINESS_RECONCILE_MISSING"),
            "{identity_error}"
        );

        let mut discrepant = report(now);
        discrepant
            .order_issues
            .push(serde_json::json!({"order_id": "unmatched"}));
        write_report(&root, discrepant);
        let discrepancy_error = require_live_rollback_reconciliation(&strategy, &root).unwrap_err();
        assert!(
            discrepancy_error.contains("LIVE_READINESS_RECONCILE_DISCREPANCY"),
            "{discrepancy_error}"
        );

        write_report(&root, report(now_ms()));
        require_live_rollback_reconciliation(&strategy, &root)
            .expect("matching fresh clean reconciliation admits live rollback/resume");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn paper_rollback_and_resume_do_not_require_live_reconciliation() {
        let root = isolated_root("paper");
        require_live_rollback_reconciliation(&StrategyRuntimeConfig::default(), &root)
            .expect("Paper operations do not use the live reconciliation gate");
        std::fs::remove_dir_all(root).unwrap();
    }
}
