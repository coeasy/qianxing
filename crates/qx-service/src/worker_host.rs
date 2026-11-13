//! Supervised runtime worker host shared by CLI and future daemon facades.

use std::path::Path;
use std::sync::Arc;

use crate::configured_backends::{
    configured_command_queue, configured_control_store, ControlStateBackend,
};
use qx_control::{CommandKind, CommandStatus, ControlCommand, Permission};
use qx_runtime::{RuntimeConfig, RuntimeSupervisor, WorkerConfig, WorkerContext, WorkerRole};
use qx_storage::ControlCommandQueueBackend;

struct RuntimeStrategyLifecyclePort {
    control: ControlStateBackend,
    queue: Arc<dyn ControlCommandQueueBackend>,
}

impl qx_orchestrator::StrategyLifecyclePort for RuntimeStrategyLifecyclePort {
    fn submit(
        &mut self,
        operation_id: &str,
        worker_id: &str,
        operator_id: &str,
        action: qx_orchestrator::StrategyLifecycleAction,
    ) -> Result<u64, String> {
        let request_id = format!("strategy-deployment:{operation_id}");
        let digest = qx_strategy::sha256_hex(request_id.as_bytes());
        let command_id = u64::from_str_radix(&digest[..16], 16)
            .map_err(|_| "cannot derive lifecycle command ID from SHA-256".to_string())?
            .max(1);
        let command = ControlCommand {
            command_id,
            request_id,
            operator_id: operator_id.to_string(),
            reason: format!("strategy deployment operation {operation_id}"),
            kind: match action {
                qx_orchestrator::StrategyLifecycleAction::Pause => CommandKind::PauseStrategy,
                qx_orchestrator::StrategyLifecycleAction::Resume => CommandKind::ResumeStrategy,
            },
            target: worker_id.to_string(),
            payload: Default::default(),
            permission: Permission::Trading,
            dry_run: false,
        };
        let current = self.control.load()?.command(command_id).cloned();
        if let Some(existing) = current {
            if existing != command {
                return Err(format!(
                    "lifecycle command ID collision for operation {operation_id}"
                ));
            }
        } else {
            let persisted = self
                .control
                .transact(|plane| plane.submit_as(command.clone(), Permission::Admin, now_ms()))?;
            persisted
                .1
                .map_err(|error| format!("cannot audit strategy lifecycle command: {error:?}"))?;
        }
        let status = self
            .control
            .load()?
            .audit()
            .iter()
            .rev()
            .find(|record| record.command_id == command_id)
            .map(|record| record.status);
        if status == Some(CommandStatus::Accepted) {
            self.queue
                .enqueue_command(command, qx_runtime::lease_clock(now_ms()))
                .map_err(|error| {
                    format!("RETRY_PENDING: cannot enqueue strategy lifecycle command: {error:?}")
                })?;
        }
        Ok(command_id)
    }

    fn outcome(
        &mut self,
        command_id: u64,
    ) -> Result<qx_orchestrator::StrategyLifecycleCommandOutcome, String> {
        let plane = self.control.load()?;
        let record = plane
            .audit()
            .iter()
            .rev()
            .find(|record| record.command_id == command_id)
            .ok_or_else(|| {
                format!("strategy lifecycle command {command_id} has no audit record")
            })?;
        Ok(match record.status {
            CommandStatus::Accepted => qx_orchestrator::StrategyLifecycleCommandOutcome::Pending,
            CommandStatus::Executed => qx_orchestrator::StrategyLifecycleCommandOutcome::Executed,
            CommandStatus::Failed => {
                qx_orchestrator::StrategyLifecycleCommandOutcome::Failed(record.result_code.clone())
            }
        })
    }
}

#[cfg(test)]
mod strategy_lifecycle_port_tests {
    fn lifecycle_command_id(operation_id: &str) -> u64 {
        let request_id = format!("strategy-deployment:{operation_id}");
        let digest = qx_strategy::sha256_hex(request_id.as_bytes());
        u64::from_str_radix(&digest[..16], 16).unwrap().max(1)
    }

