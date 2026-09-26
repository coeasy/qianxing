//! Binance 长连接重连循环的预算与静默口径（V11 R3、N10）。
//!
//! `max_reconnects` 判的是"连续失败次数"，不是进程生命周期的总额：会话交付过事件就说明
//! 连接是好的，计数必须复位，否则长跑的流会在第 N 次正常网络抖动后永久退出；反过来，
//! 什么都没交付的会话与本地回调失败都必须继续计数，否则这个循环永远不会停下来。
//! N10 再补一条：读超时是**静默**，既不算失败也不算恢复——把它算成失败，一条十分钟
//! 无人成交的薄行情就能杀掉 worker，而监督器是按进程粒度收工的。
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use qx_adapter::{
    run_binance_stream, run_binance_user_stream, BinanceStreamRead, BinanceStreamRetryPolicy,
    BinanceStreamSession,
};

struct FakeUserStream {
    events: Vec<Result<BinanceStreamRead<String>, String>>,
}

impl FakeUserStream {
    fn scripted(events: Vec<Result<BinanceStreamRead<String>, String>>) -> Self {
        Self { events }
    }
}

impl BinanceStreamSession for FakeUserStream {
    type Item = String;

    fn recv(&mut self, _now: u64) -> Result<BinanceStreamRead<Self::Item>, String> {
        self.events.remove(0)
    }

    fn close(&mut self) -> Result<(), String> {
        Ok(())
    }
}

fn message(text: &str) -> Result<BinanceStreamRead<String>, String> {
    Ok(BinanceStreamRead::Message(text.into()))
}

fn idle() -> Result<BinanceStreamRead<String>, String> {
    Ok(BinanceStreamRead::Idle)
}

fn closed() -> Result<BinanceStreamRead<String>, String> {
    Ok(BinanceStreamRead::Closed)
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
            Ok(FakeUserStream::scripted(vec![
                message(&format!("event-{connect_count}")),
                closed(),
            ]))
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
                Ok(FakeUserStream::scripted(vec![message("callback-error")]))
            } else {
                Ok(FakeUserStream::scripted(vec![
                    message("recovered"),
                    closed(),
                ]))
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
            Ok(FakeUserStream::scripted(vec![
                message("a"),
                message("b"),
                closed(),
            ]))
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
        || Ok(FakeUserStream::scripted(vec![closed()])),
        policy(2),
        || false,
        |delay| slept.push(delay),
        |_| Ok(()),
    )
    .expect_err("空会话连续重连到上限后必须报错，不能无限循环");
    assert!(failure.contains("Binance 流关闭"), "实际错误 {failure}");
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
            Ok(FakeUserStream::scripted(vec![
                message("ok"),
                message("bad"),
            ]))
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

