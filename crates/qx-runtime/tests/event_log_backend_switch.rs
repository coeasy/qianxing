//! 换后端打开同一份 EventLog 时的闸门判据（V13 R2 第十八遍 #226）。
//!
//! 改前：`open` / `open_configured` / `open_read_only` 只看自己那一套后端的文件，
//! 而「自己的文件不存在」被一律当成首次启动（`unwrap_or_default()`）。单文件后端写
//! `{name}.json`，分段后端写 `{name}.manifest.json` + `segments/`，两者在同一个
//! `storage.root` 下互不相交 —— 于是把 `storage.event_log_segment_events` 从空改成
//! 数值（或反向）之后：本轮实测读到 0 条事实（磁盘上有 5 条）、现金归零，随后追加的
//! 第一条事实拿到 seq 0，同一目录里从此并存两本历史（单文件 5 条 / 分段 1 条）。
//! 这与 `open` 自述的「不会用空状态掩盖生产数据问题」正好相反。
//!
//! 现在换后端要过闸门。这里守住闸门的三件事：① 两个方向都当场拒绝、且**不改磁盘**
//! （拒绝之后旧历史仍逐字可读）；② 读面同样拒绝（读模型印一本空账比报错更糟）；
//! ③ 不拦正常路径 —— 首次启动、以及同目录下别的账户/别的日志。

use qx_core::{Event, EventKind, EventLog};
use qx_runtime::LiveEventPipeline;
use qx_storage::{EventLogFileStore, SegmentedEventLogStore};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const LOG_NAME: &str = "binance-main";

fn temp_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "qianxing-backend-switch-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// 一份「4 条普通事实 + 1 条现金入账」的日志：够看出账簿有没有被归零。
fn seeded_log() -> EventLog {
    let mut log = EventLog::new();
    for seq in 0..4_u64 {
        log.append_checked(Event::new(seq, seq + 1, 2, EventKind::Settle))
            .unwrap();
    }
    log.append_checked(Event::new(
        4,
        5,
        9,
        EventKind::LedgerApplied {
            entry: qx_core::LedgerEntry {
                id: 1,
                account_id: "main".into(),
                currency: "USDT".into(),
                kind: qx_core::LedgerEntryKind::Settlement,
                amount: qx_core::Money::from_i64(500_000),
                instrument: None,
                quantity: qx_core::Quantity::ZERO,
                price: None,
                order_id: None,
                ts: 5,
                multiplier: 1,
                position_side: None,
            },
        },
    ))
    .unwrap();
    log
}

fn assert_refuses_backend_switch(error: &qx_core::QxError, stale_file: &str) {
    let text = format!("{error:?}");
    assert!(
        text.contains(LOG_NAME) && text.contains("换后端"),
        "报错没说是换后端导致的：{text}"
    );
    assert!(
        text.contains(stale_file),
        "报错没点名那份被留下的历史 {stale_file}：{text}"
    );
    assert!(
        text.contains("5 条事实"),
        "报错没说旧历史有多少条事实，运维无法判断损失：{text}"
    );
}