    #[test]
    fn strategy_deployment_command_id_uses_a_stable_nonzero_digest_prefix() {
        let first = lifecycle_command_id("deploy-1");
        assert_ne!(first, 0);
        assert_eq!(first, lifecycle_command_id("deploy-1"));
        assert_ne!(first, lifecycle_command_id("pause-2"));
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

/// Supervise the configured worker processes from this host process.
///
/// The facade owns runtime configuration loading, command parsing, and executable/path
/// selection. This service host owns the supervision boundary and delegates process lifecycle
/// management to the orchestrator.
pub fn supervise_processes(
    config: &RuntimeConfig,
    config_path: &Path,
    executable: &Path,
    work_dir: &Path,
    allow_unmanaged_roles: bool,
) -> Result<(), qx_orchestrator::SuperviseFailure> {
    let mut backend_config = config.clone();
    if !Path::new(&backend_config.storage.data_dir).is_absolute() {
        backend_config.storage.data_dir = work_dir
            .join(&backend_config.storage.data_dir)
            .to_string_lossy()
            .into_owned();
    }
    if let Some(sqlite_path) = backend_config.storage.sqlite_path.as_mut() {
        if !Path::new(sqlite_path).is_absolute() {
            *sqlite_path = work_dir.join(&*sqlite_path).to_string_lossy().into_owned();
        }
    }
    let control = configured_control_store(&backend_config)
        .map_err(qx_orchestrator::SuperviseFailure::Usage)?;
    let queue_root = Path::new(&backend_config.storage.data_dir);
    let queue = configured_command_queue(&backend_config, queue_root)
        .map_err(qx_orchestrator::SuperviseFailure::Usage)?;
    let mut lifecycle_port = RuntimeStrategyLifecyclePort { control, queue };
    qx_orchestrator::supervise_workers(
        config,
        config_path,
        executable,
        work_dir,
        allow_unmanaged_roles,
        &mut lifecycle_port,
    )
}

/// Run one supervised polling loop with centralized cancellation and bounded sleeping.
///
/// The role-specific tick returns its next delay in milliseconds. Keeping the loop here ensures
/// a worker checks its shutdown token before every tick and cannot spin without yielding between
/// successful polls. `once` executes exactly one tick for CLI one-shot commands.
pub fn run_polling_worker(
    context: &WorkerContext,
    once: bool,
    mut tick: impl FnMut() -> Result<u64, String>,
) -> Result<(), String> {
    while !context.should_stop() {
        let sleep_ms = tick()?;
        if once || context.should_stop() {
            break;
        }
        context.sleep_ms(sleep_ms.max(1));
    }
    Ok(())
}

/// Concrete service host selected for a runtime worker role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerHostComponent {
    RuntimeSupervisor,
    ApiHttpHost,
    VenueRuntime,
    SchedulerRuntime,
    StrategyRuntime,
    OutboxRelayRuntime,
    EventConsumerRuntime,
}

/// The service-level component graph for a role. Domain-specific venue and strategy runners
/// remain injected by the facade; this graph records the shared process host and runner family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkerComponentGraph {
    pub role: WorkerRole,
    pub components: &'static [WorkerHostComponent],
}

const API_GRAPH: &[WorkerHostComponent] = &[
    WorkerHostComponent::RuntimeSupervisor,
    WorkerHostComponent::ApiHttpHost,
];
const VENUE_GRAPH: &[WorkerHostComponent] = &[
    WorkerHostComponent::RuntimeSupervisor,
    WorkerHostComponent::VenueRuntime,
];
const SCHEDULER_GRAPH: &[WorkerHostComponent] = &[
    WorkerHostComponent::RuntimeSupervisor,
    WorkerHostComponent::SchedulerRuntime,
];
const STRATEGY_GRAPH: &[WorkerHostComponent] = &[
    WorkerHostComponent::RuntimeSupervisor,
    WorkerHostComponent::StrategyRuntime,
];
const RELAY_GRAPH: &[WorkerHostComponent] = &[
    WorkerHostComponent::RuntimeSupervisor,
    WorkerHostComponent::OutboxRelayRuntime,
];
const CONSUMER_GRAPH: &[WorkerHostComponent] = &[
    WorkerHostComponent::RuntimeSupervisor,
    WorkerHostComponent::EventConsumerRuntime,
];

/// Resolve every public runtime role to its host and domain-runner family.
pub fn component_graph(role: WorkerRole) -> WorkerComponentGraph {
    let components = match role {
        WorkerRole::Api => API_GRAPH,
        WorkerRole::MarketData
        | WorkerRole::UserStream
        | WorkerRole::Execution
        | WorkerRole::SpreadRecovery
        | WorkerRole::Reconciler => VENUE_GRAPH,
        WorkerRole::Scheduler => SCHEDULER_GRAPH,
        WorkerRole::Strategy => STRATEGY_GRAPH,
        WorkerRole::OutboxRelay => RELAY_GRAPH,
        WorkerRole::EventConsumer => CONSUMER_GRAPH,
    };
    WorkerComponentGraph { role, components }
}

