//! 读面/写面打开管线的面判据（V13 R2 第十七遍 #169a）。
//!
//! 改前：`open_with_store` 只要日志非空就做整段补投影（`store.write(name, log, 0)`），
//! 而账户快照、Ledger 读模型、StrategyContext 与 API 轮询桥都走同一个 `open`，于是
//! 一次 `GET /account/snapshot?account_id=&venue_id=` 会往 Outbox 目录写出与日志等量的文件
//! （本轮实测：4000 条日志 → 4000 个文件、5.49s）。补投影本身是写面需要的崩溃缺口
//! 恢复（EventLog 落盘成功、Outbox 未写完），但它绝不是读请求该付的代价。
//!
//! 现在 `OutboxRecovery` 把两面分开，这里守住分开的结果：
//! ① 读面打开一个空洞的 Outbox 后仍然空洞，且读到的事实与落盘的逐字节一致；
//! ② 写面打开同一个空洞仍然把它填上（否则本轮就是拿掉真修复来换性能）；
//! ③ 两条公开入口各自走各自的分支，任何一条被改回另一条都会红。

use qx_core::{Event, EventKind, EventLog};
use qx_runtime::LiveEventPipeline;
use qx_storage::EventLogFileStore;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const LOG_NAME: &str = "binance-main";

fn temp_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "qianxing-runtime-face-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn log_of(count: u64) -> EventLog {
    let mut log = EventLog::new();
    for seq in 0..count {
        // (ts, prio, seq) 严格递增是 append_checked 的前提。
        log.append_checked(Event::new(seq, seq + 1, 2, EventKind::Settle))
            .unwrap();
    }
    log
}

/// 写出一份有 `count` 条事实、但 Outbox 全空（= 崩溃缺口）的日志。
fn seeded_gap(count: u64, label: &str) -> (PathBuf, EventLog) {
    let root = temp_root(label);
    let log = log_of(count);
    EventLogFileStore::new(root.clone())
        .write(LOG_NAME, &log)
        .unwrap();
    assert_eq!(outbox_files(&root), 0, "夹具不该预置 Outbox");
    (root, log)
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

#[test]
fn read_only_open_never_touches_the_outbox_but_still_reads_every_fact() {
    let (root, expected) = seeded_gap(60, "read");
    let pipeline = LiveEventPipeline::open_read_only(&root, LOG_NAME, "USDT", None).unwrap();
    assert_eq!(
        outbox_files(&root),
        0,
        "读面打开把 60 条日志补投影进了 Outbox（实得 {} 份文件）：一次读请求又变成了一次写",
        outbox_files(&root)
    );
    assert_eq!(pipeline.log().len(), 60, "读面少读了事实");
    assert_eq!(
        pipeline.log().digest(),
        expected.digest(),
        "读面读到的日志与落盘的事实不同一本"
    );
    assert_eq!(pipeline.log().events()[0].seq, 0);
    assert_eq!(pipeline.log().events()[59].seq, 59);
    // 读面连日志文件本身都不许重写：追加锁一拿，并发写面就要排队。
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn write_face_open_still_reprojects_the_crash_gap() {
    let (root, expected) = seeded_gap(60, "write");
    let pipeline = LiveEventPipeline::open(&root, LOG_NAME, "USDT").unwrap();
    assert_eq!(
        outbox_files(&root),
        60,
        "写面打开没有补齐崩溃缺口：Outbox 里只有 {} 份文件，日志有 60 条事实。\
         本轮把读面分开时不许顺手删掉写面的恢复",
        outbox_files(&root)
    );
    assert_eq!(pipeline.log().digest(), expected.digest());
    let _ = std::fs::remove_dir_all(root);
}

/// 分段后端的两个公开入口同样要各走各的分支：`open_configured` 补投影，
/// `open_read_only`（带分段参数）不补。少了这一条，读面入口改回写面分支只在
/// 分段部署里才看得出来。
#[test]
fn segmented_backend_keeps_the_same_face_split() {
    let root = temp_root("segmented");
    let mut pipeline =
        LiveEventPipeline::open_configured(&root, "paper-segmented", "USDT", Some(8))
            .unwrap_or_else(|e| panic!("分段夹具打不开: {e:?}"));
    for seq in 0..20_u64 {
        pipeline
            .register_order(order(seq), seq + 1)
            .unwrap_or_else(|error| panic!("分段追加失败: {error:?}"));
    }
    let facts = pipeline.log().len();
    assert!(facts >= 20, "分段夹具至少要有 20 条事实，实得 {facts}");
    // 写面已经把自己的追加投出去了：清掉 Outbox 再造一次"崩溃缺口"。
    std::fs::remove_dir_all(root.join("outbox/events")).unwrap();
    assert_eq!(outbox_files(&root), 0);

    let read =
        LiveEventPipeline::open_read_only(&root, "paper-segmented", "USDT", Some(8)).unwrap();
    assert_eq!(
        outbox_files(&root),
        0,
        "分段读面写出 {} 份 Outbox 文件：读请求又不请自取地补投影",
        outbox_files(&root)
    );
    assert_eq!(read.log().len(), facts, "分段读面少读了事实");

    let write = LiveEventPipeline::open_configured(&root, "paper-segmented", "USDT", Some(8))
        .unwrap_or_else(|e| panic!("分段写面打不开: {e:?}"));
    assert_eq!(
        outbox_files(&root),
        facts,
        "分段写面没有补齐崩溃缺口（实得 {} 份）",
        outbox_files(&root)
    );
    assert_eq!(write.log().len(), facts);
    let _ = std::fs::remove_dir_all(root);
}

fn order(seq: u64) -> qx_core::Order {
    use qx_core::{InstrumentId, Order, OrderStatus, OrderTrace, Price, Quantity, Side, VenueId};
    Order {
        client_id: 100 + seq,
        instrument: InstrumentId::new("BTCUSDT", VenueId::new("BINANCE")),
        side: Side::Buy,
        qty: Quantity::from_i64(2),
        limit: Some(Price::from_i64(100)),
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: Some(OrderTrace {
            strategy_id: Some("demo".into()),
            signal_id: Some(1),
            intent_id: Some(seq),
            rule_version: Some("v1".into()),
        }),
        policy: None,
    }
}
