//! Isolated allocator counter for the shared qx-app Bar backtest use case.
//!
//! This example is intentionally not linked into the production CLI. It measures allocator
//! requests made by one application-layer backtest after request parsing and before report
//! serialization, alongside an independently measured process RSS in the Python driver.

use qx_app::{run_backtest, BacktestSpec, CallerCapability, RunContext};
use serde_json::json;
use std::alloc::{GlobalAlloc, Layout, System};
use std::env;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct CountingAllocator;

static ALLOCATION_CALLS: AtomicU64 = AtomicU64::new(0);
static DEALLOCATION_CALLS: AtomicU64 = AtomicU64::new(0);
static REQUESTED_BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE_BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK_LIVE_BYTES: AtomicU64 = AtomicU64::new(0);

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn update_peak(value: u64) {
    let mut observed = PEAK_LIVE_BYTES.load(Ordering::Relaxed);
    while value > observed {
        match PEAK_LIVE_BYTES.compare_exchange_weak(
            observed,
            value,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(current) => observed = current,
        }
    }
}

fn record_allocation(bytes: usize) {
    let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
    ALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
    REQUESTED_BYTES.fetch_add(bytes, Ordering::Relaxed);
    let live = LIVE_BYTES
        .fetch_add(bytes, Ordering::Relaxed)
        .saturating_add(bytes);
    update_peak(live);
}

fn reset_measurement() -> u64 {
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    ALLOCATION_CALLS.store(0, Ordering::Relaxed);
    DEALLOCATION_CALLS.store(0, Ordering::Relaxed);
    REQUESTED_BYTES.store(0, Ordering::Relaxed);
    PEAK_LIVE_BYTES.store(baseline, Ordering::Relaxed);
    baseline
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: this allocator forwards the exact layout to the system allocator.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record_allocation(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: this allocator forwards the exact layout to the system allocator.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            record_allocation(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        DEALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
        let bytes = u64::try_from(layout.size()).unwrap_or(u64::MAX);
        LIVE_BYTES.fetch_sub(bytes, Ordering::Relaxed);
        // SAFETY: `pointer` was allocated by this allocator with the same layout.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `pointer` was allocated by this allocator and the old layout is unchanged.
        let resized = unsafe { System.realloc(pointer, layout, new_size) };
        if !resized.is_null() {
            let old_bytes = u64::try_from(layout.size()).unwrap_or(u64::MAX);
            let new_bytes = u64::try_from(new_size).unwrap_or(u64::MAX);
            ALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
            REQUESTED_BYTES.fetch_add(new_bytes, Ordering::Relaxed);
            if new_bytes >= old_bytes {
                let delta = new_bytes - old_bytes;
                let live = LIVE_BYTES
                    .fetch_add(delta, Ordering::Relaxed)
                    .saturating_add(delta);
                update_peak(live);
            } else {
                LIVE_BYTES.fetch_sub(old_bytes - new_bytes, Ordering::Relaxed);
            }
        }
        resized
    }
}

fn next_argument(args: &mut impl Iterator<Item = String>, label: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("missing {label}"))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let spec_path = next_argument(&mut args, "spec.json")?;
    let run_id = next_argument(&mut args, "run_id")?;
    let output_dir = next_argument(&mut args, "output_dir")?;
    if args.next().is_some() {
        return Err("usage: measure_allocations <spec.json> <run_id> <output_dir>".into());
    }

    let spec_json = std::fs::read_to_string(&spec_path)?;
    let mut spec = BacktestSpec::from_json(&spec_json)?;
    spec.run_id = run_id;
    spec.output_dir = output_dir;
    let context = RunContext::new(CallerCapability::Research, spec.run_id.clone());

    let baseline_live_bytes = reset_measurement();
    let started = Instant::now();
    let outcome = run_backtest(&spec, &context)?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;

    // Snapshot before constructing/serializing the JSON response.
    let allocation_calls = ALLOCATION_CALLS.load(Ordering::Relaxed);
    let deallocation_calls = DEALLOCATION_CALLS.load(Ordering::Relaxed);
    let requested_bytes = REQUESTED_BYTES.load(Ordering::Relaxed);
    let peak_live_bytes = PEAK_LIVE_BYTES.load(Ordering::Relaxed);
    let measured = json!({
        "schema_version": 1,
        "measurement_kind": "qx_app_bar_backtest_allocator_requests",
        "scope": "run_backtest application use case; spec parsing and output serialization excluded",
        "run_id": outcome.run_id,
        "instrument": outcome.instrument,
        "data_fingerprint": outcome.data_fingerprint,
        "result_hash": outcome.result_hash,
        "fills": outcome.fills,
        "allocation_calls": allocation_calls,
        "deallocation_calls": deallocation_calls,
        "requested_bytes_total": requested_bytes,
        "peak_live_requested_bytes_delta": peak_live_bytes.saturating_sub(baseline_live_bytes),
        "live_requested_bytes_delta_at_return": LIVE_BYTES.load(Ordering::Relaxed).saturating_sub(baseline_live_bytes),
        "wall_ms": elapsed_ms,
        "allocator": "std::alloc::System wrapped by a counting GlobalAlloc",
    });
    serde_json::to_writer(io::stdout().lock(), &measured)?;
    println!();
    Ok(())
}
