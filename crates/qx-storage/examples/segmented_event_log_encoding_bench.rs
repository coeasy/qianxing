use qx_core::{Event, EventKind, Priority};
use serde_json::json;
use std::hint::black_box;
use std::time::{Duration, Instant};

const SIZES: [(usize, usize); 3] = [(1, 20_000), (64, 312), (1_024, 20)];
const REPEATS: usize = 9;

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

fn legacy_encode(events: &[Event]) -> String {
    events
        .iter()
        .map(|event| format!("{}\n", serde_json::to_string(event).unwrap()))
        .collect()
}

fn buffered_encode(events: &[Event]) -> String {
    let mut bytes = Vec::new();
    for event in events {
        serde_json::to_writer(&mut bytes, event).unwrap();
        bytes.push(b'\n');
    }
    String::from_utf8(bytes).unwrap()
}

fn measure_ns_per_operation(
    events: &[Event],
    iterations: usize,
    encode: fn(&[Event]) -> String,
) -> Duration {
    let start = Instant::now();
    for _ in 0..iterations {
        black_box(encode(black_box(events)));
    }
    start.elapsed() / iterations as u32
}

fn median(values: &mut [u128]) -> u128 {
    values.sort_unstable();
    values[values.len() / 2]
}

fn main() {
    for (size, iterations) in SIZES {
        let input = events(size);
        let legacy_bytes = legacy_encode(&input);
        let buffered_bytes = buffered_encode(&input);
        assert_eq!(legacy_bytes.as_bytes(), buffered_bytes.as_bytes());

        let mut legacy_samples = Vec::with_capacity(REPEATS);
        let mut buffered_samples = Vec::with_capacity(REPEATS);
        for repeat in 0..REPEATS {
            // Alternate order to reduce systematic warm-up and frequency bias.
            if repeat % 2 == 0 {
                legacy_samples
                    .push(measure_ns_per_operation(&input, iterations, legacy_encode).as_nanos());
                buffered_samples
                    .push(measure_ns_per_operation(&input, iterations, buffered_encode).as_nanos());
            } else {
                buffered_samples
                    .push(measure_ns_per_operation(&input, iterations, buffered_encode).as_nanos());
                legacy_samples
                    .push(measure_ns_per_operation(&input, iterations, legacy_encode).as_nanos());
            }
        }
        let legacy_median = median(&mut legacy_samples);
        let buffered_median = median(&mut buffered_samples);
        println!(
            "{}",
            json!({
                "benchmark": "segmented_event_log_encoding",
                "events_per_segment": size,
                "iterations_per_sample": iterations,
                "repeats": REPEATS,
                "encoded_bytes": legacy_bytes.len(),
                "byte_equal": true,
                "legacy_median_ns_per_segment": legacy_median,
                "buffered_median_ns_per_segment": buffered_median,
                "speedup_ratio": legacy_median as f64 / buffered_median.max(1) as f64,
            })
        );
    }
}
