//! Binance 用户流重连循环的沙盒验收：连接器、休眠与停止条件全部注入。
//!
//! 这里是 V10 §4.9「退避公式与终态判定统一委托 `qx-core::retry`」在连接器侧的可执行
//! 口径：预算按**连续**失败计，交付过事件的会话把它清零，因此长期健康的长连接不会
//! 因为生涯累计重连次数被判死。

use qx_adapter::{run_binance_user_stream, BinanceStreamRetryPolicy, BinanceUserStreamSession};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

fn policy() -> BinanceStreamRetryPolicy {
    BinanceStreamRetryPolicy::new(2, Duration::from_millis(10), Duration::from_millis(40)).unwrap()
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
        policy(),
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
        policy(),
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

#[test]
fn connect_failures_still_escalate_backoff_and_stop_at_the_budget() {
    let mut connect_count = 0_u32;
    let mut slept = Vec::new();
    let error = run_binance_user_stream(
        || {
            connect_count += 1;
            Err::<FakeUserStream, _>("injected connect failure".into())
        },
        policy(),
        || false,
        |delay| slept.push(delay),
        |_event| Ok(()),
    )
    .unwrap_err();
    assert!(error.contains("超过重试上限"), "{error}");
    assert_eq!(connect_count, 3);
    assert_eq!(
        slept,
        [Duration::from_millis(10), Duration::from_millis(20)]
    );
}

#[test]
fn healthy_sessions_do_not_exhaust_the_reconnect_budget() {
    // 反向验证依据：终态判定改回按生涯累计 `reconnects` 时，第 3 次会话就返回 Err（红）。
    let stop = Arc::new(Mutex::new(false));
    let stop_for_runner = Arc::clone(&stop);
    let stop_for_callback = Arc::clone(&stop);
    let mut connect_count = 0_u32;
    let mut slept = Vec::new();
    let report = run_binance_user_stream(
        || {
            connect_count += 1;
            // 每次会话交付一条事件后链路才断：会话健康，重连也确实发生了。
            Ok(FakeUserStream {
                events: vec![
                    Ok(Some(format!("tick-{connect_count}"))),
                    Err("injected drop".into()),
                ],
            })
        },
        policy(),
        move || *stop_for_runner.lock().unwrap(),
        |delay| slept.push(delay),
        move |event| {
            if event == "tick-4" {
                *stop_for_callback.lock().unwrap() = true;
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(connect_count, 4);
    assert_eq!(report.events, 4);
    assert_eq!(report.reconnects, 3);
    assert_eq!(report.consecutive_failures, 0);
    // 每次会话都交付过事件，所以退避永远从第一档重来，不会爬升。
    assert_eq!(slept, [Duration::from_millis(10); 3]);
}
