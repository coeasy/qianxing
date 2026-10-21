use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PipelineMetricsSnapshot {
    pub ingest_attempts: u64,
    pub ingested_events: u64,
    pub deduplicated_events: u64,
    pub transient_retries: u64,
    pub refreshes: u64,
    pub failures: u64,
    /// Total wall time spent inside `ingest`, in nanoseconds.
    #[serde(default)]
    pub ingest_duration_ns_total: u64,
    /// Longest observed `ingest` call, in nanoseconds.
    #[serde(default)]
    pub ingest_duration_ns_max: u64,
}

pub const PIPELINE_INGEST_WARNING_BUDGET_NS: u64 = 100_000_000;

#[derive(Default)]
pub(super) struct PipelineMetrics {
    pub(super) ingest_attempts: AtomicU64,
    pub(super) ingested_events: AtomicU64,
    pub(super) deduplicated_events: AtomicU64,
    pub(super) transient_retries: AtomicU64,
    pub(super) refreshes: AtomicU64,
    pub(super) failures: AtomicU64,
    ingest_duration_ns_total: AtomicU64,
    ingest_duration_ns_max: AtomicU64,
    // Regression signals distinguish cursor-tail append from full projection rebuild.
    pub(super) tail_appends: AtomicU64,
    pub(super) rebuilds: AtomicU64,
}

impl PipelineMetrics {
    pub(super) fn record_ingest_duration(&self, elapsed_ns: u64) {
        let mut total = self.ingest_duration_ns_total.load(Ordering::Relaxed);
        loop {
            let next = total.saturating_add(elapsed_ns);
            match self.ingest_duration_ns_total.compare_exchange_weak(
                total,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => total = observed,
            }
        }
        self.ingest_duration_ns_max
            .fetch_max(elapsed_ns, Ordering::Relaxed);
    }

    pub(super) fn snapshot(&self) -> PipelineMetricsSnapshot {
        PipelineMetricsSnapshot {
            ingest_attempts: self.ingest_attempts.load(Ordering::Relaxed),
            ingested_events: self.ingested_events.load(Ordering::Relaxed),
            deduplicated_events: self.deduplicated_events.load(Ordering::Relaxed),
            transient_retries: self.transient_retries.load(Ordering::Relaxed),
            refreshes: self.refreshes.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            ingest_duration_ns_total: self.ingest_duration_ns_total.load(Ordering::Relaxed),
            ingest_duration_ns_max: self.ingest_duration_ns_max.load(Ordering::Relaxed),
        }
    }
}
