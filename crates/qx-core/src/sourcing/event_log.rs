use super::Fnv1a;
use crate::{Event, QxResult};
use std::collections::BTreeSet;
use std::sync::Mutex;

#[derive(Clone, Copy)]
struct EventLogPrefixSummary {
    len: usize,
    digest: Option<u64>,
    event_digest: u64,
    next_seq: u64,
}

/// Append-only event facts with cached validation state for repeated read-side checks.
pub struct EventLog {
    events: Vec<Event>,
    /// Sequence index used by reducers and to detect raw-append duplicates.
    seqs: BTreeSet<u64>,
    next_seq: u64,
    max_event_seq: Option<u64>,
    events_well_formed: bool,
    /// Rolling event-only digest for versioned append-only storage manifests.
    event_digest: u64,
    /// A small bounded cache for immutable prefix digests reused by append-only stores.
    digest_cache: Mutex<Vec<EventLogPrefixSummary>>,
}

impl Clone for EventLog {
    fn clone(&self) -> Self {
        let digest_cache = self
            .digest_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Self {
            events: self.events.clone(),
            seqs: self.seqs.clone(),
            next_seq: self.next_seq,
            max_event_seq: self.max_event_seq,
            events_well_formed: self.events_well_formed,
            event_digest: self.event_digest,
            digest_cache: Mutex::new(digest_cache),
        }
    }
}

impl Default for EventLog {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            seqs: BTreeSet::new(),
            next_seq: 0,
            max_event_seq: None,
            events_well_formed: true,
            event_digest: Fnv1a::new().finish(),
            digest_cache: Mutex::new(Vec::new()),
        }
    }
}

