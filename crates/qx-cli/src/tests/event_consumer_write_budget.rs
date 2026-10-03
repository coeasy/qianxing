//! 事件 consumer handler 的 stdin 写入必须与退出轮询同受 `handler.timeout_ms` 约束
//! （V13 R2 第三十五遍 #282）。
//!
//! 与 `crates/qx-cli/src/strategy_host.rs` 的 #281 同族，但这条住在
//! `#[cfg(feature = "nats")]` 下、是 opt-in 后端，不在默认发布 exe 里。
//! `invoke_event_consumer_handler` 修复前把 `write_all` 直接跑在主线程，只有后面的
//! `try_wait` 轮询守 `timeout_ms`——一旦外部 handler 存活却不从 stdin 取字节，超出 OS
//! 匿名管道缓冲（~64KB）的那段写入永久阻塞，本进程就此卡死、连关停都读不到。跨进程
//! Outbox 事件里 AccountPositionSnapshot/AccountBalanceSnapshot 把整段 Vec 内联进单条
//! payload，足以越过 64KB，所以这不是假设场景。本用例把 wedge 的 handler + 超限 payload
//! 组在一起，断言调用在预算内以可观测超时失败，而不是永久阻塞。

use super::*;

/// 造一份 consumer 事件：payload 远大于 OS 匿名管道缓冲，让不取输入的 handler 必然把写侧打满。
fn oversized_outbox_event() -> OutboxEvent {
    OutboxEvent {
        event_id: "event-consumer-write-budget".into(),
        topic: "qx.eventlog".into(),
        partition_key: "event-consumer-write-budget".into(),
        sequence: 1,
        schema_version: OutboxEvent::LATEST_SCHEMA_VERSION,
        trace_id: "trace-event-consumer-write-budget".into(),
        payload: "x".repeat(200_000),
        created_ts: 1_700_000_000,
        attempts: 0,
    }
}

/// #282 行为判据：handler"活着但从不读 stdin"时，写侧必须受 `timeout_ms` 收口。
///
/// ping 继承 stdin 读端却一字节都不取，且是被直接跟踪的子进程，`kill()` 即关闭读端、
/// 放行阻塞在 write_all 上的写线程——于是超预算的写入以超时通道收场，调用永不返回的风险被消掉。
#[test]
fn live_but_non_draining_event_consumer_write_is_bounded_not_hanging() {
    const TIMEOUT_MS: u64 = 2_000;
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    let ping = format!("{system_root}\\System32\\PING.EXE");
    let handler = EventConsumerHandler::for_test(
        ping,
        vec!["-n".into(), "300".into(), "127.0.0.1".into()],
        TIMEOUT_MS,
    );
    let started = std::time::Instant::now();
    let error = invoke_event_consumer_handler(&handler, &oversized_outbox_event())
        .expect_err("handler 存活但不接收输入：写入必须在预算内失败，而不是永久阻塞");
    let elapsed = started.elapsed();
    assert!(
        error.contains("写入事件 consumer handler stdin 超时")
            && error.contains("handler 存活但不接收输入"),
        "写侧失败没走到超时通道（说明仍在无限阻塞或误落了别的分支）: {error}",
    );
    assert!(
        error.contains(&format!("timeout_ms={TIMEOUT_MS}")),
        "超时诊断必须点名所用预算，运维才看得出是写侧界住了: {error}",
    );
    assert!(
        elapsed < std::time::Duration::from_millis(TIMEOUT_MS * 5),
        "写入没有被 timeout_ms 收口：耗时 {elapsed:?} 说明调用仍在无限阻塞",
    );
}
