//! 健康快照与监督器生命周期用例。

use super::*;
use std::sync::atomic::AtomicU64;
use std::thread;
use std::time::Duration;

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

/// 注入的等待：既推进假时钟，也真睡 1ms，让被测 worker 有机会读到停机令牌。
fn advance(clock: &Arc<AtomicU64>, millis: u64) {
    clock.fetch_add(millis, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(1));
}

fn ladder_against(
    supervisor: &RuntimeSupervisor,
    worker: &JoinHandle<Result<(), String>>,
    signalled: impl Fn() -> bool,
) -> WorkerLadder {
    let clock = Arc::new(AtomicU64::new(0));
    let now = Arc::clone(&clock);
    let slept = Arc::clone(&clock);
    wait_for_worker_finish(
        supervisor,
        || worker.is_finished(),
        signalled,
        move || now.load(Ordering::SeqCst),
        move |millis| advance(&slept, millis),
    )
}

#[test]
fn worker_that_finishes_on_its_own_is_not_reported_as_shutdown() {
    let supervisor = RuntimeSupervisor::new(config()).unwrap();
    let worker = supervisor
        .spawn_worker("market", |context| {
            context.heartbeat(10)?;
            Ok(())
        })
        .unwrap();
    let ladder = ladder_against(&supervisor, &worker, || false);
    assert_eq!(ladder, WorkerLadder::Finished);
    assert!(!supervisor.is_shutdown_requested());
    assert!(worker.join().unwrap().is_ok());
}

#[test]
fn shutdown_request_reaches_the_worker_token_before_the_budget_runs_out() {
    let supervisor = RuntimeSupervisor::new(config()).unwrap();
    let observed_token = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&observed_token);
    let worker = supervisor
        .spawn_worker("market", move |context| loop {
            if context.should_stop() {
                flag.store(true, Ordering::SeqCst);
                return Ok(());
            }
            thread::sleep(Duration::from_millis(1));
        })
        .unwrap();
    let ladder = ladder_against(&supervisor, &worker, || true);
    assert!(
        matches!(ladder, WorkerLadder::StoppedWithinBudget { .. }),
        "终止请求没有让 worker 按令牌退出: {ladder:?}"
    );
    assert!(supervisor.is_shutdown_requested());
    assert!(
        observed_token.load(Ordering::SeqCst),
        "worker 线程没读到停机令牌"
    );
    assert!(worker.join().unwrap().is_ok());
}

#[test]
fn token_pressed_before_joining_still_counts_as_stopped_not_finished() {
    // 反向验证依据：`requested` 只由 `signalled()` 置真时，同一输入会报 Finished。
    let supervisor = RuntimeSupervisor::new(config()).unwrap();
    supervisor.request_shutdown();
    let worker = supervisor
        .spawn_worker("market", |context| {
            if context.should_stop() {
                return Ok(());
            }
            Err("未收到停机令牌".into())
        })
        .unwrap();
    let ladder = ladder_against(&supervisor, &worker, || false);
    assert!(matches!(ladder, WorkerLadder::StoppedWithinBudget { .. }));
    assert!(worker.join().unwrap().is_ok());
}

#[test]
fn worker_that_ignores_the_token_fails_after_the_shutdown_budget() {
    let supervisor = RuntimeSupervisor::new(config()).unwrap();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let worker = supervisor
        .spawn_worker("market", move |context| {
            // 模拟卡死的 worker：不轮询令牌，等外部放行才结束。
            let _ = release_rx.recv();
            let _ = context;
            Ok(())
        })
        .unwrap();
    let ladder = ladder_against(&supervisor, &worker, || true);
    let _ = release_tx.send(());
    let budget = supervisor.config().shutdown_timeout_ms;
    match ladder {
        WorkerLadder::StopTimedOut { waited_ms } => {
            assert!(
                waited_ms > budget,
                "预算内不得提前判超时 {waited_ms} <= {budget}"
            );
        }
        other => unreachable!("卡死的 worker 不能算停机成功: {other:?}"),
    }
    assert!(worker.join().unwrap().is_ok());
}
