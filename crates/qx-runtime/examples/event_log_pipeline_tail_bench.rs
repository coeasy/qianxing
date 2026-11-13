use qx_core::{Event, EventKind, EventLog, InstrumentId, Price, Priority, Quantity, VenueId};
use qx_runtime::{LiveEventPipeline, RuntimeEventEnvelope, RuntimeExternalEvent};
use qx_storage::{EventLogFileStore, SegmentedEventLogStore};
use serde_json::json;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SIZES: [usize; 3] = [1_000, 8_000, 64_000];
const REPEATS: usize = 7;
const EVENTS_PER_SEGMENT: usize = 1_024;

fn fixture_events(size: usize) -> EventLog {
    let mut log = EventLog::new();
    let events = (0..size).map(|seq| {
        Event::new(
            seq as u64,
            seq as u64 + 1,
            Priority::POST,
            EventKind::Settle,
        )
    });
    log.append_batch(&events.collect::<Vec<_>>())
        .expect("build EventLog history");
    log
}

fn fixture_root(size: usize, repeat: usize, segmented: bool) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    std::env::current_dir()
        .expect("read benchmark working directory")
        .join("target")
        .join(format!(
            ".pipeline-tail-bench-{nonce}-{size}-{repeat}-{}-{}",
            std::process::id(),
            if segmented { "segmented" } else { "flat" }
        ))
}

fn sample(root: &Path, size: usize, segmented: bool) -> Duration {
    std::fs::create_dir_all(root).expect("create benchmark root");
    let log = fixture_events(size);
    let flat_store = EventLogFileStore::new(root);
    let segmented_store =
        SegmentedEventLogStore::new(root, EVENTS_PER_SEGMENT).expect("create segmented store");
    if segmented {
        segmented_store
            .write("bench", &log)
            .expect("seed segmented EventLog");
    } else {
        flat_store.write("bench", &log).expect("seed flat EventLog");
    }

    let mut reader = LiveEventPipeline::open_read_only(
        root,
        "bench",
        "USDT",
        segmented.then_some(EVENTS_PER_SEGMENT),
    )
    .expect("open pipeline reader");
    let mut writer = LiveEventPipeline::open_read_only(
        root,
        "bench",
        "USDT",
        segmented.then_some(EVENTS_PER_SEGMENT),
    )
    .expect("open pipeline writer");
    let event_ts = size as u64 + 1;
    let envelope = RuntimeEventEnvelope::venue(
        RuntimeExternalEvent::MarketQuote {
            instrument: InstrumentId::new("BTCUSDT", VenueId::new("BINANCE"))
                .expect("valid benchmark instrument"),
            bid: Price::from_i64(99),
            ask: Price::from_i64(100),
            bid_qty: Quantity::from_i64(2),
            ask_qty: Quantity::from_i64(3),
            bid_qty_exact: None,
            ask_qty_exact: None,
            bid_exact: None,
            ask_exact: None,
        },
        event_ts,
        event_ts,
        1,
        format!("bench-market-quote-{event_ts}"),
    );

    let start = Instant::now();
    writer.ingest(envelope).expect("ingest market quote");
    reader
        .refresh()
        .expect("refresh pipeline from external append");
    let elapsed = start.elapsed();
    assert_eq!(reader.log().digest(), writer.log().digest());
    black_box(reader.snapshot());
    elapsed
}

fn median_ms(samples: &mut [f64]) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

fn main() {
    for size in SIZES {
        let mut flat_ms = Vec::with_capacity(REPEATS);
        let mut segmented_ms = Vec::with_capacity(REPEATS);
        for repeat in 0..REPEATS {
            let flat_root = fixture_root(size, repeat, false);
            flat_ms.push(sample(&flat_root, size, false).as_secs_f64() * 1_000.0);
            std::fs::remove_dir_all(flat_root).expect("remove flat fixture");

            let segmented_root = fixture_root(size, repeat, true);
            segmented_ms.push(sample(&segmented_root, size, true).as_secs_f64() * 1_000.0);
            std::fs::remove_dir_all(segmented_root).expect("remove segmented fixture");
        }
        let mut flat_median_input = flat_ms.clone();
        let mut segmented_median_input = segmented_ms.clone();
        println!(
            "{}",
            json!({
                "benchmark": "event_log_pipeline_ingest_refresh",
                "history_events": size,
                "repeats": REPEATS,
                "events_per_segment": EVENTS_PER_SEGMENT,
                "flat_file_ms": flat_ms,
                "segmented_ms": segmented_ms,
                "flat_file_median_ms": median_ms(&mut flat_median_input),
                "segmented_median_ms": median_ms(&mut segmented_median_input),
            })
        );
    }
}
