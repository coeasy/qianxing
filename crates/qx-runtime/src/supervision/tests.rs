//! 健康快照与监督器生命周期用例。

use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicU64;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

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
fn worker_context_sleep_is_interrupted_by_shutdown_request() {
    let supervisor = RuntimeSupervisor::new(config()).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let worker = supervisor
        .spawn_worker("market", move |context| {
            started_tx.send(()).unwrap();
            context.sleep_ms(5_000);
            finished_tx.send(context.should_stop()).unwrap();
            Ok(())
        })
        .unwrap();
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("worker entered cancellable wait");
    // Allow the worker to enter the first 200ms sleep slice before signaling; otherwise a broken
    // implementation could pass because the stop token was already set before sleeping began.
    thread::sleep(Duration::from_millis(50));
    supervisor.request_shutdown();
    assert!(finished_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("cancellable wait must wake promptly"));
    assert!(worker.join().unwrap().is_ok());
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

#[test]
fn the_grace_budget_is_counted_from_the_request_not_from_the_start_of_waiting() {
    // 反向验证依据：预算从 `wait_for_worker_finish` 进门那刻计时时，请求落下的那一轮
    // `waited_ms` 已经是 10 倍预算，长跑 worker 一分宽限都拿不到就被判超时。
    let supervisor = RuntimeSupervisor::new(config()).unwrap();
    let budget = supervisor.config().shutdown_timeout_ms;
    let worker = supervisor
        .spawn_worker("market", move |context| loop {
            if context.should_stop() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(1));
        })
        .unwrap();
    // 假时钟：每次注入的等待推进 POLL_INTERVAL_MS，因此"请求之前的等待"可以按预算倍数复现，
    // 而不必真等 —— `advance` 仍真睡 1ms，让被测 worker 有机会读到令牌。
    let clock = Arc::new(AtomicU64::new(0));
    let for_signal = Arc::clone(&clock);
    let for_now = Arc::clone(&clock);
    let for_sleep = Arc::clone(&clock);
    let ladder = wait_for_worker_finish(
        &supervisor,
        || worker.is_finished(),
        move || for_signal.load(Ordering::SeqCst) >= 10 * budget,
        move || for_now.load(Ordering::SeqCst),
        move |millis| advance(&for_sleep, millis),
    );
    match ladder {
        WorkerLadder::StoppedWithinBudget { waited_ms } => {
            assert!(
                waited_ms <= budget,
                "请求之前的等待被算进宽限预算: waited_ms={waited_ms} budget={budget}"
            );
        }
        other => unreachable!("长跑之后按停机请求退出不能判超时: {other:?}"),
    }
    assert!(supervisor.is_shutdown_requested());
    assert!(worker.join().unwrap().is_ok());
}

const REAL_SIGNAL_CHILD_ENV: &str = "QX_RUNTIME_REAL_SIGNAL_CHILD";

#[test]
fn real_shutdown_signal_child() {
    if std::env::var_os(REAL_SIGNAL_CHILD_ENV).as_deref() != Some(std::ffi::OsStr::new("1")) {
        return;
    }

    install_shutdown_signals();
    let supervisor = RuntimeSupervisor::new(config()).expect("valid runtime test config");
    let worker = supervisor
        .spawn_worker("market", |context| {
            while !context.should_stop() {
                thread::sleep(Duration::from_millis(2));
            }
            Ok(())
        })
        .expect("start signal test worker");
    println!("QX_SHUTDOWN_SIGNAL_READY");
    std::io::stdout().flush().expect("flush readiness marker");

    let started = Instant::now();
    let result = wait_for_worker_finish(
        &supervisor,
        || worker.is_finished(),
        shutdown_signalled,
        || started.elapsed().as_millis() as u64,
        |millis| thread::sleep(Duration::from_millis(millis)),
    );
    assert!(
        matches!(result, WorkerLadder::StoppedWithinBudget { .. }),
        "real OS signal must reach the worker shutdown ladder: {result:?}"
    );
    assert!(supervisor.is_shutdown_requested());
    assert!(worker.join().expect("join stopped test worker").is_ok());
}

#[test]
fn operating_system_shutdown_signal_reaches_worker_ladder() {
    let mut command = Command::new(std::env::current_exe().expect("test executable path"));
    command
        .arg("--exact")
        .arg("supervision::tests::real_shutdown_signal_child")
        .arg("--nocapture")
        .env(REAL_SIGNAL_CHILD_ENV, "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    let mut child = command.spawn().expect("spawn shutdown signal child");
    let stdout = child.stdout.take().expect("piped child stdout");
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut ready_sent = false;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    if !ready_sent {
                        let _ = ready_tx.send(Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "signal child exited before readiness",
                        )));
                    }
                    break;
                }
                Ok(_) if !ready_sent && line.trim() == "QX_SHUTDOWN_SIGNAL_READY" => {
                    ready_sent = true;
                    let _ = ready_tx.send(Ok(line));
                }
                Ok(_) => {}
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    break;
                }
            }
        }
    });
    let ready = ready_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|error| {
            let _ = child.kill();
            let _ = child.wait();
            panic!("signal child did not become ready: {error}");
        })
        .expect("read child readiness marker");
    assert_eq!(ready.trim(), "QX_SHUTDOWN_SIGNAL_READY");

    send_shutdown_signal(child.id());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().expect("poll signal child") {
            if !status.success() {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                panic!("signal child failed with {status}: {stderr}");
            }
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("worker did not exit after a real shutdown signal");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn send_shutdown_signal(process_id: u32) {
    let result = unsafe { libc::kill(process_id as libc::pid_t, libc::SIGTERM) };
    assert_eq!(result, 0, "send SIGTERM to shutdown child");
}

#[cfg(windows)]
fn send_shutdown_signal(process_id: u32) {
    const CTRL_BREAK_EVENT: u32 = 1;
    #[link(name = "kernel32")]
    extern "system" {
        fn GenerateConsoleCtrlEvent(control_type: u32, process_group_id: u32) -> i32;
    }
    let result = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, process_id) };
    assert_ne!(result, 0, "send CTRL_BREAK_EVENT to shutdown child group");
}