impl EventLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    pub fn alloc_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    pub fn append(&mut self, event: Event) {
        self.cache_current_prefix();
        let ordered = self.events.last().is_none_or(|previous| {
            (event.ts, event.prio, event.seq) > (previous.ts, previous.prio, previous.seq)
        });
        let metadata_valid = event.metadata.validate().is_ok();
        let mut rolling = Fnv1a::from_state(self.event_digest);
        event.digest(&mut rolling);
        self.event_digest = rolling.finish();
        let unique = self.seqs.insert(event.seq);
        self.events_well_formed &=
            ordered && metadata_valid && unique && event.seq.checked_add(1).is_some();
        self.next_seq = self.next_seq.max(event.seq.saturating_add(1));
        self.max_event_seq = Some(
            self.max_event_seq
                .map_or(event.seq, |max| max.max(event.seq)),
        );
        self.events.push(event);
    }

    /// Checked append with all-or-nothing semantics.
    pub fn append_checked(&mut self, event: Event) -> QxResult<()> {
        let previous = self
            .events
            .last()
            .map(|previous| (previous.ts, previous.prio, previous.seq));
        let next_seq = self
            .validate_append_event(&event, previous)?
            .max(self.next_seq);
        let max_event_seq = Some(
            self.max_event_seq
                .map_or(event.seq, |max| max.max(event.seq)),
        );

        // The validation above is complete before any log state changes. A single owned append
        // can update the existing indexes directly, avoiding append_batch's temporary BTreeSet
        // and the second Event clone used when replay already owns its input.
        self.cache_current_prefix();
        self.seqs.insert(event.seq);
        self.next_seq = next_seq;
        self.max_event_seq = max_event_seq;
        let mut rolling = Fnv1a::from_state(self.event_digest);
        event.digest(&mut rolling);
        self.event_digest = rolling.finish();
        self.events.push(event);
        Ok(())
    }

    /// Shared per-event validation used by owned single appends and atomic batch staging.
    #[inline]
    fn validate_append_event(
        &self,
        event: &Event,
        previous: Option<(u64, u8, u64)>,
    ) -> QxResult<u64> {
        event
            .metadata
            .validate()
            .map_err(crate::error::QxError::Invariant)?;
        if self.seqs.contains(&event.seq) {
            return Err(crate::error::QxError::Invariant("事件 seq 重复".into()));
        }
        let key = (event.ts, event.prio, event.seq);
        if previous.is_some_and(|value| key <= value) {
            return Err(crate::error::QxError::Invariant(
                "事件追加违反时间/优先级/序号顺序".into(),
            ));
        }
        event
            .seq
            .checked_add(1)
            .ok_or_else(|| crate::error::QxError::Invariant("事件序号溢出".into()))
    }

    /// Validate the full batch in shadow state before changing this log.
    pub fn append_batch(&mut self, batch: &[Event]) -> QxResult<usize> {
        if batch.is_empty() {
            return Ok(0);
        }
        let mut staged_seqs = BTreeSet::new();
        let mut previous = self
            .events
            .last()
            .map(|event| (event.ts, event.prio, event.seq));
        let mut next_seq = self.next_seq;
        let mut max_event_seq = self.max_event_seq;
        let mut rolling = Fnv1a::from_state(self.event_digest);
        for event in batch {
            if !staged_seqs.insert(event.seq) {
                return Err(crate::error::QxError::Invariant("事件 seq 重复".into()));
            }
            let key = (event.ts, event.prio, event.seq);
            let event_next_seq = self.validate_append_event(event, previous)?;
            previous = Some(key);
            next_seq = next_seq.max(event_next_seq);
            max_event_seq = Some(max_event_seq.map_or(event.seq, |max| max.max(event.seq)));
            event.digest(&mut rolling);
        }
        self.cache_current_prefix();
        self.next_seq = next_seq;
        self.max_event_seq = max_event_seq;
        self.event_digest = rolling.finish();
        for event in batch {
            self.seqs.insert(event.seq);
            self.events.push(event.clone());
        }
        Ok(batch.len())
    }

    /// Use the append-maintained validity summary for normal logs; malformed input falls back
    /// to the full validator so callers retain the same precise rejection behavior.
    pub fn validate(&self) -> QxResult<()> {
        let next_seq_matches = match self.max_event_seq {
            Some(seq) => seq.checked_add(1) == Some(self.next_seq),
            None => self.next_seq == 0,
        };
        if self.events_well_formed && self.seqs.len() == self.events.len() && next_seq_matches {
            return Ok(());
        }
        if self.seqs.len() != self.events.len() {
            return Err(crate::error::QxError::Invariant("事件日志 seq 重复".into()));
        }
        let mut previous: Option<(u64, u8, u64)> = None;
        let mut max_seq = None;
        for event in &self.events {
            event
                .metadata
                .validate()
                .map_err(crate::error::QxError::Invariant)?;
            let key = (event.ts, event.prio, event.seq);
            if previous.is_some_and(|value| key <= value) {
                return Err(crate::error::QxError::Invariant(
                    "事件日志未按时间/优先级/序号排序".into(),
                ));
            }
            previous = Some(key);
            max_seq = Some(max_seq.map_or(event.seq, |max: u64| max.max(event.seq)));
        }
        let expected_next = match max_seq {
            Some(seq) => seq
                .checked_add(1)
                .ok_or_else(|| crate::error::QxError::Invariant("事件序号溢出".into()))?,
            None => 0,
        };
        if self.next_seq != expected_next {
            return Err(crate::error::QxError::Invariant(
                "事件日志 next_seq 与最大 seq 不一致".into(),
            ));
        }
        Ok(())
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Stable JSON payload; storage and object persistence stay at the boundary.
    pub fn to_json(&self) -> QxResult<String> {
        self.validate()?;
        serde_json::to_string(&self.events).map_err(|error| {
            crate::error::QxError::Invariant(format!("事件日志序列化失败: {error}"))
        })
    }

    pub fn from_json(input: &str) -> QxResult<Self> {
        let events: Vec<Event> = serde_json::from_str(input).map_err(|error| {
            crate::error::QxError::Permanent(format!("事件日志 JSON 无法解析: {error}"))
        })?;
        let mut log = Self::new();
        for event in events {
            log.append(event);
        }
        log.validate()?;
        Ok(log)
    }

    /// Stable digest of the complete event sequence.
    pub fn digest(&self) -> u64 {
        self.digest_of_prefix(self.events.len()).unwrap_or_else(|| {
            // The full length is always a valid prefix; retain a direct fallback if that invariant
            // ever regresses so the digest API stays total without changing its stable format.
            let mut hasher = Fnv1a::new();
            hasher.write_u64(self.events.len() as u64);
            for event in &self.events {
                event.digest(&mut hasher);
            }
            hasher.finish()
        })
    }

    /// Digest the first `len` events with the same format as [`Self::digest`].
    pub fn digest_of_prefix(&self, len: usize) -> Option<u64> {
        self.prefix_summary(len).and_then(|summary| summary.digest)
    }

    /// Stable rolling digest of event payloads without the legacy length preamble.
    /// This is used by segmented manifest v2 so appending can extend a verified prefix in O(new events).
    pub fn event_digest(&self) -> u64 {
        self.event_digest
    }

    /// Rolling event payload digest for the first `len` events.
    pub fn event_digest_of_prefix(&self, len: usize) -> Option<u64> {
        if len > self.events.len() {
            return None;
        }
        if let Some(summary) = self
            .digest_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|summary| summary.len == len)
            .copied()
        {
            return Some(summary.event_digest);
        }
        self.prefix_summary(len).map(|summary| summary.event_digest)
    }

    fn cache_current_prefix(&self) {
        const DIGEST_CACHE_CAPACITY: usize = 8;
        let mut cache = self
            .digest_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = cache
            .iter()
            .position(|summary| summary.len == self.events.len())
        {
            let entry = cache.remove(index);
            cache.push(entry);
            return;
        }
        let next_seq = self.max_event_seq.map_or(0, |seq| seq.saturating_add(1));
        cache.push(EventLogPrefixSummary {
            len: self.events.len(),
            digest: None,
            event_digest: self.event_digest,
            next_seq,
        });
        if cache.len() > DIGEST_CACHE_CAPACITY {
            cache.remove(0);
        }
    }

    /// Maximum sequence plus one for the first `len` events.
    pub fn next_seq_of_prefix(&self, len: usize) -> Option<u64> {
        if len > self.events.len() {
            return None;
        }
        if let Some(summary) = self
            .digest_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|summary| summary.len == len)
            .copied()
        {
            return Some(summary.next_seq);
        }
        self.prefix_summary(len).map(|summary| summary.next_seq)
    }

    fn prefix_summary(&self, len: usize) -> Option<EventLogPrefixSummary> {
        if len > self.events.len() {
            return None;
        }
        const DIGEST_CACHE_CAPACITY: usize = 8;
        let mut cache = self
            .digest_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = cache
            .iter()
            .position(|summary| summary.len == len && summary.digest.is_some())
        {
            let entry = cache.remove(index);
            cache.push(entry);
            return Some(entry);
        }
        drop(cache);

        let mut hasher = Fnv1a::new();
        hasher.write_u64(len as u64);
        let mut event_hasher = Fnv1a::new();
        let mut max_event_seq = None;
        for event in &self.events[..len] {
            event.digest(&mut hasher);
            event.digest(&mut event_hasher);
            max_event_seq = Some(max_event_seq.map_or(event.seq, |max: u64| max.max(event.seq)));
        }
        let summary = EventLogPrefixSummary {
            len,
            digest: Some(hasher.finish()),
            event_digest: event_hasher.finish(),
            next_seq: max_event_seq.map_or(0, |seq| seq.saturating_add(1)),
        };
        let mut cache = self
            .digest_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Another reader may have populated this entry while the digest was being computed.
        if let Some(index) = cache.iter().position(|cached| cached.len == len) {
            let entry = cache.remove(index);
            cache.push(entry);
            if entry.digest.is_some() {
                return Some(entry);
            }
        }
        if let Some(index) = cache.iter().position(|cached| cached.len == len) {
            cache.remove(index);
        }
        cache.push(summary);
        if cache.len() > DIGEST_CACHE_CAPACITY {
            cache.remove(0);
        }
        Some(summary)
    }
}
