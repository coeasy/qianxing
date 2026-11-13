use qx_core::{Event, EventKind, EventLog, Priority};
use qx_storage::{EventLogFileStore, SegmentedEventLogStore};
use serde_json::json;
use std::hint::black_box;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SIZES: [usize; 3] = [1_000, 8_000, 64_000];
const REPEATS: usize = 7;
const EVENTS_PER_SEGMENT: usize = 1_024;

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

fn median_ms(samples: &mut [f64]) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

fn append_file_once(root: &std::path::Path, size: usize) -> Duration {
    std::fs::create_dir_all(root).expect("create benchmark root");
    let mut log = EventLog::new();
    log.append_batch(&events(size))
        .expect("build event history");
    let store = EventLogFileStore::new(root.join("store"));
    store.write("bench", &log).expect("seed flat store");

    let mut extended = log.clone();
    extended
        .append_batch(&[Event::new(
            size as u64,
            size as u64 + 1,
            Priority::POST,
            EventKind::Settle,
        )])
        .expect("append one event to in-memory log");

    let start = Instant::now();
    black_box(
        store
            .write("bench", &extended)
            .expect("persist appended event"),
    );
    start.elapsed()
}

fn append_segmented_once(root: &std::path::Path, size: usize, reopen_store: bool) -> Duration {
    std::fs::create_dir_all(root).expect("create benchmark root");
    let mut log = EventLog::new();
    log.append_batch(&events(size))
        .expect("build event history");
    let store = SegmentedEventLogStore::new(root.join("store"), EVENTS_PER_SEGMENT)
        .expect("create segmented store");
    store.write("bench", &log).expect("seed segmented store");

    let mut extended = log.clone();
    extended
        .append_batch(&[Event::new(
            size as u64,
            size as u64 + 1,
            Priority::POST,
            EventKind::Settle,
        )])
        .expect("append one event to in-memory log");

    let active_store = if reopen_store {
        drop(store);
        SegmentedEventLogStore::new(root.join("store"), EVENTS_PER_SEGMENT)
            .expect("reopen segmented store")
    } else {
        store
    };
    let start = Instant::now();
    black_box(
        active_store
            .write("bench", &extended)
            .expect("persist appended event"),
    );
    start.elapsed()
}

fn main() {
    for size in SIZES {
        let mut flat_samples_ms = Vec::with_capacity(REPEATS);
        let mut segmented_warm_samples_ms = Vec::with_capacity(REPEATS);
        let mut segmented_reopened_samples_ms = Vec::with_capacity(REPEATS);
        let mut manifest_bytes = 0;
        for repeat in 0..REPEATS {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos();
            let base = std::env::current_dir()
                .expect("read benchmark working directory")
                .join("target")
                .join(format!(
                    ".segmented-event-log-bench-{nonce}-{size}-{repeat}-{}",
                    std::process::id()
                ));
            let flat_root = base.join("flat");
            let flat = append_file_once(&flat_root, size);
            flat_samples_ms.push(flat.as_secs_f64() * 1_000.0);

            let warm_root = base.join("segmented-warm");
            let warm = append_segmented_once(&warm_root, size, false);
            manifest_bytes = std::fs::metadata(warm_root.join("store/bench.manifest.json"))
                .expect("read manifest metadata")
                .len();
            segmented_warm_samples_ms.push(warm.as_secs_f64() * 1_000.0);

            let reopened_root = base.join("segmented-reopened");
            let reopened = append_segmented_once(&reopened_root, size, true);
            segmented_reopened_samples_ms.push(reopened.as_secs_f64() * 1_000.0);
            std::fs::remove_dir_all(base).expect("remove benchmark fixture");
        }
        println!(
            "{}",
            json!({
                "benchmark": "event_log_file_backend_append_comparison",
                "history_events": size,
                "appended_events": 1,
                "repeats": REPEATS,
                "events_per_segment": EVENTS_PER_SEGMENT,
                "flat_file_median_ms": median_ms(&mut flat_samples_ms),
                "segmented_same_instance_warm_median_ms": median_ms(&mut segmented_warm_samples_ms),
                "segmented_reopened_store_median_ms": median_ms(&mut segmented_reopened_samples_ms),
                "manifest_bytes": manifest_bytes,
            })
        );
    }
}