/// V11 N10：静默不切断会话。修复前读超时以 `Err` 上来，薄行情会把 worker 直接杀掉。
#[test]
fn user_stream_runner_keeps_the_session_open_across_silence() {
    let stop = Arc::new(Mutex::new(false));
    let stop_for_runner = Arc::clone(&stop);
    let stop_for_callback = Arc::clone(&stop);
    let mut connect_count = 0_u32;
    let mut slept = Vec::new();
    let report = run_binance_user_stream(
        || {
            connect_count += 1;
            Ok(FakeUserStream::scripted(vec![
                idle(),
                idle(),
                message("after-silence"),
            ]))
        },
        policy(2),
        move || *stop_for_runner.lock().unwrap(),
        |delay| slept.push(delay),
        move |_| {
            *stop_for_callback.lock().unwrap() = true;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(connect_count, 1, "静默不是故障，不该触发重连");
    assert_eq!(report.events, 1);
    assert_eq!(report.reconnects, 0);
    assert!(slept.is_empty(), "同一条会话内部没有退避可言: {slept:?}");
}

/// 静默也不能算"已恢复"：一条只会静默的会话仍要按连续失败收口。
/// `connects >= 8` 只是逃生门——若把静默误判成恢复，这里会在断言前无限重连下去。
#[test]
fn user_stream_runner_does_not_reset_the_budget_on_silence() {
    let connects = Arc::new(AtomicU32::new(0));
    let connects_for_connect = Arc::clone(&connects);
    let mut slept = Vec::new();
    let failure = run_binance_user_stream(
        move || {
            connects_for_connect.fetch_add(1, Ordering::SeqCst);
            Ok(FakeUserStream::scripted(vec![idle(), idle(), closed()]))
        },
        policy(2),
        move || connects.load(Ordering::SeqCst) >= 8,
        |delay| slept.push(delay),
        |_| Ok(()),
    )
    .expect_err("只有静默的会话不等于健康，预算仍要走到尽头");
    assert!(failure.contains("Binance 流关闭"), "实际错误 {failure}");
    assert_eq!(
        slept,
        [Duration::from_millis(10), Duration::from_millis(20)],
        "静默既不加分也不减分"
    );
}

/// 行情流走的是同一颗引擎：静默留在会话里，每次读取拿到的都是调用方注入的时钟
/// （worker 用它给报价打接收时间戳）。
#[test]
fn market_stream_runner_survives_silence_and_uses_the_injected_clock() {
    struct FakeMarketStream {
        reads: Vec<Result<BinanceStreamRead<u64>, String>>,
        seen_clock: Arc<Mutex<Vec<u64>>>,
    }

    impl BinanceStreamSession for FakeMarketStream {
        type Item = u64;

        fn recv(&mut self, now: u64) -> Result<BinanceStreamRead<Self::Item>, String> {
            self.seen_clock.lock().unwrap().push(now);
            self.reads.remove(0)
        }

        fn close(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    let seen_clock = Arc::new(Mutex::new(Vec::new()));
    let quotes = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(Mutex::new(false));
    let stop_for_runner = Arc::clone(&stop);
    let stop_for_callback = Arc::clone(&stop);
    let seen_clock_for_connect = Arc::clone(&seen_clock);
    let quotes_for_callback = Arc::clone(&quotes);
    let mut clock = 0_u64;
    let mut slept = Vec::new();
    let report = run_binance_stream(
        move || {
            Ok(FakeMarketStream {
                reads: vec![
                    Ok(BinanceStreamRead::Idle),
                    Ok(BinanceStreamRead::Message(7)),
                ],
                seen_clock: Arc::clone(&seen_clock_for_connect),
            })
        },
        policy(2),
        move || *stop_for_runner.lock().unwrap(),
        |delay| slept.push(delay),
        move || {
            clock += 1;
            clock
        },
        move |quote| {
            quotes_for_callback.lock().unwrap().push(quote);
            *stop_for_callback.lock().unwrap() = true;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(*quotes.lock().unwrap(), [7], "静默之后的报价必须照常交付");
    assert_eq!(report.events, 1);
    assert_eq!(report.reconnects, 0, "静默不是故障，不该触发重连");
    assert!(slept.is_empty(), "同一条会话内部没有退避可言: {slept:?}");
    assert_eq!(
        *seen_clock.lock().unwrap(),
        [1, 2],
        "每次读取都取一次时钟并交给 recv：静默一轮、报价一轮，回调置停后不再多读"
    );
}

/// 行情流的故障仍按同一套连续失败口径收口：静默不等于永不放弃。
#[test]
fn market_stream_runner_gives_up_after_consecutive_transport_failures() {
    struct FailingMarketStream;

    impl BinanceStreamSession for FailingMarketStream {
        type Item = u64;

        fn recv(&mut self, _now: u64) -> Result<BinanceStreamRead<Self::Item>, String> {
            Err("injected transport failure".into())
        }

        fn close(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    let mut slept = Vec::new();
    let failure = run_binance_stream(
        || Ok(FailingMarketStream),
        policy(2),
        || false,
        |delay| slept.push(delay),
        || 1,
        |_| Ok(()),
    )
    .expect_err("持续故障必须撞到上限后退出，不能无限重连");
    assert!(
        failure.contains("injected transport failure"),
        "实际错误 {failure}"
    );
    assert_eq!(
        slept,
        [Duration::from_millis(10), Duration::from_millis(20)]
    );
}