/// Resolve an enabled worker by its configured id and require the expected role.
pub fn configured_worker(
    config: &RuntimeConfig,
    worker_id: &str,
    expected_role: WorkerRole,
) -> Result<WorkerConfig, String> {
    configured_worker_for_roles(config, worker_id, &[expected_role])
}

/// Resolve an enabled worker whose role is one of the roles accepted by a shared entrypoint.
pub fn configured_worker_for_roles(
    config: &RuntimeConfig,
    worker_id: &str,
    expected_roles: &[WorkerRole],
) -> Result<WorkerConfig, String> {
    if expected_roles.is_empty() {
        return Err(format!("worker {worker_id} 没有登记任何可接受角色"));
    }
    if expected_roles.iter().any(|role| {
        let graph = component_graph(*role);
        !graph
            .components
            .contains(&WorkerHostComponent::RuntimeSupervisor)
            || graph
                .components
                .iter()
                .all(|component| *component == WorkerHostComponent::RuntimeSupervisor)
    }) {
        return Err(format!(
            "worker {worker_id} 的服务组件图不完整：{expected_roles:?}"
        ));
    }
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    if !worker.enabled {
        return Err(format!("worker {worker_id} 未启用"));
    }
    if !expected_roles.contains(&worker.role) {
        return Err(format!(
            "worker {worker_id} 角色不匹配：需要 {expected_roles:?}，实际为 {:?}",
            worker.role,
        ));
    }
    Ok(worker.clone())
}

