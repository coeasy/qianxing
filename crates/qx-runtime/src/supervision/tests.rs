//! 健康快照与监督器生命周期用例。

use super::*;

#[test]
fn health_snapshot_detects_stale_ready_service() {
    let mut health = HealthRegistry::default();
    health.register("market", WorkerRole::MarketData).unwrap();
    health.heartbeat("market", 10).unwrap();
    assert_eq!(health.snapshot(20, 100).overall, OverallHealth::Ready);
    assert_eq!(health.snapshot(200, 100).overall, OverallHealth::Degraded);
}

#[test]
fn supervisor_registers_only_enabled_workers_and_exposes_shutdown() {
    let mut config = config();
    config.workers.push(WorkerConfig {
        id: "disabled".into(),
        role: WorkerRole::Scheduler,
        enabled: false,
        account_id: None,
        venue_id: None,
        endpoint: None,
        symbols: Vec::new(),
        settlement_currency: None,
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    });
    let supervisor = RuntimeSupervisor::new(config).unwrap();
    assert_eq!(
        supervisor
            .health()
            .lock()
            .unwrap()
            .snapshot(0, 100)
            .services
            .len(),
        2
    );
    assert!(!supervisor.is_shutdown_requested());
    supervisor.request_shutdown();
    assert!(supervisor.is_shutdown_requested());
}

#[test]
fn supervisor_runs_worker_and_records_terminal_state() {
    let supervisor = RuntimeSupervisor::new(config()).unwrap();
    let worker = supervisor
        .spawn_worker("market", |context| {
            context.heartbeat(10)?;
            assert!(!context.should_stop());
            Ok(())
        })
        .unwrap();
    assert!(worker.join().unwrap().is_ok());
    supervisor
        .health()
        .lock()
        .unwrap()
        .mark("api", ServiceStatus::Stopped, "test", None)
        .unwrap();
    let snapshot = supervisor.health().lock().unwrap().snapshot(10, 100);
    assert_eq!(snapshot.overall, OverallHealth::Stopped);
    assert_eq!(snapshot.services[0].status, ServiceStatus::Stopped);
}

#[test]
fn supervisor_converts_worker_panic_to_failed_health() {
    let supervisor = RuntimeSupervisor::new(config()).unwrap();
    let worker = supervisor
        .spawn_worker("market", |_context| -> Result<(), String> {
            panic!("injected worker panic");
        })
        .unwrap();
    assert!(worker.join().unwrap().is_err());
    supervisor
        .health()
        .lock()
        .unwrap()
        .mark("api", ServiceStatus::Stopped, "test", None)
        .unwrap();
    assert_eq!(
        supervisor.health().lock().unwrap().snapshot(0, 100).overall,
        OverallHealth::Failed
    );
}
