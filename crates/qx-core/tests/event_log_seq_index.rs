//! `EventLog` 的重复序号检测必须与日志长度无关（V13 R2 第十一遍 #187）。
//!
//! 旧实现是每条 `append_checked` 扫一遍 `events`，而 `ReplayVerifier::replay` 逐条走
//! `append_checked`、`refresh_latest` 每个 tick 都会整场重放 —— 于是一场重放是 O(n²)，
//! 长跑的累计代价还要再乘一遍 tick 数。改成维护 seq 索引后，这里钉两件事：
//! ① 规则本身没变（跨位置的重复 seq、被跳过的序号、非单调 seq 仍然一律拒绝）；
//! ② 两条写入路径（`append` 与 `append_checked`）都要把 seq 记进索引，
//!    漏一条就退回"接受了重号事实"的旧坑。

use qx_core::{Event, EventKind, EventLog, Priority, ReplayVerifier};

fn event(seq: u64, ts: u64) -> Event {
    Event::new(seq, ts, Priority::POST, EventKind::Settle)
}

fn increasing_log(count: u64) -> EventLog {
    let mut log = EventLog::new();
    for index in 0..count {
        log.append_checked(event(index, index + 1)).unwrap();
    }
    log
}

/// 重复 seq 出现在**开头**、新事件的时间戳足够晚，因果排序检查放行 ——
/// 只有"整本日志的 seq 集合"这一层能拒绝它。旧实现在这里是全表扫描。
#[test]
fn append_checked_rejects_a_seq_already_used_by_an_earlier_event() {
    let mut log = increasing_log(4);
    let error = log
        .append_checked(event(1, 100))
        .expect_err("seq=1 已被第 2 条事件占用，晚到的时间戳不能赦免重号");
    assert!(format!("{error:?}").contains("事件 seq 重复"), "{error:?}");
    assert_eq!(log.len(), 4, "被拒绝的事件不得留下半个尾部");
    assert_eq!(log.next_seq(), 4);
}

/// `append` 是不作序的载入路径（`from_json` 走它），它记的号同样要参与后续裁决。
#[test]
fn events_loaded_via_append_still_count_as_seen_sequences() {
    let mut log = EventLog::new();
    log.append(event(5, 6));
    let error = log
        .append_checked(event(5, 7))
        .expect_err("载入路径给过的 seq 也是用过的 seq");
    assert!(format!("{error:?}").contains("事件 seq 重复"), "{error:?}");
    assert_eq!(log.len(), 1);
}

/// `validate` 仍是独立的第二道：`append` 允许重号入库，但整本日志过不了校验。
#[test]
fn validate_still_catches_duplicates_that_append_allowed() {
    let mut log = EventLog::new();
    log.append(event(7, 8));
    log.append(event(7, 9));
    assert_eq!(log.len(), 2, "append 本身不设闸门，这一点没有改变");
    assert!(log
        .validate()
        .expect_err("重号日志不可接受")
        .to_string()
        .contains("seq 重复"));
}

/// 与旧扫描等价的正向一侧：合法日志照常通过，且摘要不受索引改动影响。
#[test]
fn increasing_logs_replay_and_digest_as_before() {
    let log = increasing_log(200);
    log.validate().unwrap();
    let facts = ReplayVerifier::replay_facts(log.events()).unwrap();
    assert_eq!(facts.events, 200);
    assert_eq!(facts.log_digest, log.digest());
}

/// 伸缩量具，不是常驻判据（墙钟断言容易在 CI 上掷硬币）。
/// 复跑：`cargo test -p qx-core --release -- --ignored --nocapture`
/// 判读：事件数翻倍时耗时倍率应接近 2（线性带对数因子），旧实现这里是 4。
#[test]
#[ignore = "手动复跑的伸缩量具"]
fn replay_cost_scales_near_linearly_with_event_count() {
    let seconds = |count: u64| {
        let events = increasing_log(count).events().to_vec();
        let started = std::time::Instant::now();
        ReplayVerifier::replay(&events).unwrap();
        started.elapsed().as_secs_f64()
    };
    let small = seconds(20_000);
    let large = seconds(40_000);
    println!(
        "replay 20_000 = {small:.4}s, 40_000 = {large:.4}s, 倍率 = {:.2}",
        large / small
    );
    assert!(
        large / small < 3.5,
        "翻倍耗时倍率 {:.2} 说明重复序号检测又回到了随长度线性增长",
        large / small
    );
}
