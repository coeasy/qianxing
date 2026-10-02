//! Outbox 投影游标的行为判据（V13 R2 第十七遍 #169b）。
//!
//! 改前：`project_event_log_to_outbox` 先把整本日志逐条 `serde_json::to_string` +
//! `validate`，调用处再按游标 `retain` 丢掉已投递的前缀。写面每追加一条事实都要为
//! 之前所有事实再序列化一遍，单条追加的代价随日志长度线性增长。现在过滤在序号上
//! 先做，序列化只覆盖游标之后的区间。
//!
//! 这里要守住两件事：① 过滤只能按游标切，切完剩下的那条字节序内容必须与全量投影
//! 里同一条完全一致（否则 Outbox 的消费端会读到换了的载荷）；② 日志名合法性仍由
//! 同一份规则给出（本轮把重复的字面量校验并进了 `validate_segment_name` 那条口径）。

use qx_core::{Event, EventKind, EventLog};
use qx_storage::{project_event_log_to_outbox, StorageError};

fn log_of(count: u64) -> EventLog {
    let mut log = EventLog::new();
    for seq in 0..count {
        // (ts, prio, seq) 必须严格递增，否则 append_checked 直接拒收。
        log.append_checked(Event::new(
            seq,
            seq + 1,
            2,
            EventKind::AccountBalanceSnapshot {
                account_id: "main".into(),
                venue_id: "binance".into(),
                balances: vec![],
            },
        ))
        .unwrap();
    }
    log
}

#[test]
fn projection_cursor_skips_only_the_already_delivered_prefix() {
    let log = log_of(12);
    let full = project_event_log_to_outbox("binance-main", &log, 0).unwrap();
    assert_eq!(full.len(), 12);

    for cursor in [1_u64, 5, 11, 12] {
        let tail = project_event_log_to_outbox("binance-main", &log, cursor).unwrap();
        assert_eq!(
            tail.len(),
            full.len() - cursor as usize,
            "游标 {cursor} 应留下 {} 条，实得 {} 条",
            full.len() - cursor as usize,
            tail.len()
        );
        assert!(
            tail.iter().all(|event| event.sequence >= cursor),
            "游标 {cursor} 之前的事件混进了投影结果：Outbox 会把已投递的事实再投一次"
        );
        // 切出来的尾巴必须与全量投影的同一条逐字节相同（event_id、载荷、topic 都不许变）。
        for (offset, event) in tail.iter().enumerate() {
            assert_eq!(
                event,
                &full[cursor as usize + offset],
                "游标 {cursor} 的第 {offset} 条与全量投影不一致"
            );
        }
    }

    // 游标落在日志末尾之后：一条都不投，而不是回退成全量。
    assert!(project_event_log_to_outbox("binance-main", &log, 12)
        .unwrap()
        .is_empty());
    assert!(project_event_log_to_outbox("binance-main", &log, 999)
        .unwrap()
        .is_empty());
}

#[test]
fn projection_rejects_names_the_shared_charset_rule_rejects() {
    let log = log_of(1);
    for name in [
        "",
        " ",
        "binance main",
        "binance.main",
        "binance/main",
        "../run",
    ] {
        let error = project_event_log_to_outbox(name, &log, 0).unwrap_err();
        assert!(
            matches!(error, StorageError::InvalidName(_)),
            "日志名 `{name}` 得到 {error:?}，期望 InvalidName：合法性口径必须由那份共用规则给出，\
             不能在这里再抄一份更宽或更窄的字面量"
        );
    }
    for name in ["run", "binance-main", "paper_main_2"] {
        assert!(
            project_event_log_to_outbox(name, &log, 0).is_ok(),
            "合法日志名 `{name}` 被拒了"
        );
    }
}
