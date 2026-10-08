//! `EventLog::append_batch` 的**单事务语义**（方案 §5.2 DD-2 / §5.4 P1-1）。
//!
//! 逐条 `append_checked` 的写法在中途失败时会把前半批留在日志里：调用方以为整批被拒、
//! 实际已经写进一半，恢复重放于是看到一段本不该存在的历史。这里钉住"要么整批落地、
//! 要么一条不落"，以及合法批与空批的恒等行为。

use qx_core::clock::Ts;
use qx_core::{Event, EventKind, EventLog};

fn fact(seq: u64, ts: Ts) -> Event {
    Event::new(seq, ts, 2, EventKind::Settle)
}

#[test]
fn append_batch_rejects_the_whole_batch_without_touching_the_log() {
    let mut log = EventLog::new();
    log.append_batch(&[fact(0, 10), fact(1, 20)]).unwrap();
    let before = (log.len(), log.next_seq(), log.digest());

    // 批内 seq 与既有事实撞车：整批必须被拒，前半批也不许留下。
    assert!(
        log.append_batch(&[fact(2, 30), fact(3, 40), fact(0, 50)])
            .is_err(),
        "批内 seq 撞车必须整批拒绝"
    );
    assert_eq!(
        (log.len(), log.next_seq(), log.digest()),
        before,
        "整批被拒后日志必须一字不改"
    );

    // 批内自身违反因果序（ts 倒退）同样整批拒绝。
    assert!(
        log.append_batch(&[fact(2, 30), fact(3, 25)]).is_err(),
        "批内因果序违反必须整批拒绝"
    );
    assert_eq!((log.len(), log.next_seq(), log.digest()), before);
}

#[test]
fn append_batch_commits_a_valid_batch_in_one_go() {
    let mut log = EventLog::new();
    assert_eq!(log.append_batch(&[]).unwrap(), 0, "空批是恒等变换");
    assert_eq!(log.append_batch(&[fact(0, 10), fact(1, 20)]).unwrap(), 2);
    assert_eq!(log.len(), 2);
    assert_eq!(log.next_seq(), 2);
    assert_eq!(log.append_batch(&[fact(2, 30), fact(3, 40)]).unwrap(), 2);
    assert_eq!(log.len(), 4);
    assert_eq!(log.next_seq(), 4);
    log.validate().unwrap();
}
