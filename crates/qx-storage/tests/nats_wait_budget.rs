//! NATS 适配器三条等待预算的契约，以及「发布者连接在没有 Tokio reactor 的线程上
//! 也必须返回」的回归。
//!
//! 这些用例不需要 broker：本机 TCP 桩分别模拟端口被拒、接受连接但不发 INFO、
//! 握手完成但对任何请求有去无回三种链路状态。需要真实 JetStream 的端到端契约在
//! `nats_jetstream.rs` 里，靠 CI 的服务容器以 `--ignored` 运行。
//!
//! 判别余量说明：修复前这三条等待全部退回 async-nats 自己的默认值（握手与发布确认
//! 5s、`$JS.API.*` 请求 10s），所以断言一律用「3s 内返回」作界线——预算给 300ms 时
//! 必须远早于 3s，没有预算时必须超过 3s。

#![cfg(feature = "nats")]

use qx_storage::{
    NatsJetStreamConsumer, NatsJetStreamPublisher, NatsWaitBudget, OutboxEvent, OutboxPublisher,
};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// 用例给三条等待的预算，远小于依赖默认的 5s。
const BUDGET_MS: u64 = 300;
/// 「本仓写了预算」与「退回依赖默认」的分界线。
const MAX_WAIT: Duration = Duration::from_secs(3);

fn budget() -> NatsWaitBudget {
    NatsWaitBudget {
        connect_timeout_ms: BUDGET_MS,
        request_timeout_ms: BUDGET_MS,
        pull_expires_ms: BUDGET_MS,
    }
}

fn event() -> OutboxEvent {
    OutboxEvent {
        event_id: "wait-budget".into(),
        topic: "qianxing.wait.budget".into(),
        partition_key: "budget".into(),
        sequence: 1,
        schema_version: 1,
        trace_id: "wait-budget".into(),
        payload: "{\"sequence\":1}".into(),
        created_ts: 10,
        attempts: 0,
    }
}

/// 端口被拒（没有进程在听）时用的地址。
fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("占用临时端口失败");
    let addr = listener.local_addr().expect("读取临时端口失败");
    drop(listener);
    format!("nats://{addr}")
}

/// 接受 TCP 连接但一个字节都不发的桩：客户端读不到 INFO，只能靠连接预算脱身。
fn silent_accept_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定静默桩失败");
    let addr = listener.local_addr().expect("读取静默桩端口失败");
    thread::spawn(move || {
        // 持有所有连接直到进程结束，既不 INFO 也不 PONG。
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    format!("nats://{addr}")
}

/// 握手能过（发 INFO、把 PING 回成 PONG）、之后对任何请求有去无回的桩。
///
/// async-nats 的握手是「读 INFO → 写 CONNECT+PING → 等 PONG」，只发 INFO 的桩会让
/// 客户端在握手阶段就失败，量不到请求侧的价钱。
fn handshake_only_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定握手桩失败");
    let addr = listener.local_addr().expect("读取握手桩端口失败");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            thread::spawn(move || {
                let _ = stream.write_all(
                    b"INFO {\"server_id\":\"stub\",\"server_name\":\"stub\",\"version\":\"2.10.0\",\"proto\":1,\"host\":\"127.0.0.1\",\"port\":4222,\"headers\":true,\"max_payload\":1048576,\"jetstream\":true}\r\n",
                );
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(size) => {
                            buffer.extend_from_slice(&chunk[..size]);
                            while let Some(position) =
                                find(&buffer, b"PING").or_else(|| find(&buffer, b"+OK"))
                            {
                                let skip = if buffer[position] == b'P' { 4 } else { 3 };
                                buffer.drain(..position + skip);
                                let _ = stream.write_all(b"PONG\r\n");
                            }
                        }
                    }
                }
            });
        }
    });
    format!("nats://{addr}")
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 在一条全新的普通线程上跑 `work`，返回（耗时，结果文本）。
///
/// 线程本身没有 Tokio reactor，正是 `NatsJetStreamPublisher::connect` 曾经的崩溃点；
/// 用独立线程跑还能顺带证明等待真的会返回，而不是把调用方吊死。
fn on_plain_thread<R>(
    work: impl FnOnce() -> R + Send + 'static,
) -> (Duration, Result<String, String>)
where
    R: std::fmt::Debug + Send + 'static,
{
    let started = Instant::now();
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let outcome = work();
        let text = format!("{outcome:?}");
        let _ = sender.send(text);
    });
    // 看门狗放宽到 30s：这条用例判的是「会不会回来」，不是「几秒回来」。
    let outcome = receiver
        .recv_timeout(Duration::from_secs(30))
        .map_err(|_| "30s 内没有返回".to_string());
    let _ = handle.join();
    (started.elapsed(), outcome)
}

