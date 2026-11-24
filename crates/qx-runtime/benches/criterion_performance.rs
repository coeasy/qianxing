//! Measure balance-snapshot cursor lookup against the pre-cursor reverse EventLog scan.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use qx_core::{
    AccountBalance, Event, EventKind, EventLog, InstrumentId, Money, Price, Priority, Quantity,
    VenueId,
};
use qx_runtime::LiveEventPipeline;
use qx_storage::EventLogFileStore;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ACCOUNT_ID: &str = "criterion-account";
const VENUE_ID: &str = "BINANCE";
const HISTORY_SIZES: [usize; 3] = [1_000, 8_000, 64_000];

struct PipelineFixture {
    root: PathBuf,
    pipeline: LiveEventPipeline,
}

impl Drop for PipelineFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn pipeline_with_snapshot_at_history_start(history_events: usize) -> PipelineFixture {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after UNIX epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "qx-runtime-balance-cursor-bench-{}-{nonce}-{history_events}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create benchmark EventLog directory");

    let mut log = EventLog::new();
    log.append(Event::new(
        0,
        1,
        Priority::APPLY,
        EventKind::AccountBalanceSnapshot {
            account_id: ACCOUNT_ID.into(),
            venue_id: VENUE_ID.into(),
            balances: vec![AccountBalance {
                asset: "USDT".into(),
                free: Money::from_i64(1_000),
                locked: Money::ZERO,
                borrowed: Money::ZERO,
                net_cash_balance: None,
                exact_amounts: None,
            }],
        },
    ));
    let instrument = InstrumentId::new("BTC-USDT", VenueId::new(VENUE_ID))
        .expect("benchmark instrument is valid");
    for offset in 0..history_events {
        let seq = offset as u64 + 1;
        log.append(Event::new(
            seq,
            seq + 1,
            Priority::MARKET,
            EventKind::MarketQuote {
                instrument: instrument.clone(),
                bid: Price::from_i64(100),
                ask: Price::from_i64(101),
                bid_qty: Quantity::from_i64(1),
                ask_qty: Quantity::from_i64(1),
                bid_exact: None,
                ask_exact: None,
                bid_qty_exact: None,
                ask_qty_exact: None,
            },
        ));
    }

    EventLogFileStore::new(&root)
        .write("balance-cursor", &log)
        .expect("write benchmark EventLog");
    let pipeline = LiveEventPipeline::open_read_only(&root, "balance-cursor", "USDT", None)
        .expect("open benchmark runtime pipeline");
    assert_eq!(
        pipeline.latest_account_balance_snapshot_cursor(ACCOUNT_ID, VENUE_ID),
        Some((0, 1))
    );
    PipelineFixture { root, pipeline }
}

fn legacy_reverse_scan(pipeline: &LiveEventPipeline) -> Option<(u64, u64)> {
    pipeline
        .log()
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::AccountBalanceSnapshot {
                account_id,
                venue_id,
                ..
            } if account_id == ACCOUNT_ID && venue_id.eq_ignore_ascii_case(VENUE_ID) => {
                Some((event.seq, event.ts))
            }
            _ => None,
        })
}

fn bench_balance_snapshot_cursor(c: &mut Criterion) {
    let fixtures: Vec<_> = HISTORY_SIZES
        .iter()
        .map(|size| (*size, pipeline_with_snapshot_at_history_start(*size)))
        .collect();
    let mut group = c.benchmark_group("qx_runtime_balance_snapshot_cursor");
    for (history_events, fixture) in &fixtures {
        group.bench_with_input(
            BenchmarkId::new("indexed", history_events),
            &fixture.pipeline,
            |b, pipeline| {
                b.iter(|| {
                    black_box(pipeline.latest_account_balance_snapshot_cursor(ACCOUNT_ID, VENUE_ID))
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("legacy_reverse_scan", history_events),
            &fixture.pipeline,
            |b, pipeline| b.iter(|| black_box(legacy_reverse_scan(pipeline))),
        );
    }
    group.finish();
}

fn configure_criterion() -> Criterion {
    Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(2))
}

criterion_group! {
    name = benches;
    config = configure_criterion();
    targets = bench_balance_snapshot_cursor
}
criterion_main!(benches);
