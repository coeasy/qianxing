/// Convert wall-clock milliseconds to the seconds domain used by leases and job timestamps.
pub fn lease_clock(timestamp_ms: u64) -> u64 {
    timestamp_ms / 1_000
}
