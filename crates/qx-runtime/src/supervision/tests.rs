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

/// 迟到信号的阶梯夹具：假时钟按注入的等待推进，走过 `signal_after_ms` 才落下终止请求。
/// 带着假时钟一起用，是因为"预算给没给满"只能由退出那一刻的时钟说出口。
struct LateSignal {
    clock: Arc<AtomicU64>,
    signal_after_ms: u64,
}

impl LateSignal {
    fn new(signal_after_ms: u64) -> Self {
        Self {
            clock: Arc::new(AtomicU64::new(0)),
            signal_after_ms,
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }

    fn run(
        &self,
        supervisor: &RuntimeSupervisor,
        worker_finished: impl Fn() -> bool,
    ) -> WorkerLadder {
        let now = Arc::clone(&self.clock);
        let signalled = Arc::clone(&self.clock);
        let slept = Arc::clone(&self.clock);
        let signal_after_ms = self.signal_after_ms;
        wait_for_worker_finish(
            supervisor,
            worker_finished,
            move || signalled.load(Ordering::SeqCst) >= signal_after_ms,
            move || now.load(Ordering::SeqCst),
            move |millis| advance(&slept, millis),
        )
    }
}

/// 预算的起点是"第一次观察到停机请求"。这一格里阶梯先陪跑了 1.2 秒（预算 1 秒）才收到请求：
/// 按"从进循环起算"的写法，信号落下的那一跳 `waited_ms` 就已经超预算，worker 一秒优雅窗口
/// 都拿不到就被判超时。
#[test]
fn a_late_shutdown_signal_still_gets_the_full_graceful_window() {
    let mut config = config();
    config.shutdown_timeout_ms = 1_000;
    let supervisor = RuntimeSupervisor::new(config).unwrap();
    let budget = supervisor.config().shutdown_timeout_ms;
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
    let ladder = LateSignal::new(budget + 200).run(&supervisor, || worker.is_finished());
    assert!(
        observed_token.load(Ordering::SeqCst),
        "迟到的请求没有把令牌传到 worker"
    );
    match ladder {
        WorkerLadder::StoppedWithinBudget { waited_ms } => assert!(
            waited_ms <= budget,
            "waited_ms 只能是信号之后的等待，不能把陪跑时间算进来: {waited_ms} > {budget}"
        ),
        other => unreachable!("迟到的信号仍要给满优雅窗口: {other:?}"),
    }
    assert!(worker.join().unwrap().is_ok());
}

/// 反向对照：改成"从信号起算"不等于取消预算。读不到令牌的 worker 仍要在信号之后等满预算才判
/// 超时。那个 worker 到 10 倍预算之外才结束，所以删掉预算判据的用例是慢，不是挂死。
#[test]
fn a_late_signal_still_bounds_a_worker_that_ignores_it() {
    let mut config = config();
    config.shutdown_timeout_ms = 1_000;
    let supervisor = RuntimeSupervisor::new(config).unwrap();
    let budget = supervisor.config().shutdown_timeout_ms;
    let signal_at = budget + 200;
    let harness = LateSignal::new(signal_at);
    let ladder = harness.run(&supervisor, || {
        harness.elapsed_ms() >= signal_at + budget * 10
    });
    assert!(
        matches!(ladder, WorkerLadder::StopTimedOut { waited_ms } if waited_ms > budget),
        "worker 读不到令牌，阶梯仍要判超时: {ladder:?}"
    );
    assert!(
        harness.elapsed_ms() >= signal_at + budget,
        "预算要从信号起算才算满: 信号 {signal_at}ms，判超时 {}ms",
        harness.elapsed_ms()
    );
    assert!(supervisor.is_shutdown_requested());
}