#[test]
fn switching_from_flat_to_segmented_is_refused_and_leaves_the_old_log_intact() {
    let root = temp_root("flat-first");
    let expected = seeded_log();
    EventLogFileStore::new(root.clone())
        .write(LOG_NAME, &expected)
        .unwrap();

    let error = LiveEventPipeline::open_configured(&root, LOG_NAME, "USDT", Some(8))
        .err()
        .expect("换成分段后端打开一份单文件历史被放行了：账户会读到空账本并从 seq 0 另起一本");
    assert_refuses_backend_switch(&error, "binance-main.json");

    // 拒绝必须是纯读：旧历史逐字还在，也没顺手建出第二本后端的目录。
    let still = EventLogFileStore::new(root.clone()).read(LOG_NAME).unwrap();
    assert_eq!(still.digest(), expected.digest(), "拒绝打开时改写了旧历史");
    assert!(
        !root.join("segments").exists(),
        "拒绝打开却建出了 segments/"
    );
    assert!(
        !root.join(format!("{LOG_NAME}.manifest.json")).exists(),
        "拒绝打开却写出了分段后端的 manifest"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn switching_from_segmented_to_flat_is_refused_and_leaves_the_old_log_intact() {
    let root = temp_root("segmented-first");
    let expected = seeded_log();
    SegmentedEventLogStore::new(root.clone(), 8)
        .unwrap()
        .write(LOG_NAME, &expected)
        .unwrap();

    let error = LiveEventPipeline::open(&root, LOG_NAME, "USDT")
        .err()
        .expect("换回单文件后端打开一份分段历史被放行了");
    assert_refuses_backend_switch(&error, "binance-main.manifest.json");

    let still = SegmentedEventLogStore::new(root.clone(), 8)
        .unwrap()
        .read(LOG_NAME)
        .unwrap();
    assert_eq!(still.digest(), expected.digest(), "拒绝打开时改写了旧历史");
    assert!(
        !root.join(format!("{LOG_NAME}.json")).exists(),
        "拒绝打开却写出了第二本单文件历史"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 读面（账户快照、Ledger 读模型、StrategyContext）同样不许读到一本空账：
/// 换错后端时印一份「现金 0」的快照，比当场报错危险得多。
#[test]
fn read_face_open_is_refused_too() {
    let root = temp_root("read-face");
    EventLogFileStore::new(root.clone())
        .write(LOG_NAME, &seeded_log())
        .unwrap();
    let error = LiveEventPipeline::open_read_only(&root, LOG_NAME, "USDT", Some(8))
        .err()
        .expect("读面打开绕过了换后端闸门");
    assert_refuses_backend_switch(&error, "binance-main.json");
    let _ = std::fs::remove_dir_all(root);
}

/// 闸门只拦「换后端」，不拦正常运行：首次启动、以及同一目录里别的日志名。
#[test]
fn the_gate_does_not_fire_on_a_fresh_root_or_an_unrelated_log_name() {
    let root = temp_root("no-false-positive");
    // 首次启动：两套后端都该能干净地打开。
    let mut fresh = LiveEventPipeline::open_configured(&root, LOG_NAME, "USDT", Some(8)).unwrap();
    fresh
        .register_order(order(1), 10)
        .expect("分段首次启动写不进自己的日志");
    let _ = LiveEventPipeline::open_read_only(&root, "brand-new-account", "USDT", Some(8))
        .expect("同目录下另一个账户的首次启动被闸门拦住了");

    // 另一本单文件历史与分段历史并存但名字不同 —— 打开任意一本都不该被拦。
    EventLogFileStore::new(root.clone())
        .write("other-account", &seeded_log())
        .unwrap();
    let _ = LiveEventPipeline::open(&root, "brand-new-account", "USDT")
        .expect("单文件首次启动被另一本无关历史拦住");
    let _ = LiveEventPipeline::open_configured(&root, LOG_NAME, "USDT", Some(8))
        .expect("自己的分段历史被当成另一本历史");

    // 另一本后端只留下一个空文件（没有事实）也不该拦：闸门拦的是"丢历史"，
    // 不是一份从未落过账的占位文件。
    EventLogFileStore::new(root.clone())
        .write("placeholder-account", &EventLog::new())
        .unwrap();
    let _ = LiveEventPipeline::open_configured(&root, "placeholder-account", "USDT", Some(8))
        .expect("另一本后端留下的空占位文件拦住了首次启动");

    // 非法名字仍归打开入口报错，闸门不抢口径。
    let _ = std::fs::remove_dir_all(root);
}

/// 数据库后端（SQLite / PostgreSQL）的 EventLog 与文件后端各存各的：qx-cli 的打开
/// 编排必须在选定数据库后端之前调用这条闸门，否则文件里的旧历史会归零后被孤儿化。
#[test]
fn the_database_gate_sees_both_file_backends() {
    let root = temp_root("db-flat");
    EventLogFileStore::new(root.clone())
        .write(LOG_NAME, &seeded_log())
        .unwrap();
    let error = LiveEventPipeline::assert_no_abandoned_file_log(&root, LOG_NAME)
        .expect_err("文件后端有历史时数据库闸门放行了");
    assert_refuses_backend_switch(&error, "binance-main.json");
    let _ = std::fs::remove_dir_all(root);

    let root = temp_root("db-segmented");
    SegmentedEventLogStore::new(root.clone(), 8)
        .unwrap()
        .write(LOG_NAME, &seeded_log())
        .unwrap();
    let error = LiveEventPipeline::assert_no_abandoned_file_log(&root, LOG_NAME)
        .expect_err("分段后端有历史时数据库闸门放行了");
    assert_refuses_backend_switch(&error, "binance-main.manifest.json");
    let _ = std::fs::remove_dir_all(root);

    // 干净目录、以及只有别的账户的目录，都不该拦。
    let root = temp_root("db-clean");
    LiveEventPipeline::assert_no_abandoned_file_log(&root, LOG_NAME)
        .expect("空数据目录被数据库闸门拦住");
    EventLogFileStore::new(root.clone())
        .write("other-account", &seeded_log())
        .unwrap();
    LiveEventPipeline::assert_no_abandoned_file_log(&root, LOG_NAME)
        .expect("同目录下别的账户的历史拦住了这个账户");
    // 非法名字由打开入口报错，闸门不抢口径。
    LiveEventPipeline::assert_no_abandoned_file_log(&root, "../escape")
        .expect("闸门替打开入口报了非法名");
    let _ = std::fs::remove_dir_all(root);
}

/// 闸门之后写面仍要能正常补投影（与 event_log_open_faces.rs 同一口径，这里只确认
/// 拒绝路径没有把恢复逻辑一起带走）。
#[test]
fn same_backend_open_still_recovers_the_crash_gap() {
    let root = temp_root("same-backend");
    let mut pipeline =
        LiveEventPipeline::open_configured(&root, LOG_NAME, "USDT", Some(8)).unwrap();
    for seq in 0..12_u64 {
        pipeline.register_order(order(seq), seq + 1).unwrap();
    }
    let facts = pipeline.log().len();
    std::fs::remove_dir_all(root.join("outbox/events")).unwrap();
    let reopened = LiveEventPipeline::open_configured(&root, LOG_NAME, "USDT", Some(8))
        .expect("同一后端重开被换后端闸门拦住了");
    assert_eq!(reopened.log().len(), facts);
    assert_eq!(outbox_files(&root), facts, "同后端重开没有补齐崩溃缺口");
    let _ = std::fs::remove_dir_all(root);
}

fn outbox_files(root: &Path) -> usize {
    let dir = root.join("outbox/events");
    if !dir.exists() {
        return 0;
    }
    std::fs::read_dir(dir)
        .map(|entries| entries.count())
        .unwrap_or(0)
}

fn order(seq: u64) -> qx_core::Order {
    use qx_core::{InstrumentId, Order, OrderStatus, Price, Quantity, Side, VenueId};
    Order {
        client_id: 100 + seq,
        instrument: InstrumentId::new("BTCUSDT", VenueId::new("BINANCE")),
        side: Side::Buy,
        qty: Quantity::from_i64(2),
        limit: Some(Price::from_i64(100)),
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
    }
}
