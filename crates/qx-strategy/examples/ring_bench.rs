//! QXRB SPSC mmap ring micro-benchmark.
//!
//! This measures only the transport layer (encode-free payload push/pop) in
//! two mappings of the same file. It is not a strategy, exchange, or full
//! end-to-end latency claim.

use qx_strategy::{SharedRingConfig, SharedRingReader, SharedRingWriter};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

fn percentile(sorted: &[u128], numerator: usize, denominator: usize) -> u128 {
    let index = ((sorted.len() - 1) * numerator) / denominator;
    sorted[index]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let iterations = std::env::args()
        .nth(1)
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(100_000);
    if iterations == 0 {
        return Err("iterations must be positive".into());
    }
    let path: PathBuf = std::env::temp_dir().join(format!(
        "qianxing-ring-bench-{}-{}.bin",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let config = SharedRingConfig {
        capacity: 1024,
        slot_bytes: 4096,
    };
    let mut writer = SharedRingWriter::create(&path, config)?;
    let mut reader = SharedRingReader::open(&path, config)?;
    let payload = vec![0x5a; 512];
    let mut samples = Vec::with_capacity(iterations);
    let started = Instant::now();
    for _ in 0..iterations {
        let begin = Instant::now();
        writer.try_push(&payload)?;
        let received = reader.try_pop()?;
        if received != payload {
            return Err("ring payload mismatch".into());
        }
        samples.push(begin.elapsed().as_nanos());
    }
    let elapsed = started.elapsed();
    samples.sort_unstable();
    let seconds = elapsed.as_secs_f64();
    println!(
        "qxrb_ring_bench iterations={} payload_bytes={} p50_ns={} p95_ns={} p99_ns={} p999_ns={} throughput_msg_s={:.0}",
        iterations,
        payload.len(),
        percentile(&samples, 50, 100),
        percentile(&samples, 95, 100),
        percentile(&samples, 99, 100),
        percentile(&samples, 999, 1000),
        iterations as f64 / seconds.max(f64::MIN_POSITIVE)
    );
    drop(reader);
    drop(writer);
    fs::remove_file(path)?;
    Ok(())
}
