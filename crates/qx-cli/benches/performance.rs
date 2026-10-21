//! Deterministic, dependency-free microbenchmarks for the hot paths named by WP-0.2.
//!
//! These are directional measurements, not portable performance guarantees. Run on the
//! same machine and build profile when comparing results.

use qx_core::{
    Event, EventKind, EventLog, InstrumentId, Order, OrderStatus, Price, Priority, Quantity,
    ReplayVerifier, Side,
};
#[cfg(feature = "sqlite")]
use qx_guanxing::QuoteTick;
#[cfg(feature = "sqlite")]
use qx_runtime::{LiveEventPipeline, RuntimeEventEnvelope};
use qx_storage::SegmentedEventLogStore;
#[cfg(feature = "sqlite")]
use qx_storage::SqliteEventLogStore;
use qx_xingban::{BookLevel, OrderBookMatchingEngine, OrderBookSnapshot};
use std::hint::black_box;
#[cfg(feature = "sqlite")]
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SIZES: [usize; 3] = [1_000, 8_000, 64_000];
const REPEATS: usize = 5;
const MIN_MEASURE_WINDOW: Duration = Duration::from_millis(10);
const MAX_ITERATIONS_PER_SAMPLE: usize = 16 * 1024 * 1024;

fn events(size: usize) -> Vec<Event> {
    (0..size)
        .map(|seq| {
            Event::new(
                seq as u64,
                seq as u64 + 1,
                Priority::POST,
                EventKind::Settle,
            )
        })
        .collect()
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

/// Return the median per-operation duration, calibrating fast operations so each sample
/// spans long enough to survive coarse platform timer resolution. Without batching, cached
/// EventLog::digest() can round to 0 ms on Windows and produce meaningless throughput.
fn measure(mut operation: impl FnMut(), repeats: usize) -> Duration {
    operation(); // warm up code and data before calibration
    let mut iterations = 1usize;
    loop {
        let start = Instant::now();
        for _ in 0..iterations {
            operation();
        }
        if start.elapsed() >= MIN_MEASURE_WINDOW || iterations >= MAX_ITERATIONS_PER_SAMPLE {
            break;
        }
        iterations = iterations.saturating_mul(2).min(MAX_ITERATIONS_PER_SAMPLE);
    }
    let mut samples = Vec::with_capacity(repeats);
    for _ in 0..repeats {
        let start = Instant::now();
        for _ in 0..iterations {
            operation();
        }
        samples.push(Duration::from_secs_f64(
            start.elapsed().as_secs_f64() / iterations as f64,
        ));
    }
    median(samples)
}

fn report(name: &str, n: usize, duration: Duration) {
    let seconds = duration.as_secs_f64();
    let iterations_per_second = if seconds > 0.0 { 1.0 / seconds } else { 0.0 };
    // `EventLog::digest()` is a cached O(1) read, so dividing the log's event count by
    // its duration would falsely report billions of events scanned per second.
    let throughput = if name == "event_log_digest" {
        String::new()
    } else {
        format!(",\"operations_per_second\":{:.1}", n as f64 / seconds)
    };
    println!(
        "{{\"benchmark\":\"{name}\",\"operations\":{n},\"median_ns\":{:.1},\"median_ms\":{:.6},\"iterations_per_second\":{iterations_per_second:.1}{throughput}}}",
        seconds * 1_000_000_000.0,
        seconds * 1_000.0,
    );
}

fn bench_append_batch(n: usize) {
    let batch = events(n);
    let elapsed = measure(
        || {
            let mut log = EventLog::new();
            black_box(
                log.append_batch(black_box(&batch))
                    .expect("valid benchmark batch"),
            );
            black_box(log);
        },
        REPEATS,
    );
    report("append_batch", n, elapsed);
}

fn bench_digest(n: usize) {
    let mut log = EventLog::new();
    log.append_batch(&events(n)).expect("valid benchmark batch");
    let elapsed = measure(
        || {
            black_box(log.digest());
        },
        REPEATS * 4,
    );
    report("event_log_digest", n, elapsed);
}

fn bench_segmented_event_log_append(n: usize) {
    static NEXT_ROOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut warm_samples = Vec::with_capacity(REPEATS);
    let mut cold_samples = Vec::with_capacity(REPEATS);
    for _ in 0..REPEATS {
        let id = NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let root = std::env::current_dir()
            .expect("read benchmark working directory")
            .join("target")
            .join(format!(
                ".codex-tmp-segmented-{stamp}-{id}-{}",
                std::process::id()
            ));
        std::fs::create_dir(&root).expect("create segmented benchmark directory");
        let mut base = EventLog::new();
        base.append_batch(&events(n)).expect("build valid history");
        let warm_store = SegmentedEventLogStore::new(root.join("warm"), 1_024)
            .expect("create warm segmented store");
        warm_store.write("bench", &base).expect("seed warm store");
        let mut warm_extended = base.clone();
        warm_extended
            .append_batch(
                &events(1)
                    .into_iter()
                    .map(|mut event| {
                        event.seq = n as u64;
                        event.ts = n as u64 + 1;
                        event
                    })
                    .collect::<Vec<_>>(),
            )
            .expect("append warm event");
        let start = Instant::now();
        black_box(
            warm_store
                .write("bench", &warm_extended)
                .expect("warm append"),
        );
        warm_samples.push(start.elapsed());

        let cold_store_path = root.join("cold");
        let seed_store =
            SegmentedEventLogStore::new(&cold_store_path, 1_024).expect("create cold seed store");
        seed_store.write("bench", &base).expect("seed cold store");
        drop(seed_store);
        let cold_store =
            SegmentedEventLogStore::new(&cold_store_path, 1_024).expect("reopen cold store");
        let start = Instant::now();
        black_box(
            cold_store
                .write("bench", &warm_extended)
                .expect("cold append"),
        );
        cold_samples.push(start.elapsed());
        std::fs::remove_dir_all(root).expect("remove segmented benchmark directory");
    }
    let warm = median(warm_samples);
    let cold = median(cold_samples);
    println!(
        "{{\"benchmark\":\"segmented_event_log_append\",\"history_events\":{n},\"appended_events\":1,\"warm_store_median_ms\":{:.6},\"cold_store_median_ms\":{:.6}}}",
        warm.as_secs_f64() * 1_000.0,
        cold.as_secs_f64() * 1_000.0,
    );
}

fn bench_full_replay(n: usize) {
    let batch = events(n);
    let elapsed = measure(
        || {
            let (log, ledger) = ReplayVerifier::replay(black_box(&batch)).expect("valid replay");
            black_box((log.digest(), ledger.entries().len()));
        },
        REPEATS,
    );
    report("full_replay", n, elapsed);
}

#[cfg(feature = "sqlite")]
fn bench_full_vs_incremental_replay() {
    const EVENT_COUNT: usize = 1_000;
    const CHUNK: usize = 100;
    let root = std::env::current_dir()
        .expect("read benchmark working directory")
        .join(format!(".codex-tmp-bench-{}", std::process::id()));
    std::fs::create_dir(&root).expect("create benchmark directory");
    let database = root.join("events.sqlite");
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").expect("valid instrument");
    let mut writer =
        LiveEventPipeline::open_sqlite(&database, "bench", "USDT").expect("open benchmark writer");
    let mut reader = LiveEventPipeline::open_sqlite_read_only(&database, "bench", "USDT")
        .expect("open benchmark reader");
    let mut incremental_samples = Vec::new();
    for chunk_start in (0..EVENT_COUNT).step_by(CHUNK) {
        for index in chunk_start..(chunk_start + CHUNK).min(EVENT_COUNT) {
            let ts = index as u64 + 1;
            let quote = QuoteTick::new(
                ts,
                Price::from_i64(99),
                Quantity::from_i64(10),
                Price::from_i64(100),
                Quantity::from_i64(10),
                ts,
            );
            writer
                .ingest(RuntimeEventEnvelope::market_quote(
                    instrument.clone(),
                    quote,
                    ts,
                    ts,
                    format!("bench-{ts}"),
                ))
                .expect("append benchmark quote");
        }
        let start = Instant::now();
        reader.refresh().expect("incremental refresh");
        incremental_samples.push(start.elapsed());
    }
    let incremental_digest = reader.log().digest();
    let incremental_median = median(incremental_samples);
    drop(reader);
    drop(writer);

    let full_replay = measure(
        || {
            let full = LiveEventPipeline::open_sqlite_read_only(&database, "bench", "USDT")
                .expect("full replay");
            assert_eq!(full.log().digest(), incremental_digest);
            assert_eq!(full.log().len(), EVENT_COUNT);
            black_box(full);
        },
        REPEATS,
    );
    println!(
        "{{\"benchmark\":\"full_vs_incremental_replay\",\"events\":{EVENT_COUNT},\"chunks\":{},\"incremental_refresh_p50_ms\":{:.3},\"full_replay_median_ms\":{:.3},\"digests_equal\":true}}",
        EVENT_COUNT / CHUNK,
        incremental_median.as_secs_f64() * 1_000.0,
        full_replay.as_secs_f64() * 1_000.0,
    );
    let _ = std::fs::remove_file(&database);
    let _ = std::fs::remove_file(root.join("events.sqlite-wal"));
    let _ = std::fs::remove_file(root.join("events.sqlite-shm"));
    let _ = std::fs::remove_dir(root);
}

fn bench_l2_matching(n: usize) {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").expect("valid instrument");
    let elapsed = measure(
        || {
            let mut matcher = OrderBookMatchingEngine::new(0).expect("valid fee model");
            for index in 0..n {
                let id = index as u64 + 1;
                matcher
                    .submit(Order {
                        client_id: id,
                        instrument: instrument.clone(),
                        side: Side::Buy,
                        qty: Quantity::from_i64(1),
                        limit: None,
                        status: OrderStatus::PendingSubmit,
                        filled: Quantity::ZERO,
                        account_id: "bench".into(),
                        trace: None,
                        policy: None,
                    })
                    .expect("valid benchmark order");
                let snapshot = OrderBookSnapshot {
                    instrument: instrument.clone(),
                    ts: id,
                    sequence: id,
                    bids: vec![BookLevel {
                        price: Price::from_i64(99),
                        qty: Quantity::from_i64(1),
                    }],
                    asks: vec![BookLevel {
                        price: Price::from_i64(100),
                        qty: Quantity::from_i64(1),
                    }],
                };
                black_box(
                    matcher
                        .on_snapshot(black_box(&snapshot))
                        .expect("valid snapshot"),
                );
            }
        },
        REPEATS,
    );
    report("l2_matching", n, elapsed);
}

#[cfg(feature = "sqlite")]
struct TickFixture {
    root: PathBuf,
    database: PathBuf,
}

#[cfg(feature = "sqlite")]
impl TickFixture {
    fn new(size: usize) -> Self {
        let root = std::env::current_dir()
            .expect("read benchmark working directory")
            .join("target")
            .join(format!(
                ".codex-tmp-steady-tick-{}-{size}",
                std::process::id()
            ));
        std::fs::create_dir(&root).expect("create steady tick fixture directory");
        let database = root.join("events.sqlite");
        let store = SqliteEventLogStore::new(&database).expect("open fixture store");
        let mut log = EventLog::new();
        log.append_batch(&events(size))
            .expect("build valid steady tick history");
        store
            .write("steady-tick", &log)
            .expect("persist steady tick history");
        Self { root, database }
    }
}

#[cfg(feature = "sqlite")]
impl Drop for TickFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(feature = "sqlite")]
fn percentile(mut samples: Vec<Duration>, fraction: f64) -> Duration {
    samples.sort_unstable();
    let index = ((samples.len() as f64 * fraction).ceil() as usize)
        .saturating_sub(1)
        .min(samples.len().saturating_sub(1));
    samples[index]
}

