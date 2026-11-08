use super::{admission::require_live_intent_reconciliation, execution_updates};
use crate::{persist_strategy_submit, runtime_timestamp_ms};
use qx_runtime::StrategyRuntimeConfig;
use qx_service::configured_backends::{ConfiguredJobQueue, ControlStateBackend};
use qx_storage::{ControlCommandQueueBackend, JobLease, QueuedJob};
use std::path::Path;
use std::sync::Arc;

#[allow(clippy::too_many_arguments)]
pub(super) fn replay_prepared_commands(
    prepared_evaluation_path: &Path,
    prepared: &mut execution_updates::PreparedEvaluationV3,
    strategy_config: &StrategyRuntimeConfig,
    root: &Path,
    queued: &QueuedJob,
    queue: &ConfiguredJobQueue,
    lease_owner: &str,
    lease_seconds: u64,
    lease: &mut JobLease,
    control_store: &ControlStateBackend,
    command_queue: &Arc<dyn ControlCommandQueueBackend>,
) -> Result<(), String> {
    for command in &prepared.commands {
        if !command.dry_run && queued.job.job_id.starts_with("live-strategy:") {
            *lease = queue
                .claim(
                    queued.run.run_id,
                    lease_owner,
                    crate::lease_clock(runtime_timestamp_ms()),
                    lease_seconds,
                )
                .map_err(|error| {
                    format!("恢复实时 Strategy intent 前重新领取 fencing 租约失败: {error:?}")
                })?;
            require_live_intent_reconciliation(strategy_config, root, runtime_timestamp_ms())?;
        }

        let local_result = match persist_strategy_submit(
            control_store,
            command_queue.as_ref(),
            command,
            runtime_timestamp_ms(),
        ) {
            Ok(result) => result,
            Err(error) => {
                prepared.command_results.insert(
                    command.command_id,
                    execution_updates::unknown_command_result(&error),
                );
                execution_updates::save_prepared_evaluation(prepared_evaluation_path, prepared)?;
                if queued.job.job_id.starts_with("live-strategy:") && !command.dry_run {
                    execution_updates::benchmark_crash_after("live_unknown_outcome_persisted");
                    return Err(format!(
                        "LIVE_COMMAND_OUTCOME_UNKNOWN:{}: {error}",
                        command.command_id
                    ));
                }
                return Err(error);
            }
        };
        execution_updates::benchmark_crash_after("control_command_persisted");
        prepared.completed_command_ids.insert(command.command_id);
        prepared
            .command_results
            .insert(command.command_id, local_result);
        execution_updates::save_prepared_evaluation(prepared_evaluation_path, prepared)?;
        execution_updates::benchmark_crash_after("command_result_persisted");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workers::strategy_worker::{
        admission::require_live_strategy_commands_resolved, execution_updates,
    };
    use qx_core::{Event, EventKind, EventLog, InstrumentId, Order, OrderStatus, Quantity, Side};
    use qx_runtime::RuntimeConfig;
    use qx_scheduler::LiveStrategyJobInput;
    use qx_service::configured_backends::{ConfiguredJobQueue, ControlStateBackend};
    use qx_storage::{
        ControlCommandQueue, ControlCommandQueueBackend, FileJobQueue, JsonStateStore, QueuedJob,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;
    use std::process::Command;

    fn report(observed_ts: u64, account_id: &str) -> qx_api::ReconcileReportSnapshot {
        qx_api::ReconcileReportSnapshot {
            schema_version: 1,
            worker_id: "test-reconciler".into(),
            account_id: account_id.into(),
            venue_id: "binance".into(),
            observed_ts,
            order_issues: Vec::new(),
            balances_count: 0,
            balance_discrepancies: Vec::new(),
            position_snapshots_count: None,
            funding_rate_snapshots_count: None,
            cashflow_count: None,
        }
    }

    fn make_prepared(
        worker_id: &str,
        queued: &QueuedJob,
        command: qx_control::ControlCommand,
        order: Order,
    ) -> execution_updates::PreparedEvaluationV3 {
        let mut prepared = execution_updates::PreparedEvaluationV3 {
            schema_version: execution_updates::PREPARED_EVALUATION_SCHEMA_VERSION,
            worker_id: worker_id.into(),
            job_id: queued.job.job_id.clone(),
            run_id: queued.run.run_id,
            input_sha256: "1".repeat(64),
            facts_sha256: "2".repeat(64),
            config_sha256: "3".repeat(64),
            lease_fencing_token: 1,
            ranges: Vec::new(),
            orders: vec![order],
            commands: vec![command],
            spread_group_id: None,
            checkpoint: None,
            completed_command_ids: BTreeSet::new(),
            command_results: BTreeMap::new(),
            payload_sha256: String::new(),
        };
        prepared.payload_sha256 =
            execution_updates::prepared_evaluation_payload_sha256(&prepared).unwrap();
        prepared
    }

    fn bind_prepared_recovery_identity(
        prepared: &mut execution_updates::PreparedEvaluationV3,
        config: &RuntimeConfig,
        runtime_path: &Path,
        root: &Path,
        job: &qx_scheduler::JobSpec,
    ) {
        use sha2::Digest;

        let job_bytes = serde_json::to_vec(job).unwrap();
        let job_digest = format!("{:x}", sha2::Sha256::digest(&job_bytes));
        let mut strategy_config = config.strategy_for_worker("strategy-paper").unwrap();
        crate::resolve_strategy_runtime_paths(&mut strategy_config, runtime_path);
        let mut strategy_runtime_config = config.clone();
        crate::resolve_all_worker_runtime_paths(&mut strategy_runtime_config, runtime_path);
        strategy_runtime_config.strategy = strategy_config.clone();
        let config_bytes = serde_json::to_vec(&strategy_config).unwrap();
        let config_digest = format!("{:x}", sha2::Sha256::digest(&config_bytes));
        let ranges = execution_updates::ranges_from_job(job).unwrap();
        let strategy_id = strategy_config
            .id
            .clone()
            .unwrap_or_else(|| strategy_config.version.clone());
        let facts = execution_updates::execution_updates_for_ranges(
            &strategy_runtime_config,
            root,
            &strategy_id,
            &ranges,
        )
        .unwrap();
        let facts_bytes = serde_json::to_vec(&(&ranges, &facts)).unwrap();
        prepared.input_sha256 = job_digest;
        prepared.facts_sha256 = format!("{:x}", sha2::Sha256::digest(&facts_bytes));
        prepared.config_sha256 = config_digest;
        prepared.ranges = ranges;
        prepared.payload_sha256 =
            execution_updates::prepared_evaluation_payload_sha256(prepared).unwrap();
    }

    fn configured_queue(config: &RuntimeConfig, root: &Path) -> ConfiguredJobQueue {
        match config.storage.backend {
            qx_runtime::StorageBackend::Files => {
                ConfiguredJobQueue::Files(FileJobQueue::new(root.join("job-queue")))
            }
            #[cfg(feature = "sqlite")]
            qx_runtime::StorageBackend::Sqlite => ConfiguredJobQueue::Sqlite(
                qx_storage::SqliteJobQueue::new(config.storage.sqlite_path.as_deref().unwrap())
                    .unwrap(),
            ),
            #[cfg(not(feature = "sqlite"))]
            qx_runtime::StorageBackend::Sqlite => {
                panic!("process fixture requires the sqlite feature")
            }
            qx_runtime::StorageBackend::Postgres => {
                #[cfg(feature = "postgres")]
                {
                    let dsn = qx_service::configured_backends::postgres_dsn(config).unwrap();
                    ConfiguredJobQueue::Postgres(
                        qx_storage::PostgresJobQueue::connect(&dsn).unwrap(),
                    )
                }
                #[cfg(not(feature = "postgres"))]
                panic!("process fixture requires the postgres feature")
            }
        }
    }

    fn configured_control(config: &RuntimeConfig, root: &Path) -> ControlStateBackend {
        match config.storage.backend {
            qx_runtime::StorageBackend::Files => {
                ControlStateBackend::Files(JsonStateStore::new(root))
            }
            #[cfg(feature = "sqlite")]
            qx_runtime::StorageBackend::Sqlite => ControlStateBackend::Sqlite(
                qx_storage::SqliteControlStore::new(config.storage.sqlite_path.as_deref().unwrap())
                    .unwrap(),
            ),
            #[cfg(not(feature = "sqlite"))]
            qx_runtime::StorageBackend::Sqlite => {
                panic!("process fixture requires the sqlite feature")
            }
            qx_runtime::StorageBackend::Postgres => {
                #[cfg(feature = "postgres")]
                {
                    let dsn = qx_service::configured_backends::postgres_dsn(config).unwrap();
                    ControlStateBackend::Postgres(
                        qx_storage::PostgresControlStore::connect(&dsn).unwrap(),
                    )
                }
                #[cfg(not(feature = "postgres"))]
                panic!("process fixture requires the postgres feature")
            }
        }
    }

    fn configured_command_queue(
        config: &RuntimeConfig,
        root: &Path,
    ) -> Arc<dyn ControlCommandQueueBackend> {
        match config.storage.backend {
            qx_runtime::StorageBackend::Files => {
                Arc::new(ControlCommandQueue::new(root.join("control-queue")))
            }
            #[cfg(feature = "sqlite")]
            qx_runtime::StorageBackend::Sqlite => Arc::new(
                qx_storage::SqliteControlCommandQueue::new(
                    config.storage.sqlite_path.as_deref().unwrap(),
                )
                .unwrap(),
            ),
            #[cfg(not(feature = "sqlite"))]
            qx_runtime::StorageBackend::Sqlite => {
                panic!("process fixture requires the sqlite feature")
            }
            qx_runtime::StorageBackend::Postgres => {
                #[cfg(feature = "postgres")]
                {
                    let dsn = qx_service::configured_backends::postgres_dsn(config).unwrap();
                    Arc::new(qx_storage::PostgresControlCommandQueue::connect(&dsn).unwrap())
                }
                #[cfg(not(feature = "postgres"))]
                panic!("process fixture requires the postgres feature")
            }
        }
    }

    fn write_event_log(config: &RuntimeConfig, root: &Path, name: &str, log: &EventLog) {
        match config.storage.backend {
            qx_runtime::StorageBackend::Files => {
                qx_storage::EventLogFileStore::new(root)
                    .write(name, log)
                    .unwrap();
            }
            #[cfg(feature = "sqlite")]
            qx_runtime::StorageBackend::Sqlite => {
                qx_storage::SqliteEventLogStore::new(
                    config.storage.sqlite_path.as_deref().unwrap(),
                )
                .unwrap()
                .write(name, log)
                .unwrap();
            }
            #[cfg(not(feature = "sqlite"))]
            qx_runtime::StorageBackend::Sqlite => {
                panic!("process fixture requires the sqlite feature")
            }
            qx_runtime::StorageBackend::Postgres => {
                #[cfg(feature = "postgres")]
                {
                    let dsn = qx_service::configured_backends::postgres_dsn(config).unwrap();
                    qx_storage::PostgresEventLogStore::connect(&dsn)
                        .unwrap()
                        .write(name, log)
                        .unwrap();
                }
                #[cfg(not(feature = "postgres"))]
                panic!("process fixture requires the postgres feature")
            }
        }
    }

    fn read_event_log(config: &RuntimeConfig, root: &Path, name: &str) -> EventLog {
        let log = match config.storage.backend {
            qx_runtime::StorageBackend::Files => qx_storage::EventLogFileStore::new(root)
                .read_if_exists(name)
                .unwrap(),
            #[cfg(feature = "sqlite")]
            qx_runtime::StorageBackend::Sqlite => {
                qx_storage::SqliteEventLogStore::new(config.storage.sqlite_path.as_deref().unwrap())
                    .unwrap()
                    .read_if_exists(name)
                    .unwrap()
            }
            #[cfg(not(feature = "sqlite"))]
            qx_runtime::StorageBackend::Sqlite => {
                panic!("process fixture requires the sqlite feature")
            }
            qx_runtime::StorageBackend::Postgres => {
                #[cfg(feature = "postgres")]
                {
                    let dsn = qx_service::configured_backends::postgres_dsn(config).unwrap();
                    qx_storage::PostgresEventLogStore::connect(&dsn)
                        .unwrap()
                        .read_if_exists(name)
                        .unwrap()
                }
                #[cfg(not(feature = "postgres"))]
                panic!("process fixture requires the postgres feature")
            }
        };
        log.expect("fixture EventLog must survive child process restart")
    }

    fn live_recovery_child(root: &Path, phase: &str) {
        let config: RuntimeConfig =
            serde_json::from_slice(&std::fs::read(root.join("runtime.json")).unwrap()).unwrap();
        let queued: QueuedJob =
            serde_json::from_slice(&std::fs::read(root.join("queued-job.json")).unwrap()).unwrap();
        let worker_id = "strategy-paper";
        let lease_seconds = std::env::var("QX_BENCH_STRATEGY_LEASE_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1);
        let strategy_config = config.strategy_for_worker(worker_id).unwrap();
        let root = Path::new(&config.storage.data_dir);
        let queue = configured_queue(&config, root);
        let lease_owner = crate::strategy_job_lease_owner(worker_id);
        let mut lease = queue
            .claim(
                queued.run.run_id,
                &lease_owner,
                crate::lease_clock(crate::runtime_timestamp_ms()),
                lease_seconds,
            )
            .unwrap();
        let control = configured_control(&config, root);
        let command_queue = configured_command_queue(&config, root);
        let journal_path = execution_updates::prepared_evaluation_path(root, worker_id);
        let mut prepared = execution_updates::load_prepared_evaluation(&journal_path)
            .unwrap()
            .unwrap();
        let command_id = prepared
            .commands
            .first()
            .expect("recovery fixture must contain a command")
            .command_id;
        let cursor_path =
            execution_updates::configured_cursor_path(root, worker_id, &config).unwrap();
        let cursor_before = std::fs::read(root.join("cursor-before.bin")).unwrap();

        match phase {
            "unknown" => {
                if config.storage.backend == qx_runtime::StorageBackend::Files {
                    let replay_error = replay_prepared_commands(
                        &journal_path,
                        &mut prepared,
                        &strategy_config,
                        root,
                        &queued,
                        &queue,
                        &lease_owner,
                        1,
                        &mut lease,
                        &control,
                        &command_queue,
                    )
                    .unwrap_err();
                    assert!(replay_error
                        .starts_with(&format!("LIVE_COMMAND_OUTCOME_UNKNOWN:{command_id}:")));
                    let persisted = execution_updates::load_prepared_evaluation(&journal_path)
                        .unwrap()
                        .unwrap();
                    assert!(persisted.command_results[&command_id].starts_with("OUTCOME_UNKNOWN:"));
                } else {
                    assert!(prepared.command_results[&command_id].starts_with("OUTCOME_UNKNOWN:"));
                }
                assert_eq!(
                    serde_json::to_vec(&execution_updates::load_cursor(&cursor_path).unwrap())
                        .unwrap(),
                    cursor_before
                );
                // Exit without Rust destructors to model a worker process killed after the
                // unknown outcome has reached durable storage.
                std::process::exit(86);
            }
            "resolve" => {}
            other => panic!("unknown child phase {other}"),
        }

        if config.storage.backend == qx_runtime::StorageBackend::Files {
            std::fs::remove_file(root.join("control-plane.json")).unwrap();
        }
        replay_prepared_commands(
            &journal_path,
            &mut prepared,
            &strategy_config,
            root,
            &queued,
            &queue,
            &lease_owner,
            lease_seconds,
            &mut lease,
            &control,
            &command_queue,
        )
        .unwrap();
        let accepted = control.load().unwrap();
        let pending =
            require_live_strategy_commands_resolved(&config, root, &accepted, &prepared.commands)
                .unwrap_err();
        let command_id = prepared.commands[0].command_id;
        assert!(pending.starts_with(&format!("LIVE_COMMAND_OUTCOME_PENDING:{command_id}")));
        assert_eq!(
            serde_json::to_vec(&execution_updates::load_cursor(&cursor_path).unwrap()).unwrap(),
            cursor_before
        );

        let failed_at = crate::runtime_timestamp_ms();
        let outcome = std::env::var("QX_LIVE_RECOVERY_OUTCOME").unwrap_or_else(|_| "failed".into());
        let executed = outcome == "executed";
        control
            .transact(|plane| {
                plane.execute(command_id, failed_at, |_| {
                    if executed {
                        Ok("SIMULATED_VENUE_EXECUTED".into())
                    } else {
                        Err("SIMULATED_VENUE_TIMEOUT".into())
                    }
                })
            })
            .unwrap()
            .1
            .unwrap();
        if !executed {
            let account_id = prepared.orders[0].account_id.as_str();
            JsonStateStore::new(root)
                .save_json_at(
                    "reconcile/test-reconciler.json",
                    &report(failed_at.saturating_add(1), account_id),
                )
                .unwrap();
        }
        let account_id = prepared.orders[0].account_id.as_str();
        let log_name = qx_runtime::account_event_log_name(account_id, "binance").unwrap();
        let mut log = read_event_log(&config, root, &log_name);
        log.append_checked(Event::new(
            log.next_seq(),
            failed_at.saturating_add(2),
            0,
            EventKind::OrderStatusReconciled {
                client_order_id: command_id,
                account_id: account_id.into(),
                venue_id: "binance".into(),
                status: if executed {
                    OrderStatus::Accepted
                } else {
                    OrderStatus::Rejected
                },
                venue_order_id: None,
                filled_raw: 0,
                filled_exact: None,
            },
        ))
        .unwrap();
        write_event_log(&config, root, &log_name, &log);
        require_live_strategy_commands_resolved(
            &config,
            root,
            &control.load().unwrap(),
            &prepared.commands,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(
            lease_seconds * 1_000 + 100,
        ));
        crate::run_strategy_worker(&root.join("runtime.json"), worker_id, true, true).unwrap();
        assert_ne!(
            serde_json::to_vec(&execution_updates::load_cursor(&cursor_path).unwrap()).unwrap(),
            cursor_before
        );
    }

    fn create_live_recovery_fixture(
        root: &Path,
        backend: qx_runtime::StorageBackend,
        command_id: u64,
        account_id: &str,
    ) {
        std::fs::create_dir_all(root).unwrap();
        let mut config: RuntimeConfig = serde_json::from_str(include_str!(
            "../../../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .unwrap();
        // This process-recovery fixture submits to a simulated Binance testnet worker,
        // so model the external-trading transport explicitly instead of inheriting the
        // paper template's plaintext API topology.
        config.environment = "testnet".into();
        config.api.transport = qx_runtime::ApiTransport::Mtls;
        config.api.tls = Some(qx_runtime::TlsPaths {
            certificate_chain: "server.pem".into(),
            private_key: "server.key".into(),
            client_ca: "clients.pem".into(),
        });
        config.api.operators.insert(
            "ops".into(),
            qx_runtime::OperatorConfig {
                permission: qx_control::Permission::Trading,
                certificate: "ops.pem".into(),
            },
        );
        config.storage.data_dir = root.to_string_lossy().into_owned();
        config.storage.backend = backend;
        config.storage.sqlite_path = (backend == qx_runtime::StorageBackend::Sqlite)
            .then(|| root.join("runtime.sqlite3").to_string_lossy().into_owned());
        if backend == qx_runtime::StorageBackend::Postgres {
            config.profile = qx_runtime::RuntimeProfile::Distributed;
            config.storage.consistency = qx_runtime::StorageConsistency::Transactional;
            config.storage.postgres_dsn_env = Some("QX_TEST_POSTGRES_DSN".into());
        }
        config.scheduler.job_queue_path = root.join("job-queue").to_string_lossy().into_owned();
        config.strategy.live_enabled = true;
        config.strategy.account_id = Some(account_id.into());
        config.strategy.venue_id = Some("binance".into());
        config.strategy.bars_snapshot_path =
            Some(root.join("bars.json").to_string_lossy().into_owned());
        std::fs::write(
            root.join("bars.json"),
            br#"{"schema_version":1,"instrument":"BTCUSDT.BINANCE","source":"fixture","ts":[1793232000000],"open_raw":[1000000000],"high_raw":[1000000000],"low_raw":[1000000000],"close_raw":[1000000000],"volume_raw":[1000000000]}"#,
        )
        .unwrap();
        config.workers.push(
            serde_json::from_value::<qx_runtime::WorkerConfig>(serde_json::json!({
                "id": "marketdata-process-test",
                "role": "market_data",
                "enabled": true,
                "account_id": null,
                "venue_id": "binance",
                "endpoint": "ws://127.0.0.1:1",
                "symbols": ["BTCUSDT.BINANCE"],
                "credential_env": null
            }))
            .unwrap(),
        );
        if let Some(worker) = config
            .workers
            .iter_mut()
            .find(|worker| worker.id == "strategy-paper")
        {
            worker.venue_id = Some("binance".into());
            worker.account_id = Some(account_id.into());
        }
        if let Some(worker) = config
            .workers
            .iter_mut()
            .find(|worker| worker.id == "paper-execution")
        {
            worker.venue_id = Some("binance".into());
            worker.account_id = Some(account_id.into());
            worker.paper_initial_cash_raw = None;
            worker.credential_env = Some(qx_runtime::CredentialEnv {
                api_key: "QX_TEST_BINANCE_API_KEY".into(),
                secret: "QX_TEST_BINANCE_API_SECRET".into(),
            });
        }
        std::fs::write(
            root.join("runtime.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let worker_id = "strategy-paper";
        let now = crate::runtime_timestamp_ms();
        JsonStateStore::new(root)
            .save_json_at("reconcile/test-reconciler.json", &report(now, account_id))
            .unwrap();
        let lease_now = crate::lease_clock(now);
        let (mut job, mut run) = qx_scheduler::live_strategy_job(LiveStrategyJobInput {
            job_version: "test-live-process-v1",
            worker_id,
            instrument: Some("BTCUSDT.BINANCE"),
            data_fingerprint: command_id,
            trading_day: "2026-10-29".into(),
            lease_now,
            environment: "live",
        });
        let log_name = qx_runtime::account_event_log_name(account_id, "binance").unwrap();
        execution_updates::attach_ranges_to_job(
            &mut job,
            &mut run,
            &[execution_updates::CursorRange {
                log_name: log_name.clone(),
                from_seq: 0,
                through_seq: 0,
            }],
        )
        .unwrap();
        let queued = QueuedJob {
            job: job.clone(),
            run: run.clone(),
            enqueued_ts: lease_now,
        };
        std::fs::write(
            root.join("queued-job.json"),
            serde_json::to_vec(&queued).unwrap(),
        )
        .unwrap();
        let queue = configured_queue(&config, root);
        queue.enqueue(job.clone(), run, lease_now).unwrap();
        let order = Order {
            client_id: command_id,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_raw(1_000_000_000),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: account_id.into(),
            trace: Some(qx_core::OrderTrace {
                strategy_id: Some(worker_id.into()),
                signal_id: Some(1),
                intent_id: Some(command_id),
                rule_version: Some("test-v1".into()),
            }),
            policy: None,
            exact_amounts: None,
        };
        let command = crate::strategy_submit_command(worker_id, &order, false, None).unwrap();
        let mut log = EventLog::new();
        log.append_checked(Event::new(
            0,
            now,
            0,
            EventKind::OrderSubmitted {
                order: order.clone(),
            },
        ))
        .unwrap();
        write_event_log(&config, root, &log_name, &log);
        let mut prepared = make_prepared(worker_id, &queued, command, order.clone());
        bind_prepared_recovery_identity(
            &mut prepared,
            &config,
            &root.join("runtime.json"),
            root,
            &job,
        );
        if backend != qx_runtime::StorageBackend::Files {
            prepared.command_results.insert(
                command_id,
                "OUTCOME_UNKNOWN: simulated prior process exit".into(),
            );
            prepared.payload_sha256 =
                execution_updates::prepared_evaluation_payload_sha256(&prepared).unwrap();
        } else {
            std::fs::write(root.join("control-plane.json"), b"{corrupt control state").unwrap();
        }
        let journal_path = execution_updates::prepared_evaluation_path(root, worker_id);
        execution_updates::save_prepared_evaluation(&journal_path, &prepared).unwrap();
        let cursor_path =
            execution_updates::configured_cursor_path(root, worker_id, &config).unwrap();
        let cursor = execution_updates::load_cursor(&cursor_path).unwrap();
        execution_updates::save_cursor(&cursor_path, &cursor).unwrap();
        let cursor_bytes = serde_json::to_vec(&cursor).unwrap();
        std::fs::write(root.join("cursor-before.bin"), cursor_bytes).unwrap();
    }

    #[test]
    fn live_unknown_recovery_survives_real_file_sqlite_and_postgres_process_restarts() {
        const TEST_NAME: &str = "workers::strategy_worker::prepared_command_replay::tests::live_unknown_recovery_survives_real_file_sqlite_and_postgres_process_restarts";
        if let (Ok(root), Ok(phase)) = (
            std::env::var("QX_LIVE_RECOVERY_FIXTURE_ROOT"),
            std::env::var("QX_LIVE_RECOVERY_FIXTURE_PHASE"),
        ) {
            live_recovery_child(Path::new(&root), &phase);
            return;
        }

        #[cfg_attr(not(any(feature = "sqlite", feature = "postgres")), allow(unused_mut))]
        let mut backends = vec![qx_runtime::StorageBackend::Files];
        #[cfg(feature = "sqlite")]
        backends.push(qx_runtime::StorageBackend::Sqlite);
        #[cfg(feature = "postgres")]
        if std::env::var_os("QX_TEST_POSTGRES_DSN").is_some() {
            backends.push(qx_runtime::StorageBackend::Postgres);
        }
        for (backend_index, backend) in backends.into_iter().enumerate() {
            // The crash process uses a one-second lease to make expiry deterministic. Recovery
            // runs under the full workspace's concurrent test load, so its lease must not expire
            // while slow CI/Windows filesystem or database work is still committing.
            let recovery_lease_seconds = 20;
            for outcome in ["failed", "executed"] {
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let account_id = if backend == qx_runtime::StorageBackend::Postgres {
                    format!("main-pg-{backend_index}-{outcome}-{nonce}")
                } else {
                    "main".to_string()
                };
                let command_id = if backend == qx_runtime::StorageBackend::Postgres {
                    nonce as u64
                } else if backend == qx_runtime::StorageBackend::Sqlite {
                    52
                } else {
                    51
                };
                let root = std::env::current_dir()
                    .unwrap()
                    .join("target")
                    .join(format!(
                        "live-process-recovery-{backend:?}-{outcome}-{nonce}"
                    ));
                create_live_recovery_fixture(&root, backend, command_id, &account_id);
                let executable = std::env::current_exe().unwrap();
                let unknown = Command::new(&executable)
                    .arg("--exact")
                    .arg(TEST_NAME)
                    .arg("--nocapture")
                    .env("QX_LIVE_RECOVERY_FIXTURE_ROOT", &root)
                    .env("QX_LIVE_RECOVERY_FIXTURE_PHASE", "unknown")
                    .env("QX_BENCH_STRATEGY_LEASE_SECONDS", "1")
                    .status()
                    .unwrap();
                assert_eq!(
                    unknown.code(),
                    Some(86),
                    "Unknown process must exit at the durable boundary"
                );
                let config: RuntimeConfig =
                    serde_json::from_slice(&std::fs::read(root.join("runtime.json")).unwrap())
                        .unwrap();
                let cursor_path =
                    execution_updates::configured_cursor_path(&root, "strategy-paper", &config)
                        .unwrap();
                assert_eq!(
                    serde_json::to_vec(&execution_updates::load_cursor(&cursor_path).unwrap())
                        .unwrap(),
                    std::fs::read(root.join("cursor-before.bin")).unwrap()
                );
                std::thread::sleep(std::time::Duration::from_millis(1_100));

                let resolved = Command::new(&executable)
                    .arg("--exact")
                    .arg(TEST_NAME)
                    .arg("--nocapture")
                    .env("QX_LIVE_RECOVERY_FIXTURE_ROOT", &root)
                    .env("QX_LIVE_RECOVERY_FIXTURE_PHASE", "resolve")
                    .env("QX_LIVE_RECOVERY_OUTCOME", outcome)
                    .env(
                        "QX_BENCH_STRATEGY_LEASE_SECONDS",
                        recovery_lease_seconds.to_string(),
                    )
                    .output()
                    .unwrap();
                assert!(
                    resolved.status.success(),
                    "recovery child failed: {}\nstdout:\n{}\nstderr:\n{}",
                    resolved.status,
                    String::from_utf8_lossy(&resolved.stdout),
                    String::from_utf8_lossy(&resolved.stderr)
                );
                let worker_id = "strategy-paper";
                let observation_directory = root
                    .join("strategy-observations")
                    .join(&qx_strategy::sha256_hex(worker_id.as_bytes())[..32]);
                let observations = std::fs::read_dir(&observation_directory)
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        entry
                            .path()
                            .extension()
                            .is_some_and(|value| value == "json")
                    })
                    .map(|entry| {
                        serde_json::from_slice::<qx_runtime::StrategyWorkerObservationV3>(
                            &std::fs::read(entry.path()).unwrap(),
                        )
                        .unwrap()
                    })
                    .collect::<Vec<_>>();
                let observation = observations
                    .into_iter()
                    .find(|value| {
                        value.worker_id == worker_id
                            && value.phase == qx_runtime::StrategyWorkerPhase::Stopped
                    })
                    .expect("completed worker must persist a stopped observation");
                assert_eq!(observation.schema_version, 3);
                assert_eq!(observation.worker_id, worker_id);
                assert_eq!(observation.phase, qx_runtime::StrategyWorkerPhase::Stopped);
                let capabilities = observation
                    .host_capabilities
                    .expect("default Rust Strategy host capabilities must be observed");
                assert_eq!(capabilities.host_id, "builtin_rust_strategy_runtime");
                assert!(capabilities
                    .supported
                    .contains(&qx_strategy::capabilities::StrategyCapability::Bar));
                assert!(capabilities
                    .supported
                    .contains(&qx_strategy::capabilities::StrategyCapability::BarHistory));
                let lifecycle = observation
                    .host_lifecycle
                    .expect("completed worker must persist host lifecycle evidence");
                assert_eq!(lifecycle.schema_version, 2);
                assert!(lifecycle.constructed);
                assert!(lifecycle.initialized);
                let config: RuntimeConfig =
                    serde_json::from_slice(&std::fs::read(root.join("runtime.json")).unwrap())
                        .unwrap();
                let statuses = qx_service::strategy_deployments::read_strategy_deployments(
                    &config,
                    crate::runtime_timestamp_ms(),
                )
                .unwrap();
                let status = statuses
                    .iter()
                    .find(|value| value.worker_id == worker_id)
                    .expect("worker status projection must include the observed strategy");
                assert!(status.trading_enabled.is_none());
                assert!(!status
                    .blocking_reasons
                    .iter()
                    .any(|reason| reason == "host_capabilities_not_observed"));
                assert!(!status
                    .blocking_reasons
                    .iter()
                    .any(|reason| reason == "host_lifecycle_not_observed"));
                assert_ne!(
                    serde_json::to_vec(&execution_updates::load_cursor(&cursor_path).unwrap())
                        .unwrap(),
                    std::fs::read(root.join("cursor-before.bin")).unwrap()
                );
                std::fs::remove_dir_all(root).unwrap();
            }
        }
    }

    #[test]
    fn runtime_strategy_checkpoint_is_restored_by_a_new_worker_process() {
        const TEST_NAME: &str = "workers::strategy_worker::prepared_command_replay::tests::runtime_strategy_checkpoint_is_restored_by_a_new_worker_process";
        if let Ok(root) = std::env::var("QX_CHECKPOINT_RESTART_FIXTURE_ROOT") {
            let config_path = Path::new(&root).join("runtime.json");
            crate::run_scheduler_worker(&config_path, "scheduler-paper", true, true).unwrap();
            crate::run_strategy_worker(&config_path, "strategy-paper", true, true).unwrap();
            return;
        }

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let workspace_root = workspace_root.canonicalize().unwrap();
        let root = workspace_root
            .join("target")
            .join(format!("runtime-checkpoint-process-restart-{nonce}"));
        std::fs::create_dir_all(&root).unwrap();
        let mut config: RuntimeConfig = serde_json::from_str(include_str!(
            "../../../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .unwrap();
        config.storage.data_dir = root.to_string_lossy().into_owned();
        config.scheduler.state_path = root.join("scheduler.json").to_string_lossy().into_owned();
        config.scheduler.jobs_path = root.join("jobs.json").to_string_lossy().into_owned();
        config.scheduler.job_queue_path = root.join("job-queue").to_string_lossy().into_owned();
        config.strategy.id = Some("checkpoint-process-worker".into());
        config.strategy.version = "1.0".into();
        config.strategy.builtin_strategy = None;
        config.strategy.python_module = Some(
            workspace_root
                .join("python/tests/fixtures/runtime_checkpoint_process_strategy.py")
                .to_string_lossy()
                .into_owned(),
        );
        config.strategy.python_strategy_spec = Some(serde_json::json!({
            "data_level": "bar",
            "parameters": {"restore_log": root.join("restore.jsonl")}
        }));
        config.strategy.bars_snapshot_path = Some(
            workspace_root
                .join("deploy/qianxing.bar-frame.example.json")
                .to_string_lossy()
                .into_owned(),
        );
        for worker in &mut config.workers {
            if worker.instrument_spec_path.is_some() {
                worker.instrument_spec_path = Some(
                    workspace_root
                        .join("deploy/qianxing.binance.spot.spec.json")
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        config.strategy.live_enabled = false;
        std::fs::write(
            root.join("runtime.json"),
            serde_json::to_vec_pretty(&config).unwrap(),
        )
        .unwrap();

        let mut jobs = qx_scheduler::declared_jobs(include_str!(
            "../../../../../deploy/qianxing.scheduler.paper-order-smoke.json"
        ))
        .unwrap();
        let mut first_job = jobs.pop().unwrap();
        first_job.job_id = "checkpoint-process-run-1".into();
        first_job.concurrency_key = first_job.job_id.clone();
        let mut second_job = first_job.clone();
        second_job.job_id = "checkpoint-process-run-2".into();
        second_job.concurrency_key = second_job.job_id.clone();
        second_job.depends_on = vec![first_job.job_id.clone()];
        std::fs::write(
            root.join("jobs.json"),
            serde_json::to_vec_pretty(&[first_job, second_job]).unwrap(),
        )
        .unwrap();
        let executable = std::env::current_exe().unwrap();

        let mut first_bar_count = serde_json::Value::Null;
        for index in [1_u64, 2] {
            let child = Command::new(&executable)
                .arg("--exact")
                .arg(TEST_NAME)
                .arg("--nocapture")
                .env("QX_CHECKPOINT_RESTART_FIXTURE_ROOT", &root)
                .status()
                .unwrap();
            assert!(
                child.success(),
                "strategy worker process {index} failed: {child}"
            );

            let cursor_path =
                execution_updates::configured_cursor_path(&root, "strategy-paper", &config)
                    .unwrap();
            let cursor = execution_updates::load_cursor(&cursor_path).unwrap();
            let checkpoint = execution_updates::strategy_checkpoint(&cursor)
                .expect("runtime Python worker must persist a strategy checkpoint");
            if index == 1 {
                first_bar_count = checkpoint.strategy_state["bar_count"].clone();
                assert!(first_bar_count.as_u64().is_some_and(|count| count > 0));
                assert_eq!(cursor.commit_generation, 1);
                assert!(!root.join("restore.jsonl").exists());
            } else {
                assert_eq!(
                    checkpoint.strategy_state["bar_count"], first_bar_count,
                    "unchanged historical bars must not be replayed after checkpoint restore"
                );
                assert_eq!(cursor.commit_generation, 2);
                let restores = std::fs::read_to_string(root.join("restore.jsonl")).unwrap();
                let records = restores
                    .lines()
                    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(records.len(), 1, "second worker must restore once");
                assert_eq!(records[0]["bar_count"], first_bar_count);

                let observation_directory = root
                    .join("strategy-observations")
                    .join(&qx_strategy::sha256_hex(b"strategy-paper")[..32]);
                let observation = std::fs::read_dir(observation_directory)
                    .unwrap()
                    .filter_map(Result::ok)
                    .map(|entry| {
                        serde_json::from_slice::<qx_runtime::StrategyWorkerObservationV3>(
                            &std::fs::read(entry.path()).unwrap(),
                        )
                        .unwrap()
                    })
                    .filter(|value| value.phase == qx_runtime::StrategyWorkerPhase::Stopped)
                    .max_by_key(|value| value.observed_at_ms)
                    .expect("second worker must persist its stopped observation");
                assert!(observation.checkpoint_roundtrip_observed);
                assert!(
                    observation
                        .host_lifecycle
                        .expect("host lifecycle evidence is required")
                        .checkpoint_input_supplied
                );
            }
        }
    }

    #[test]
    fn durable_unknown_replay_waits_for_failed_command_venue_fact_and_clean_report() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "live-prepared-replay-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let mut config: RuntimeConfig = serde_json::from_str(include_str!(
            "../../../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .unwrap();
        config.storage.data_dir = root.to_string_lossy().into_owned();
        config.strategy.live_enabled = true;
        config.strategy.account_id = Some("main".into());
        config.strategy.venue_id = Some("binance".into());
        let strategy_config = config.strategy_for_worker("strategy-paper").unwrap();
        let now = crate::runtime_timestamp_ms();
        let state_store = JsonStateStore::new(&root);
        state_store
            .save_json_at("reconcile/test-reconciler.json", &report(now, "main"))
            .unwrap();

        let worker_id = "strategy-paper";
        let lease_now = crate::lease_clock(now);
        let (job, run) = qx_scheduler::live_strategy_job(LiveStrategyJobInput {
            job_version: "test-live-v1",
            worker_id,
            instrument: Some("BTCUSDT.BINANCE"),
            data_fingerprint: 41,
            trading_day: "2026-10-29".into(),
            lease_now,
            environment: "live",
        });
        let run_id = run.run_id;
        let queue = FileJobQueue::new(root.join("job-queue"));
        queue.enqueue(job.clone(), run.clone(), lease_now).unwrap();
        let queued = QueuedJob {
            job,
            run,
            enqueued_ts: lease_now,
        };
        let configured_queue = ConfiguredJobQueue::Files(queue.clone());
        let lease_owner = "strategy-paper-test";
        let mut lease = queue.claim(run_id, lease_owner, lease_now, 30).unwrap();

        let order = Order {
            client_id: 41,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_raw(1_000_000_000),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: Some(qx_core::OrderTrace {
                strategy_id: Some(worker_id.into()),
                signal_id: Some(1),
                intent_id: Some(41),
                rule_version: Some("test-v1".into()),
            }),
            policy: None,
            exact_amounts: None,
        };
        let command = crate::strategy_submit_command(worker_id, &order, false, None).unwrap();
        let mut prepared = make_prepared(worker_id, &queued, command.clone(), order.clone());
        let journal_path = execution_updates::prepared_evaluation_path(&root, worker_id);
        execution_updates::save_prepared_evaluation(&journal_path, &prepared).unwrap();

        let cursor_path = execution_updates::cursor_path(&root, worker_id);
        let cursor = execution_updates::load_cursor(&cursor_path).unwrap();
        execution_updates::save_cursor(&cursor_path, &cursor).unwrap();
        let cursor_before_replay = std::fs::read(&cursor_path).unwrap();

        let control_path = root.join("control-plane.json");
        std::fs::write(&control_path, b"{corrupt control state").unwrap();
        let control_store = ControlStateBackend::Files(state_store.clone());
        let command_queue: Arc<dyn ControlCommandQueueBackend> =
            Arc::new(ControlCommandQueue::new(root.join("control-queue")));
        let unknown = replay_prepared_commands(
            &journal_path,
            &mut prepared,
            &strategy_config,
            &root,
            &queued,
            &configured_queue,
            lease_owner,
            30,
            &mut lease,
            &control_store,
            &command_queue,
        )
        .unwrap_err();
        assert!(unknown.starts_with("LIVE_COMMAND_OUTCOME_UNKNOWN:41:"));
        drop(prepared);
        let mut prepared = execution_updates::load_prepared_evaluation(&journal_path)
            .unwrap()
            .unwrap();
        assert!(prepared.command_results[&41].starts_with("OUTCOME_UNKNOWN:"));
        assert!(!prepared.completed_command_ids.contains(&41));
        assert_eq!(std::fs::read(&cursor_path).unwrap(), cursor_before_replay);

        // Restart from the durable journal after repairing the control store. The recovered
        // command is accepted once, then the venue facts resolve it as Failed/Rejected.
        std::fs::remove_file(&control_path).unwrap();
        let restarted_control = ControlStateBackend::Files(JsonStateStore::new(&root));
        let restarted_queue = ConfiguredJobQueue::Files(FileJobQueue::new(root.join("job-queue")));
        let restarted_command_queue: Arc<dyn ControlCommandQueueBackend> =
            Arc::new(ControlCommandQueue::new(root.join("control-queue")));
        replay_prepared_commands(
            &journal_path,
            &mut prepared,
            &strategy_config,
            &root,
            &queued,
            &restarted_queue,
            lease_owner,
            30,
            &mut lease,
            &restarted_control,
            &restarted_command_queue,
        )
        .unwrap();
        assert!(prepared.completed_command_ids.contains(&41));

        let failed_at = crate::runtime_timestamp_ms();
        restarted_control
            .transact(|plane| {
                plane.execute(41, failed_at, |_| Err("SIMULATED_VENUE_TIMEOUT".into()))
            })
            .unwrap()
            .1
            .unwrap();
        let report_ts = failed_at.saturating_add(1);
        state_store
            .save_json_at("reconcile/test-reconciler.json", &report(report_ts, "main"))
            .unwrap();

        let log_name = qx_runtime::account_event_log_name("main", "binance").unwrap();
        let mut log = EventLog::new();
        log.append_checked(Event::new(
            0,
            failed_at.saturating_add(2),
            0,
            EventKind::OrderSubmitted {
                order: order.clone(),
            },
        ))
        .unwrap();
        log.append_checked(Event::new(
            1,
            failed_at.saturating_add(3),
            0,
            EventKind::OrderStatusReconciled {
                client_order_id: 41,
                account_id: "main".into(),
                venue_id: "binance".into(),
                status: OrderStatus::Rejected,
                venue_order_id: None,
                filled_raw: 0,
                filled_exact: None,
            },
        ))
        .unwrap();
        qx_storage::EventLogFileStore::new(&root)
            .write(&log_name, &log)
            .unwrap();

        let resolved_control = restarted_control.load().unwrap();
        require_live_strategy_commands_resolved(
            &config,
            &root,
            &resolved_control,
            &prepared.commands,
        )
        .unwrap();
        assert_eq!(std::fs::read(&cursor_path).unwrap(), cursor_before_replay);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_live_replay_keeps_cursor_fixed_until_failed_command_is_reconciled() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "sqlite-live-prepared-replay-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let database = root.join("runtime.sqlite3");
        let mut config: RuntimeConfig = serde_json::from_str(include_str!(
            "../../../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .unwrap();
        config.storage.backend = qx_runtime::StorageBackend::Sqlite;
        config.storage.data_dir = root.to_string_lossy().into_owned();
        config.storage.sqlite_path = Some(database.to_string_lossy().into_owned());
        config.strategy.live_enabled = true;
        config.strategy.account_id = Some("main".into());
        config.strategy.venue_id = Some("binance".into());
        let strategy_config = config.strategy_for_worker("strategy-paper").unwrap();
        let state_store = JsonStateStore::new(&root);
        let initial_now = crate::runtime_timestamp_ms();
        state_store
            .save_json_at(
                "reconcile/test-reconciler.json",
                &report(initial_now, "main"),
            )
            .unwrap();

        let worker_id = "strategy-paper";
        let lease_now = crate::lease_clock(initial_now);
        let (job, run) = qx_scheduler::live_strategy_job(LiveStrategyJobInput {
            job_version: "test-live-v1",
            worker_id,
            instrument: Some("BTCUSDT.BINANCE"),
            data_fingerprint: 42,
            trading_day: "2026-10-29".into(),
            lease_now,
            environment: "live",
        });
        let run_id = run.run_id;
        let queue = qx_storage::SqliteJobQueue::new(&database).unwrap();
        queue.enqueue(job.clone(), run.clone(), lease_now).unwrap();
        let queued = QueuedJob {
            job,
            run,
            enqueued_ts: lease_now,
        };
        let configured_queue = ConfiguredJobQueue::Sqlite(queue.clone());
        let lease_owner = "strategy-paper-sqlite-test";
        let mut lease = queue.claim(run_id, lease_owner, lease_now, 30).unwrap();

        let order = Order {
            client_id: 42,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_raw(1_000_000_000),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: Some(qx_core::OrderTrace {
                strategy_id: Some(worker_id.into()),
                signal_id: Some(1),
                intent_id: Some(42),
                rule_version: Some("test-v1".into()),
            }),
            policy: None,
            exact_amounts: None,
        };
        let command = crate::strategy_submit_command(worker_id, &order, false, None).unwrap();
        let mut prepared = make_prepared(worker_id, &queued, command.clone(), order.clone());
        prepared
            .command_results
            .insert(42, "OUTCOME_UNKNOWN: simulated prior process exit".into());
        prepared.payload_sha256 =
            execution_updates::prepared_evaluation_payload_sha256(&prepared).unwrap();
        let journal_path = execution_updates::prepared_evaluation_path(&root, worker_id);
        execution_updates::save_prepared_evaluation(&journal_path, &prepared).unwrap();
        drop(prepared);
        let mut prepared = execution_updates::load_prepared_evaluation(&journal_path)
            .unwrap()
            .unwrap();

        let cursor_path = execution_updates::cursor_path(&root, worker_id);
        let cursor = execution_updates::load_cursor(&cursor_path).unwrap();
        execution_updates::save_cursor(&cursor_path, &cursor).unwrap();
        let cursor_before_replay = std::fs::read(&cursor_path).unwrap();

        let control_store =
            ControlStateBackend::Sqlite(qx_storage::SqliteControlStore::new(&database).unwrap());
        let command_queue: Arc<dyn ControlCommandQueueBackend> =
            Arc::new(qx_storage::SqliteControlCommandQueue::new(&database).unwrap());
        replay_prepared_commands(
            &journal_path,
            &mut prepared,
            &strategy_config,
            &root,
            &queued,
            &configured_queue,
            lease_owner,
            30,
            &mut lease,
            &control_store,
            &command_queue,
        )
        .unwrap();
        assert!(prepared.completed_command_ids.contains(&42));
        let log_name = qx_runtime::account_event_log_name("main", "binance").unwrap();
        let mut log = EventLog::new();
        log.append_checked(Event::new(
            0,
            crate::runtime_timestamp_ms(),
            0,
            EventKind::OrderSubmitted {
                order: order.clone(),
            },
        ))
        .unwrap();
        let event_store = qx_storage::SqliteEventLogStore::new(&database).unwrap();
        event_store.write(&log_name, &log).unwrap();
        let accepted_control = control_store.load().unwrap();
        let pending = require_live_strategy_commands_resolved(
            &config,
            &root,
            &accepted_control,
            &prepared.commands,
        )
        .unwrap_err();
        assert!(
            pending.starts_with("LIVE_COMMAND_OUTCOME_PENDING:42"),
            "{pending}"
        );
        assert_eq!(std::fs::read(&cursor_path).unwrap(), cursor_before_replay);

        let failed_at = crate::runtime_timestamp_ms();
        control_store
            .transact(|plane| {
                plane.execute(42, failed_at, |_| Err("SIMULATED_VENUE_TIMEOUT".into()))
            })
            .unwrap()
            .1
            .unwrap();
        state_store
            .save_json_at(
                "reconcile/test-reconciler.json",
                &report(failed_at.saturating_add(1), "main"),
            )
            .unwrap();

        log.append_checked(Event::new(
            1,
            failed_at.saturating_add(1),
            0,
            EventKind::OrderStatusReconciled {
                client_order_id: 42,
                account_id: "main".into(),
                venue_id: "binance".into(),
                status: OrderStatus::Rejected,
                venue_order_id: None,
                filled_raw: 0,
                filled_exact: None,
            },
        ))
        .unwrap();
        event_store.write(&log_name, &log).unwrap();

        require_live_strategy_commands_resolved(
            &config,
            &root,
            &control_store.load().unwrap(),
            &prepared.commands,
        )
        .unwrap();
        assert_eq!(std::fs::read(&cursor_path).unwrap(), cursor_before_replay);
        std::fs::remove_dir_all(root).unwrap();
    }
}