/// Run a configured worker under the runtime shutdown ladder.
///
/// The facade supplies the role-specific loop. This host owns topology validation, worker
/// registration, process signal forwarding, bounded shutdown and result propagation.
pub fn run_worker(
    config: RuntimeConfig,
    worker_id: &str,
    expected_role: WorkerRole,
    role_label: &str,
    run: impl FnOnce(WorkerContext) -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    let supervisor = RuntimeSupervisor::new(config)?;
    let worker = configured_worker(supervisor.config(), worker_id, expected_role)?;
    let handle = supervisor.spawn_worker(&worker.id, run)?;
    qx_runtime::host::join_worker_handle(&supervisor, handle, role_label, &worker.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn runtime_config() -> RuntimeConfig {
        RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .expect("paper strategy runtime fixture is valid")
    }

    #[test]
    fn every_runtime_role_has_a_supervised_component_graph() {
        let cases = [
            (WorkerRole::Api, WorkerHostComponent::ApiHttpHost),
            (WorkerRole::MarketData, WorkerHostComponent::VenueRuntime),
            (WorkerRole::UserStream, WorkerHostComponent::VenueRuntime),
            (WorkerRole::Execution, WorkerHostComponent::VenueRuntime),
            (
                WorkerRole::SpreadRecovery,
                WorkerHostComponent::VenueRuntime,
            ),
            (WorkerRole::Scheduler, WorkerHostComponent::SchedulerRuntime),
            (WorkerRole::Reconciler, WorkerHostComponent::VenueRuntime),
            (WorkerRole::Strategy, WorkerHostComponent::StrategyRuntime),
            (
                WorkerRole::OutboxRelay,
                WorkerHostComponent::OutboxRelayRuntime,
            ),
            (
                WorkerRole::EventConsumer,
                WorkerHostComponent::EventConsumerRuntime,
            ),
        ];
        for (role, runner) in cases {
            let graph = component_graph(role);
            assert_eq!(graph.role, role);
            assert!(graph
                .components
                .contains(&WorkerHostComponent::RuntimeSupervisor));
            assert!(graph.components.contains(&runner), "{role:?}: {graph:?}");
        }
    }

    #[test]
    fn configured_worker_requires_enabled_id_and_matching_role() {
        let mut config = runtime_config();
        let (worker_id, role, expected) = {
            let worker = config
                .workers
                .iter()
                .find(|worker| worker.enabled)
                .expect("fixture has an enabled worker");
            (worker.id.clone(), worker.role, worker.clone())
        };
        assert_eq!(
            configured_worker(&config, &worker_id, role).unwrap(),
            expected
        );
        assert!(configured_worker(&config, "missing-worker", role)
            .unwrap_err()
            .contains("找不到 worker"));
        let other_role = if role == WorkerRole::Api {
            WorkerRole::Scheduler
        } else {
            WorkerRole::Api
        };
        assert!(configured_worker(&config, &worker_id, other_role)
            .unwrap_err()
            .contains("角色不匹配"));
        config
            .workers
            .iter_mut()
            .find(|worker| worker.id == worker_id)
            .expect("selected worker remains configured")
            .enabled = false;
        assert!(configured_worker(&config, &worker_id, role)
            .unwrap_err()
            .contains("未启用"));
    }

    #[test]
    fn configured_worker_accepts_only_registered_roles_for_shared_entrypoints() {
        let mut config = runtime_config();
        let worker = config
            .workers
            .iter_mut()
            .find(|worker| worker.enabled)
            .expect("fixture has an enabled worker");
        worker.role = WorkerRole::SpreadRecovery;
        let worker_id = worker.id.clone();
        assert_eq!(
            configured_worker_for_roles(
                &config,
                &worker_id,
                &[WorkerRole::Execution, WorkerRole::SpreadRecovery]
            )
            .expect("shared paper entry allows its two registered roles")
            .role,
            WorkerRole::SpreadRecovery
        );
        assert!(
            configured_worker_for_roles(&config, &worker_id, &[WorkerRole::Api])
                .unwrap_err()
                .contains("角色不匹配")
        );
        assert!(configured_worker_for_roles(&config, &worker_id, &[])
            .unwrap_err()
            .contains("没有登记任何可接受角色"));
    }

    #[test]
    fn run_worker_propagates_runner_result_through_supervisor() {
        let config = runtime_config();
        let worker = config
            .workers
            .iter()
            .find(|worker| worker.enabled)
            .expect("fixture has an enabled worker");
        let worker_id = worker.id.clone();
        let role = worker.role;
        let observed_id = Arc::new(std::sync::Mutex::new(None));
        let observed = Arc::clone(&observed_id);
        run_worker(config, &worker_id, role, "test", move |context| {
            *observed.lock().unwrap() = Some(context.id().to_string());
            Ok(())
        })
        .expect("successful worker run is returned");
        assert_eq!(*observed_id.lock().unwrap(), Some(worker_id.clone()));
    }

    #[test]
    fn run_worker_propagates_worker_errors() {
        let config = runtime_config();
        let worker = config
            .workers
            .iter()
            .find(|worker| worker.enabled)
            .expect("fixture has an enabled worker");
        let worker_id = worker.id.clone();
        let role = worker.role;
        let error = run_worker(config, &worker_id, role, "test", |_| {
            Err("worker failure sentinel".into())
        })
        .expect_err("worker failure must reach the caller");
        assert!(error.contains("worker failure sentinel"), "{error}");
    }

    #[test]
    fn wrong_role_is_rejected_before_runner_starts() {
        let config = runtime_config();
        let worker = config
            .workers
            .iter()
            .find(|worker| worker.enabled && worker.role != WorkerRole::Api)
            .or_else(|| config.workers.iter().find(|worker| worker.enabled))
            .expect("fixture has an enabled worker");
        let worker_id = worker.id.clone();
        let wrong_role = if worker.role == WorkerRole::Api {
            WorkerRole::Scheduler
        } else {
            WorkerRole::Api
        };
        let started = Arc::new(AtomicBool::new(false));
        let runner_started = Arc::clone(&started);
        let error = run_worker(config, &worker_id, wrong_role, "test", move |_| {
            runner_started.store(true, Ordering::Release);
            Ok(())
        })
        .unwrap_err();
        assert!(error.contains("角色不匹配"), "{error}");
        assert!(!started.load(Ordering::Acquire));
    }

    #[test]
    fn polling_worker_once_executes_one_tick_and_returns() {
        let config = runtime_config();
        let worker = config
            .workers
            .iter()
            .find(|worker| worker.enabled)
            .expect("fixture has an enabled worker");
        let worker_id = worker.id.clone();
        let role = worker.role;
        let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = Arc::clone(&ticks);
        run_worker(config, &worker_id, role, "test", move |context| {
            run_polling_worker(&context, true, || {
                observed.fetch_add(1, Ordering::Relaxed);
                Ok(0)
            })
        })
        .expect("one-shot worker completes");
        assert_eq!(ticks.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn polling_worker_stops_during_a_long_sleep() {
        let config = runtime_config();
        let worker = config
            .workers
            .iter()
            .find(|worker| worker.enabled)
            .expect("fixture has an enabled worker");
        let worker_id = worker.id.clone();
        let (tick_tx, tick_rx) = std::sync::mpsc::channel();
        let supervisor = RuntimeSupervisor::new(config).expect("runtime topology is valid");
        let handle = supervisor
            .spawn_worker(&worker_id, move |context| {
                run_polling_worker(&context, false, || {
                    let _ = tick_tx.send(());
                    Ok(10_000)
                })
            })
            .expect("worker starts");
        tick_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("worker performs its first tick");
        supervisor.request_shutdown();
        qx_runtime::host::join_worker_handle(&supervisor, handle, "test", &worker_id)
            .expect("worker observes shutdown while sleeping");
    }
}
