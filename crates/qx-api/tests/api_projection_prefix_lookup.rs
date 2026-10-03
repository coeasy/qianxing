//! API 读模型的"已投影前缀"核对判据（V13 R2 第十七遍 #169c）。
//!
//! 改前：`project_event_log*` 对 `event.seq < next_seq` 的前缀逐条用线性 `find`
//! 找回同一序号。API 轮询桥（`crates/qx-cli/src/market_bridges.rs:167-176`）每个
//! tick 都把整本 Runtime EventLog 交给投影，于是每 tick 为前缀付 O(n)、整场长跑
//! O(n²)（本轮实测：16000 条日志二次投影 0.6038s，倍率 3.87/4.18/4.32）。
//!
//! 现在按序号二分。二分比线性扫更容易在边界上错一格，而"错一格"在这段代码里的
//! 表现是把一致的前缀判成漂移、或把漂移的前缀判成一致 —— 两种都足以让 API 悄悄
//! 偏离事实源。所以这里把前缀的**每一个**序号都单独篡改一次：任何一格找错，
//! 那一位就用别人的内容比对自己，用例当场红。

use qx_api::ApiState;
use qx_core::{Event, EventKind, EventLog};

const PREFIX: u64 = 200;

/// `prio` 是 Event 内容的一部分：篡改它等于篡改这条事实，而序号与时间戳都仍合法。
fn source(prio_at: Option<u64>) -> EventLog {
    let mut log = EventLog::new();
    for seq in 0..PREFIX {
        let prio = u8::try_from(if prio_at == Some(seq) { 7 } else { 2 }).unwrap();
        log.append(Event::new(seq, seq + 1, prio, EventKind::Settle));
    }
    log
}

fn projected(source: &EventLog) -> ApiState {
    let mut state = ApiState::default();
    assert_eq!(
        state
            .project_account_event_log("main", "binance", source)
            .unwrap(),
        PREFIX as usize,
        "首次投影应当把整本日志装进读模型"
    );
    state
}

#[test]
fn reprojecting_an_unchanged_log_skips_the_whole_prefix() {
    let source = source(None);
    let mut state = projected(&source);
    for _ in 0..3 {
        assert_eq!(
            state
                .project_account_event_log("main", "binance", &source)
                .unwrap(),
            0,
            "重复投影同一本日志新增了事件：前缀里有序号被当成缺口重装了一遍"
        );
    }
    assert_eq!(state.projections.len(), 1);
    let projection = state
        .projections
        .values()
        .next()
        .expect("投影读模型应当存在");
    assert_eq!(projection.events.len(), PREFIX as usize);
}

#[test]
fn every_prefix_position_reports_its_own_drift() {
    let clean = source(None);
    for seq in 0..PREFIX {
        let mut state = projected(&clean);
        let tampered = source(Some(seq));
        let error = state
            .project_account_event_log("main", "binance", &tampered)
            .expect_err(&format!("序号 {seq} 的内容漂移没有被发现"));
        assert!(
            error.contains(&format!("event_seq={seq}")),
            "序号 {seq} 的漂移报成了别的：{error}"
        );
        assert!(
            error.contains("内容漂移"),
            "序号 {seq} 的失败不是漂移通道：{error}"
        );
        assert_eq!(
            state.projections.values().next().unwrap().events.len(),
            PREFIX as usize,
            "漂移之后读模型被改动了 {} 条",
            state.projections.values().next().unwrap().events.len()
        );
    }
}

/// 全局（不带账户键）那条兼容投影走的是同一份按序号定位的前缀核对，也要各守一次。
#[test]
fn global_compat_projection_keeps_the_same_prefix_verdicts() {
    let clean = source(None);
    let mut state = ApiState::default();
    assert_eq!(state.project_event_log(&clean).unwrap(), PREFIX as usize);
    assert_eq!(state.project_event_log(&clean).unwrap(), 0);
    for seq in [0_u64, 1, PREFIX / 2, PREFIX - 2, PREFIX - 1] {
        let error = state
            .project_event_log(&source(Some(seq)))
            .expect_err(&format!("全局投影漏掉序号 {seq} 的漂移"));
        assert!(
            error.contains(&format!("event_seq={seq}")),
            "全局投影把序号 {seq} 的漂移报成：{error}"
        );
    }
}
