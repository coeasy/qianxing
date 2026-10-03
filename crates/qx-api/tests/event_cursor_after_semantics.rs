//! 两条事件读链必须共用同一个 `after` 口径（V12 R4-g）。
//!
//! 缺陷形态：`/events` 把 `after` 当成投影日志的**下标**切数组，`/events/live` 却按
//! **事件序号**过滤。日志被裁剪过（或序号从不为 0 开始）时，下标与序号必然错位：
//! 同一个游标会让两条链给出不同的事件批次，`after` 落在数组内还会把"游标已经失效"
//! 印成"没有新事件"，让客户端停在旧状态而不自知。

use qx_api::{ApiEventBus, ApiService, ApiState};
use qx_core::{Event, EventKind, Priority};

/// 让两条读链看到**同一份**被裁剪过的日志：总线按容量 3 裁到序号 5/6/7，
/// 事件日志直接以这三条起步（下标 0/1/2、序号 5/6/7 —— 两者从此不再相等）。
fn service_with_trimmed_logs() -> ApiService {
    let mut state = ApiState::default();
    state.event_bus = ApiEventBus::new(3).expect("用例总线容量必须合法");
    for seq in 0..8 {
        state
            .event_bus
            .publish(Event::new(seq, seq + 1, Priority::POST, EventKind::Settle))
            .expect("连续序号必须能发布");
    }
    for seq in 5..8 {
        state
            .events
            .append(Event::new(seq, seq + 1, Priority::POST, EventKind::Settle));
    }
    ApiService::new(state)
}

/// `/events` 直接吐事件数组，`/events/live` 吐投影信封数组；两边都只取序号比较。
fn seqs_in(body: &str, key: &str) -> Vec<u64> {
    let value: serde_json::Value =
        serde_json::from_str(body).expect("两条读链的正文必须是 JSON 数组");
    value
        .as_array()
        .unwrap_or_else(|| panic!("正文必须是数组: {body}"))
        .iter()
        .map(|event| {
            event[key]
                .as_u64()
                .unwrap_or_else(|| panic!("{key} 必须是序号: {event}"))
        })
        .collect()
}

/// 逐游标对照两条链：状态码与事件批次必须逐项一致，且都是"序号 > 游标"那一段。
#[test]
fn both_event_read_chains_answer_the_same_cursor_the_same_way() {
    let service = service_with_trimmed_logs();
    // 5/6/7 是这份日志剩下的全部：游标 4 之前都算失效，5 起才是有效游标。
    for (query, expected) in [
        ("", vec![5, 6, 7]),
        ("?after=5", vec![6, 7]),
        ("?after=6", vec![7]),
        ("?after=7", vec![]),
    ] {
        let snapshot = service.handle("GET", &format!("/events{query}"), "", 1);
        let live = service.handle("GET", &format!("/events/live{query}"), "", 1);
        assert_eq!(
            (snapshot.status, live.status),
            (200, 200),
            "游标 {query:?} 有效，两条链都必须给 200: {query} -> {}\nlive -> {}",
            snapshot.status,
            live.status
        );
        assert_eq!(
            (
                seqs_in(&snapshot.body, "seq"),
                seqs_in(&live.body, "event_seq")
            ),
            (expected.clone(), expected),
            "游标 {query:?} 下两条链必须交出同一批事件（按序号，不是按日志下标）"
        );
    }
}

/// 失效游标要说"回快照重取"，不能印成空批次。
#[test]
fn stale_or_ahead_cursors_are_rejected_on_both_chains() {
    let service = service_with_trimmed_logs();
    for query in ["?after=1", "?after=3", "?after=8", "?after=99"] {
        let snapshot = service.handle("GET", &format!("/events{query}"), "", 1);
        let live = service.handle("GET", &format!("/events/live{query}"), "", 1);
        assert_eq!(
            (snapshot.status, live.status),
            (409, 409),
            "游标 {query:?} 已经对不上这份日志，两条链都必须报 409:\n\
             events -> {} {}\nlive -> {} {}",
            snapshot.status,
            snapshot.body,
            live.status,
            live.body
        );
        assert!(
            snapshot.body.contains("event_cursor_requires_snapshot")
                && live.body.contains("event_cursor_requires_snapshot"),
            "409 必须点名失效口径，客户端才知道要回到快照: {query}"
        );
    }
    // 非无符号整数仍是客户端错误，且这条口径也归一到同一个解析器。
    assert_eq!(
        service.handle("GET", "/events?after=x", "", 1).status,
        400,
        "无效游标必须是 400"
    );
    assert_eq!(
        service.handle("GET", "/events/live?after=x", "", 1).status,
        400,
        "live 侧的无效游标必须同样是 400"
    );
}
