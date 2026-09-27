//! Binance 用户流重连循环的预算口径（V11 R3）。
//!
//! `max_reconnects` 判的是"连续失败次数"，不是进程生命周期的总额：会话交付过事件就说明
//! 连接是好的，计数必须复位，否则长跑的用户流会在第 N 次正常网络抖动后永久退出；反过来，
//! 什么都没交付的会话与本地回调失败都必须继续计数，否则这个循环永远不会停下来。
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use qx_adapter::{
    run_binance_user_stream, BinanceStreamPoll, BinanceStreamRetryPolicy, BinanceUserStreamSession,
};

struct FakeUserStream {
    events: Vec<Result<BinanceStreamPoll, String>>,
}

impl BinanceUserStreamSession for FakeUserStream {
    fn recv_event(&mut self) -> Result<BinanceStreamPoll, String> {
        self.events.remove(0)
    }

    fn close(&mut self) -> Result<(), String> {
        Ok(())
    }
}

fn policy(max_reconnects: u32) -> BinanceStreamRetryPolicy {
    BinanceStreamRetryPolicy::new(
        max_reconnects,
        Duration::from_millis(10),
        Duration::from_millis(40),
    )
    .unwrap()
}

#[test]
fn user_stream_runner_reconnects_with_injected_clock_and_stop() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stop_seen = Arc::clone(&seen);
    let callback_seen = Arc::clone(&seen);
    let mut connect_count = 0_u32;
    let mut slept = Vec::new();
    let report = run_binance_user_stream(
        || {
            connect_count += 1;
            Ok(FakeUserStream {
                events: vec![
                    Ok(BinanceStreamPoll::Event(format!("event-{connect_count}"))),
                    Ok(BinanceStreamPoll::Closed),
                ],
            })
        },
        policy(2),
        move || !stop_seen.lock().unwrap().is_empty(),
        |delay| slept.push(delay),
        move |event| {
            callback_seen.lock().unwrap().push(event.to_string());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(report.events, 1);
    assert_eq!(report.reconnects, 0);
    assert_eq!(connect_count, 1);
    assert!(slept.is_empty());
    assert_eq!(seen.lock().unwrap().as_slice(), ["event-1"]);
}

#[test]
fn user_stream_runner_reconnects_after_session_or_callback_error() {
    let stop = Arc::new(Mutex::new(false));
    let stop_for_runner = Arc::clone(&stop);
    let stop_for_callback = Arc::clone(&stop);
    let mut connect_count = 0_u32;
    let mut slept = Vec::new();
    let report = run_binance_user_stream(
        || {
            connect_count += 1;
            if connect_count == 1 {
                Ok(FakeUserStream {
                    events: vec![Ok(BinanceStreamPoll::Event("callback-error".into()))],
                })
            } else {
                Ok(FakeUserStream {
                    events: vec![
                        Ok(BinanceStreamPoll::Event("recovered".into())),
                        Ok(BinanceStreamPoll::Closed),
                    ],
                })
            }
        },
        policy(2),
        move || *stop_for_runner.lock().unwrap(),
        |delay| slept.push(delay),
        move |event| {
            if event == "callback-error" {
                return Err("injected callback failure".into());
            }
            *stop_for_callback.lock().unwrap() = true;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(connect_count, 2);
    assert_eq!(report.events, 1);
    assert_eq!(report.reconnects, 1);
    assert_eq!(slept, [Duration::from_millis(10)]);
}

/// 每轮都正常交付事件的长跑会话必须能越过 `max_reconnects`，且退避停在 base：
/// 修复前 `report.reconnects` 终身累加，第三条会话一结束就返回 Err 让 worker 退出。
#[test]
fn user_stream_runner_survives_reconnects_after_healthy_sessions() {
    let seen = Arc::new(Mutex::new(0_u32));
    let stop_seen = Arc::clone(&seen);
    let count_seen = Arc::clone(&seen);
    let mut slept = Vec::new();
    let report = run_binance_user_stream(
        || {
            Ok(FakeUserStream {
                events: vec![
                    Ok(BinanceStreamPoll::Event("a".into())),
                    Ok(BinanceStreamPoll::Event("b".into())),
                    Ok(BinanceStreamPoll::Closed),
                ],
            })
        },
        policy(2),
        move || *stop_seen.lock().unwrap() >= 6,
        |delay| slept.push(delay),
        move |_| {
            *count_seen.lock().unwrap() += 1;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(report.events, 6);
    assert_eq!(report.reconnects, 2);
    assert_eq!(
        slept,
        [Duration::from_millis(10), Duration::from_millis(10)],
        "恢复过的会话之后退避必须回到 base，而不是接着上一次的指数"
    );
}

/// 反面：什么都没交付的会话仍要按连续次数收口，否则"复位"变成了"永不放弃"。
#[test]
fn user_stream_runner_still_gives_up_when_sessions_never_deliver() {
    let mut slept = Vec::new();
    let failure = run_binance_user_stream(
        || {
            Ok(FakeUserStream {
                events: vec![Ok(BinanceStreamPoll::Closed)],
            })
        },
        policy(2),
        || false,
        |delay| slept.push(delay),
        |_| Ok(()),
    )
    .expect_err("空会话连续重连到上限后必须报错，不能无限循环");
    assert!(failure.contains("Binance 用户流关闭"), "实际错误 {failure}");
    assert_eq!(
        slept,
        [Duration::from_millis(10), Duration::from_millis(20)],
        "没有恢复时退避必须照旧指数增长"
    );
}

/// 回调失败通常是确定性错误：即使同一条会话此前交付过事件，也不得把它算作"已恢复"。
#[test]
fn user_stream_runner_does_not_reset_the_budget_on_callback_failures() {
    let mut slept = Vec::new();
    let failure = run_binance_user_stream(
        || {
            Ok(FakeUserStream {
                events: vec![
                    Ok(BinanceStreamPoll::Event("ok".into())),
                    Ok(BinanceStreamPoll::Event("bad".into())),
                ],
            })
        },
        policy(1),
        || false,
        |delay| slept.push(delay),
        |event| {
            if event == "bad" {
                return Err("injected callback failure".into());
            }
            Ok(())
        },
    )
    .expect_err("本地回调持续失败必须撞到上限后退出");
    assert!(
        failure.contains("injected callback failure"),
        "实际错误 {failure}"
    );
    assert_eq!(slept, [Duration::from_millis(10)]);
}

/// 空闲读窗不是链路故障（V13 R2 #165）：修复前一次 10 秒静默就被 `recv_event` 报错上抛，
/// 一个当天没有成交的纸面账户在几个窗口内就会被预算判死；修复后 40 个静默窗只是计数，
/// 会话收摊时才正常退出，一次预算也不花。
///
/// 空闲窗本身不会让 `should_stop` 变真，所以收摊由这条会话的 `close()` 武装：
/// 这样runner 既能跑满 40 个静默窗，又不会永远循环下去。
struct IdleThenClosing {
    remaining: usize,
    closed: Arc<AtomicUsize>,
}

impl BinanceUserStreamSession for IdleThenClosing {
    fn recv_event(&mut self) -> Result<BinanceStreamPoll, String> {
        if self.remaining == 0 {
            return Ok(BinanceStreamPoll::Closed);
        }
        self.remaining -= 1;
        Ok(BinanceStreamPoll::Idle)
    }

    fn close(&mut self) -> Result<(), String> {
        self.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn user_stream_runner_does_not_charge_idle_windows_against_the_reconnect_budget() {
    let mut slept = Vec::new();
    let closed = Arc::new(AtomicUsize::new(0));
    let closed_for_session = Arc::clone(&closed);
    let closed_for_stop = Arc::clone(&closed);
    let report = run_binance_user_stream(
        move || {
            Ok(IdleThenClosing {
                remaining: 40,
                closed: Arc::clone(&closed_for_session),
            })
        },
        // 预算只有 1：只要静默窗被当成故障，第 2 个窗就返回 Err 而不是 Ok(report)。
        policy(1),
        move || closed_for_stop.load(Ordering::SeqCst) > 0,
        |delay| slept.push(delay),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(report.idle_windows, 40, "空闲窗必须逐窗计数而不是丢掉");
    assert_eq!(report.events, 0, "空闲窗不得冒充交付");
    assert_eq!(report.reconnects, 0, "静默窗不能触发重连");
    assert!(slept.is_empty(), "没有重连就不该退避: {slept:?}");
}

/// 反面：空闲也不能冒充"链路是好的"。只静默、从未交付、每次都以断链收摊的会话必须
/// 照样按连续失败收口，否则把空闲当交付的过度修复会让这个循环永不退出。
/// `sessions >= 10` 是护栏：那条过度修复路径要能被判成失败，而不是把测试挂死。
#[test]
fn user_stream_runner_still_gives_up_when_only_idle_windows_are_delivered() {
    let mut slept = Vec::new();
    let sessions = Arc::new(AtomicUsize::new(0));
    let sessions_for_connect = Arc::clone(&sessions);
    let sessions_for_stop = Arc::clone(&sessions);
    let outcome = run_binance_user_stream(
        move || {
            sessions_for_connect.fetch_add(1, Ordering::SeqCst);
            Ok(FakeUserStream {
                events: (0..5)
                    .map(|_| Ok(BinanceStreamPoll::Idle))
                    .chain([Err("link down".into())])
                    .collect(),
            })
        },
        policy(2),
        move || sessions_for_stop.load(Ordering::SeqCst) >= 10,
        |delay| slept.push(delay),
        |_| Ok(()),
    );
    let failure = match outcome {
        Ok(report) => panic!(
            "空闲窗被当成了交付，连续失败预算一直复位：跑了 {} 轮会话、{} 个空闲窗仍未收口",
            report.reconnects, report.idle_windows
        ),
        Err(error) => error,
    };
    assert!(failure.contains("link down"), "实际错误 {failure}");
    assert_eq!(
        slept,
        [Duration::from_millis(10), Duration::from_millis(20)],
        "空闲窗不得给连续失败预算复位"
    );
}
