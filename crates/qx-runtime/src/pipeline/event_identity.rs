//! Event identity indexes used by the ingest idempotency gate.
//!
//! `LiveEventPipeline` retains the full EventLog for replay, so searching it from the head for
//! every new event makes steady-state ingestion grow linearly with log length. These indexes are
//! derived from that log, updated on append/tail refresh, and rebuilt from the log on open.

use qx_core::Event;
use std::collections::BTreeMap;

#[derive(Clone, Default)]
pub(super) struct RuntimeEventIdentityIndex {
    by_dedup_key: BTreeMap<String, (u64, u64)>,
    by_correlation_source: BTreeMap<String, BTreeMap<u64, (u64, u64)>>,
    by_correlation: BTreeMap<String, (u64, u64)>,
}

impl RuntimeEventIdentityIndex {
    pub(super) fn remember(&mut self, event: &Event) {
        let identity = (event.seq, event.ts);
        if !event.metadata.dedup_key.is_empty() {
            self.by_dedup_key
                .entry(event.metadata.dedup_key.clone())
                .or_insert(identity);
        }
        self.by_correlation_source
            .entry(event.correlation_id.clone())
            .or_default()
            .entry(event.source_seq)
            .or_insert(identity);
        self.by_correlation
            .entry(event.correlation_id.clone())
            .or_insert(identity);
    }

    /// Return the timestamp of the first log event satisfying the same disjunction as the legacy
    /// EventLog scan. Choosing the smallest sequence preserves `.iter().find(...)` semantics when
    /// a dedup key and correlation identity point at different earlier events.
    pub(super) fn duplicate_ts(
        &self,
        dedup_key: &str,
        correlation_id: &str,
        source_seq: u64,
        semantic_replay: bool,
    ) -> Option<u64> {
        let by_dedup_key = (!dedup_key.is_empty())
            .then(|| self.by_dedup_key.get(dedup_key).copied())
            .flatten();
        let by_correlation_source = self
            .by_correlation_source
            .get(correlation_id)
            .and_then(|sources| sources.get(&source_seq))
            .copied();
        let by_correlation = semantic_replay
            .then(|| self.by_correlation.get(correlation_id).copied())
            .flatten();

        [by_dedup_key, by_correlation_source, by_correlation]
            .into_iter()
            .flatten()
            .min_by_key(|(seq, _)| *seq)
            .map(|(_, ts)| ts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{Event, EventKind, EventMetadata, Priority};

    fn event(seq: u64, ts: u64, source_seq: u64, correlation: &str, dedup_key: &str) -> Event {
        Event::new(seq, ts, Priority::MARKET, EventKind::Settle)
            .sourced_by(source_seq)
            .correlated(correlation)
            .with_metadata(EventMetadata {
                dedup_key: dedup_key.into(),
                ..EventMetadata::default()
            })
    }

    #[test]
    fn duplicate_lookup_keeps_first_match_across_all_identity_paths() {
        let mut index = RuntimeEventIdentityIndex::default();
        index.remember(&event(2, 200, 20, "correlation-a", "dedup-a"));
        index.remember(&event(3, 300, 40, "correlation-b", "dedup-x"));
        index.remember(&event(4, 400, 50, "correlation-b", "dedup-b"));

        assert_eq!(
            index.duplicate_ts("dedup-b", "correlation-b", 40, false),
            Some(300),
            "a dedup-key hit and correlation/source hit must select the earliest sequence"
        );
        assert_eq!(
            index.duplicate_ts("", "correlation-b", 99, true),
            Some(300),
            "semantic replay matches the first event for its correlation"
        );
        assert_eq!(
            index.duplicate_ts("", "correlation-a", 99, false),
            None,
            "a different source sequence is not a duplicate outside semantic replay"
        );
        assert_eq!(
            index.duplicate_ts("", "correlation-b", 50, false),
            Some(400),
            "same correlation and source sequence remains an idempotency hit"
        );
    }
}
