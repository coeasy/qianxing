//! Binance 用户流重连循环的预算口径（V11 R3）。
//!
//! `max_reconnects` 判的是"连续失败次数"，不是进程生命周期的总额：会话交付过事件就说明
//! 连接是好的，计数必须复位，否则长跑的用户流会在第 N 次正常网络抖动后永久退出；反过来，
//! 什么都没交付的会话与本地回调失败都必须继续计数，否则这个循环永远不会停下来。
use std::sync::{Arc, Mutex};
use std::time::Duration;

use qx_adapter::{run_binance_user_stream, BinanceStreamRetryPolicy, BinanceUserStreamSession};

struct FakeUserStream {
    events: Vec<Result<Option<String>, String>>,
}

impl BinanceUserStreamSession for FakeUserStream {
    fn recv_event(&mut self) -> Result<Option<String>, String> {
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
                events: vec![Ok(Some(format!("event-{connect_count}"))), Ok(None)],
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
                    events: vec![Ok(Some("callback-error".into()))],
                })
            } else {
                Ok(FakeUserStream {
                    events: vec![Ok(Some("recovered".into())), Ok(None)],
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
                events: vec![Ok(Some("a".into())), Ok(Some("b".into())), Ok(None)],
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
                events: vec![Ok(None)],
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
                events: vec![Ok(Some("ok".into())), Ok(Some("bad".into()))],
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