#[cfg(feature = "sqlite")]
fn bench_steady_state_tick_cost(n: usize) {
    const TICKS: usize = 21;
    const WINDOWS: usize = 5;
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").expect("valid instrument");
    let mut samples = Vec::with_capacity(TICKS * WINDOWS);
    let mut final_digest = 0;
    for _window in 0..WINDOWS {
        let fixture = TickFixture::new(n);
        let mut pipeline = LiveEventPipeline::open_sqlite(&fixture.database, "steady-tick", "USDT")
            .expect("open seeded live pipeline");
        assert_eq!(pipeline.log().len(), n);
        for tick in 0..TICKS {
            let ts = (n + tick + 1) as u64;
            let quote = QuoteTick::new(
                ts,
                Price::from_i64(99),
                Quantity::from_i64(10),
                Price::from_i64(100),
                Quantity::from_i64(10),
                ts,
            );
            let envelope = RuntimeEventEnvelope::market_quote(
                instrument.clone(),
                quote,
                ts,
                ts,
                format!("steady-tick-{n}-{tick}"),
            );
            let started = Instant::now();
            black_box(
                pipeline
                    .ingest(black_box(envelope))
                    .expect("ingest steady tick"),
            );
            samples.push(started.elapsed());
        }
        assert_eq!(pipeline.log().len(), n + TICKS);
        final_digest = pipeline.log().digest();
        drop(pipeline);
        drop(fixture);
    }
    let p50 = percentile(samples.clone(), 0.50);
    let p95 = percentile(samples, 0.95);
    let seconds = p50.as_secs_f64();
    let ticks_per_second = if seconds > 0.0 { 1.0 / seconds } else { 0.0 };
    println!(
        "{{\"benchmark\":\"steady_state_pipeline_ingest\",\"operations\":{n},\"windows\":{WINDOWS},\"ticks_per_window\":{TICKS},\"ticks\":{},\"median_ms\":{:.6},\"p95_ms\":{:.6},\"operations_per_second\":{ticks_per_second:.3},\"event_log_digest\":\"{:016x}\"}}",
        TICKS * WINDOWS,
        p50.as_secs_f64() * 1_000.0,
        p95.as_secs_f64() * 1_000.0,
        final_digest,
    );
}

fn main() {
    #[cfg(feature = "sqlite")]
    bench_full_vs_incremental_replay();
    for n in SIZES {
        #[cfg(feature = "sqlite")]
        bench_steady_state_tick_cost(n);
        bench_append_batch(n);
        bench_digest(n);
        bench_segmented_event_log_append(n);
        bench_full_replay(n);
        bench_l2_matching(n);
    }
}