#[test]
fn publisher_connect_returns_on_a_thread_without_a_reactor() {
    let (elapsed, outcome) = on_plain_thread(|| {
        NatsJetStreamPublisher::connect(&handshake_only_url(), "qx.budget", budget())
    });
    let outcome = outcome.expect("publisher connect 必须返回而不是把线程吊死");
    assert!(
        outcome.contains("Ok"),
        "握手成功的桩上 connect 应当建立出 JetStream 上下文，实际: {outcome}\
         （`jetstream::new` 会 spawn ack 监视任务，必须在已进入的 runtime 里调用，\
         否则在未进入 reactor 的线程上当场 panic）"
    );
    assert!(
        elapsed < MAX_WAIT,
        "connect 用了 {:.1}s",
        elapsed.as_secs_f64()
    );
}

#[test]
fn consumer_connect_returns_on_a_thread_without_a_reactor() {
    let (elapsed, outcome) = on_plain_thread(|| {
        NatsJetStreamConsumer::connect(
            &closed_port_url(),
            "STREAM",
            "consumer",
            "group",
            3,
            budget(),
        )
    });
    let outcome = outcome.expect("consumer connect 必须返回");
    assert!(outcome.contains("Err"), "被拒端口必须报错: {outcome}");
    assert!(
        elapsed < MAX_WAIT,
        "connect 用了 {:.1}s",
        elapsed.as_secs_f64()
    );
}

#[test]
fn connect_gives_up_within_the_connect_budget() {
    // 连接被接受但对端不发 INFO：只有本仓写下的连接预算能让它在此脱身。
    let url = silent_accept_url();
    let started = Instant::now();
    let error = NatsJetStreamPublisher::connect(&url, "qx.budget", budget())
        .expect_err("静默对端必须让 connect 失败");
    let elapsed = started.elapsed();
    assert!(
        elapsed < MAX_WAIT,
        "connect 没有遵守 {BUDGET_MS}ms 的连接预算，等了 {:.1}s: {error}",
        elapsed.as_secs_f64()
    );
}

#[test]
fn publish_gives_up_within_the_request_budget() {
    let publisher = NatsJetStreamPublisher::connect(&handshake_only_url(), "qx.budget", budget())
        .expect("握手桩上应当能建立发布者");
    let started = Instant::now();
    let error = publisher
        .publish(&event())
        .expect_err("对端不回 ack，publish 必须失败而不是永久等待");
    let elapsed = started.elapsed();
    assert!(
        elapsed < MAX_WAIT,
        "publish 没有遵守 {BUDGET_MS}ms 的请求预算，等了 {:.1}s: {error}",
        elapsed.as_secs_f64()
    );
}

#[test]
fn consumer_stream_lookup_gives_up_within_the_request_budget() {
    let started = Instant::now();
    let error = NatsJetStreamConsumer::connect(
        &handshake_only_url(),
        "STREAM",
        "consumer",
        "group",
        3,
        budget(),
    )
    .expect_err("对端不回 $JS.API 请求，connect 必须失败");
    let elapsed = started.elapsed();
    assert!(
        elapsed < MAX_WAIT,
        "get_stream/get_consumer 没有遵守 {BUDGET_MS}ms 的请求预算，等了 {:.1}s: {error}",
        elapsed.as_secs_f64()
    );
}

#[test]
fn out_of_range_budget_is_rejected_before_any_io() {
    let url = closed_port_url();
    for (field, value) in [
        ("connect_timeout_ms", 1u64),
        ("request_timeout_ms", 1u64),
        ("pull_expires_ms", 1u64),
        ("connect_timeout_ms", 60_001),
        ("request_timeout_ms", 300_001),
        ("pull_expires_ms", 30_001),
    ] {
        let mut budget = budget();
        match field {
            "connect_timeout_ms" => budget.connect_timeout_ms = value,
            "request_timeout_ms" => budget.request_timeout_ms = value,
            "pull_expires_ms" => budget.pull_expires_ms = value,
            _ => unreachable!(),
        }
        let started = Instant::now();
        let error = NatsJetStreamPublisher::connect(&url, "qx.budget", budget)
            .expect_err("越界预算必须被拒绝");
        assert!(
            error.contains(field) && error.contains("必须在"),
            "错误要指名字段 {field}: {error}"
        );
        // 被拒端口本身要 ~2s 才失败；预算校验必须在连接之前，所以这里必须远小于它。
        assert!(
            started.elapsed() < Duration::from_millis(BUDGET_MS * 2),
            "{field} 的校验应当先于 I/O"
        );
    }
}

#[test]
fn default_budget_matches_the_dependency_defaults_it_replaces() {
    // 握手与拉取就是过去实际生效的数；请求/确认把依赖的 10s 请求默认与 5s 确认默认
    // 并成 5s（只收紧不放宽）。改动任何一格都等于改动运行时行为，必须走配置面。
    let default = NatsWaitBudget::default();
    assert_eq!(default.connect_timeout_ms, 5_000);
    assert_eq!(default.request_timeout_ms, 5_000);
    assert_eq!(default.pull_expires_ms, 1_000);
    default.validate().expect("默认预算必须合法");
}
