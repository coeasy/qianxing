use super::{acquire_storage_lock, validate_segment_name, write_atomic_path, StorageError};
use qx_core::{Event, EventLog};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(test)]
thread_local! {
    static INTERRUPT_AFTER_SEGMENT_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Clone, Debug)]
pub struct SegmentedEventLogStore {
    root: PathBuf,
    max_events_per_segment: usize,
    /// The latest manifest fully checked against disk during this store instance's lifetime.
    /// Segment reads remain fully verified by `read_if_exists`; this cache only lets
    /// append writes avoid re-reading immutable history after it has been validated.
    validated_manifest: Arc<Mutex<Option<(String, SegmentManifest)>>>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub(super) struct SegmentManifest {
    schema_version: u32,
    name: String,
    #[serde(default)]
    events_per_segment: usize,
    event_count: usize,
    next_seq: u64,
    /// v1 stores EventLog::digest(); v2 stores the rolling event-only FNV digest.
    digest: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    segments: Vec<String>,
}

impl SegmentedEventLogStore {
    pub fn new(
        root: impl Into<PathBuf>,
        max_events_per_segment: usize,
    ) -> Result<Self, StorageError> {
        if max_events_per_segment == 0 {
            return Err(StorageError::Conflict(
                "事件日志 segment 大小必须为正".into(),
            ));
        }
        Ok(Self {
            root: root.into(),
            max_events_per_segment,
            validated_manifest: Arc::default(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn read(&self, name: &str) -> Result<EventLog, StorageError> {
        self.read_if_exists(name)?
            .ok_or_else(|| StorageError::NotFound(format!("segmented event log {name} 不存在")))
    }

    pub fn read_if_exists(&self, name: &str) -> Result<Option<EventLog>, StorageError> {
        validate_segment_name(name)?;
        let mut cached = self
            .validated_manifest
            .lock()
            .map_err(|_| StorageError::Conflict("segment manifest 缓存锁中毒".into()))?;
        if cached
            .as_ref()
            .is_some_and(|(cached_name, _)| cached_name == name)
        {
            *cached = None;
        }
        drop(cached);
        let Some(manifest) = self.read_manifest(name)? else {
            return Ok(None);
        };
        let layout_valid = match manifest.schema_version {
            1 => manifest.segments.is_empty() == (manifest.event_count == 0),
            2 => manifest.events_per_segment > 0 && manifest.segments.is_empty(),
            _ => false,
        };
        if !layout_valid || manifest.name != name {
            return Err(StorageError::Conflict(
                "segment manifest 身份或版本非法".into(),
            ));
        }
        let mut log = EventLog::new();
        let segment_count = if manifest.schema_version == 1 {
            manifest.segments.len()
        } else {
            manifest.event_count.div_ceil(manifest.events_per_segment)
        };
        for index in 0..segment_count {
            let generated_name;
            let segment_name = if manifest.schema_version == 1 {
                &manifest.segments[index]
            } else {
                generated_name = format!("{name}-{index:016}.jsonl");
                &generated_name
            };
            if !segment_name.starts_with(&format!("{name}-")) || !segment_name.ends_with(".jsonl") {
                return Err(StorageError::InvalidName(segment_name.clone()));
            }
            let segment = std::fs::read(self.root.join("segments").join(segment_name))
                .map_err(|error| StorageError::Io(error.to_string()))?;
            for line in segment.split(|byte| *byte == b'\n') {
                // A v2 manifest is the commit point. A process may have atomically replaced
                // the active tail (or appended part of a row) before crashing prior to
                // publishing the next manifest; bytes past event_count are uncommitted.
                if manifest.schema_version == 2 && log.len() == manifest.event_count {
                    break;
                }
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                let line = std::str::from_utf8(line).map_err(|error| {
                    StorageError::Io(format!("segment event 非 UTF-8: {error}"))
                })?;
                let event: Event = serde_json::from_str(line)
                    .map_err(|error| StorageError::Io(format!("segment event 无效: {error}")))?;
                log.append(event);
            }
        }
        log.validate().map_err(StorageError::Core)?;
        let digest_matches = if manifest.schema_version == 1 {
            log.digest() == manifest.digest
        } else {
            log.event_digest() == manifest.digest
        };
        if log.len() != manifest.event_count
            || log.next_seq() != manifest.next_seq
            || !digest_matches
        {
            return Err(StorageError::Conflict(
                "segment manifest 与事件内容摘要不一致".into(),
            ));
        }
        *self
            .validated_manifest
            .lock()
            .map_err(|_| StorageError::Conflict("segment manifest 缓存锁中毒".into()))? =
            Some((name.into(), manifest));
        Ok(Some(log))
    }

    fn read_manifest(&self, name: &str) -> Result<Option<SegmentManifest>, StorageError> {
        let path = self.manifest_path(name)?;
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        let manifest: SegmentManifest = serde_json::from_str(&content)
            .map_err(|error| StorageError::Io(format!("segment manifest 无效: {error}")))?;
        Ok(Some(manifest))
    }

    fn manifest_path(&self, name: &str) -> Result<PathBuf, StorageError> {
        validate_segment_name(name)?;
        Ok(self.root.join(format!("{name}.manifest.json")))
    }
}

impl SegmentedEventLogStore {
    pub fn write(&self, name: &str, log: &EventLog) -> Result<PathBuf, StorageError> {
        validate_segment_name(name)?;
        log.validate().map_err(StorageError::Core)?;
        std::fs::create_dir_all(self.root.join("segments"))
            .map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.root.join(format!(".{name}.segments.lock")))?;
        let manifest_path = self.manifest_path(name)?;
        let disk_manifest = self.read_manifest(name)?;
        let cached_manifest = self
            .validated_manifest
            .lock()
            .map_err(|_| StorageError::Conflict("segment manifest 缓存锁中毒".into()))?
            .as_ref()
            .filter(|(cached_name, _)| cached_name == name)
            .map(|(_, manifest)| manifest.clone());
        let cached_manifest_is_current =
            disk_manifest.is_some() && disk_manifest == cached_manifest;

        // A process-local manifest is trusted only after a full read verified it, or after this
        // store atomically published it. On a cold open or a changed manifest, retain the old
        // full verification path before caching any append boundary.
        let (existing_event_count, existing_manifest) = if cached_manifest_is_current {
            (
                disk_manifest.as_ref().map_or(0, |value| value.event_count),
                disk_manifest,
            )
        } else if disk_manifest.is_some() {
            let existing = self
                .read_if_exists(name)?
                .ok_or_else(|| StorageError::Conflict("segment manifest 在校验时消失".into()))?;
            (existing.len(), self.read_manifest(name)?)
        } else {
            (0, None)
        };

        if existing_event_count > log.len() {
            return Err(StorageError::NonAppendOnly(name.into()));
        }

        if let Some(manifest) = existing_manifest.as_ref() {
            let prefix_matches = if manifest.schema_version == 1 {
                log.digest_of_prefix(existing_event_count) == Some(manifest.digest)
            } else {
                log.event_digest_of_prefix(existing_event_count) == Some(manifest.digest)
            };
            let prefix_next_seq = log
                .next_seq_of_prefix(existing_event_count)
                .ok_or_else(|| StorageError::Conflict("EventLog 前缀长度超出日志".into()))?;
            if !prefix_matches || prefix_next_seq != manifest.next_seq {
                return Err(StorageError::NonAppendOnly(name.into()));
            }
            if existing_event_count == log.len() {
                return Ok(manifest_path);
            }
        }

        let can_append_incrementally = existing_manifest.as_ref().is_none_or(|manifest| {
            manifest.schema_version == 2
                && manifest.name == name
                && manifest.events_per_segment == self.max_events_per_segment
                && manifest.segments.is_empty()
        });
        if can_append_incrementally {
            return self.write_incremental_append(name, log, existing_event_count, manifest_path);
        }

        self.write_full_layout(name, log, manifest_path)
    }

    fn write_incremental_append(
        &self,
        name: &str,
        log: &EventLog,
        existing_event_count: usize,
        manifest_path: PathBuf,
    ) -> Result<PathBuf, StorageError> {
        let sealed_segment_count = existing_event_count / self.max_events_per_segment;
        let active_tail_index = (!existing_event_count.is_multiple_of(self.max_events_per_segment))
            .then_some(sealed_segment_count);

        let segment_count = log.len().div_ceil(self.max_events_per_segment);
        for index in sealed_segment_count..segment_count {
            let start = index * self.max_events_per_segment;
            let chunk_len = (log.len() - start).min(self.max_events_per_segment);
            let chunk = &log.events()[start..start + chunk_len];
            let segment_name = format!("{name}-{index:016}.jsonl");
            let segment_path = self.root.join("segments").join(&segment_name);
            let content = encode_segment(chunk)?;

            if active_tail_index == Some(index) {
                let committed_count = existing_event_count % self.max_events_per_segment;
                let committed_prefix_len = segment_line_prefix_len(&content, committed_count)
                    .ok_or_else(|| {
                        StorageError::Conflict(format!(
                            "新编码的活动 segment 缺少已提交前缀: {segment_name}"
                        ))
                    })?;
                let content = content.as_bytes();
                let expected_committed = &content[..committed_prefix_len];
                let old = std::fs::read(&segment_path)
                    .map_err(|error| StorageError::Io(error.to_string()))?;
                // Only the manifest prefix is committed. A prior process may have appended
                // some or all of this retry's bytes before crashing; resume only when the
                // on-disk suffix is an exact prefix of the requested append.
                let uncommitted = old.get(expected_committed.len()..).ok_or_else(|| {
                    StorageError::Conflict(format!("活动 segment 短于已提交前缀: {segment_name}"))
                })?;
                let pending = &content[committed_prefix_len..];
                if !old.starts_with(expected_committed) || !pending.starts_with(uncommitted) {
                    return Err(StorageError::Conflict(format!(
                        "活动 segment 与已提交前缀或重试内容不一致: {segment_name}"
                    )));
                }
                let remaining = &pending[uncommitted.len()..];
                if !remaining.is_empty() {
                    append_segment_bytes(&segment_path, remaining)?;
                }
            } else if segment_path.exists() {
                // A segment beyond the committed manifest can only be an orphan from an
                // interrupted prior write. Replace it with the retry's deterministic content.
                write_atomic_path(&segment_path, &self.root, &content)?;
            } else {
                write_atomic_path(&segment_path, &self.root, &content)?;
            }
        }

        #[cfg(test)]
        if INTERRUPT_AFTER_SEGMENT_SYNC.with(|interrupt| interrupt.replace(false)) {
            return Err(StorageError::Io(
                "injected interruption after segment sync".into(),
            ));
        }

        let manifest = SegmentManifest {
            schema_version: 2,
            name: name.into(),
            events_per_segment: self.max_events_per_segment,
            event_count: log.len(),
            next_seq: log.next_seq(),
            digest: log.event_digest(),
            segments: Vec::new(),
        };
        write_atomic_path(
            &manifest_path,
            &self.root,
            &serde_json::to_string_pretty(&manifest)
                .map_err(|error| StorageError::Io(error.to_string()))?,
        )?;
        *self
            .validated_manifest
            .lock()
            .map_err(|_| StorageError::Conflict("segment manifest 缓存锁中毒".into()))? =
            Some((name.into(), manifest));
        Ok(manifest_path)
    }

    fn write_full_layout(
        &self,
        name: &str,
        log: &EventLog,
        manifest_path: PathBuf,
    ) -> Result<PathBuf, StorageError> {
        for (index, chunk) in log.events().chunks(self.max_events_per_segment).enumerate() {
            let segment_name = format!("{name}-{index:016}.jsonl");
            let segment_path = self.root.join("segments").join(&segment_name);
            let content = encode_segment(chunk)?;
            if segment_path.exists() {
                let old = std::fs::read_to_string(&segment_path)
                    .map_err(|error| StorageError::Io(error.to_string()))?;
                if old != content {
                    let active_segment_can_extend = old.len() < content.len()
                        && old.lines().count() < self.max_events_per_segment
                        && content.starts_with(&old);
                    if !active_segment_can_extend {
                        return Err(StorageError::NonAppendOnly(name.into()));
                    }
                    // Atomic replacement keeps a partially written active tail invisible.
                    write_atomic_path(&segment_path, &self.root, &content)?;
                }
            } else {
                write_atomic_path(&segment_path, &self.root, &content)?;
            }
        }
        let manifest = SegmentManifest {
            schema_version: 2,
            name: name.into(),
            events_per_segment: self.max_events_per_segment,
            event_count: log.len(),
            next_seq: log.next_seq(),
            digest: log.event_digest(),
            segments: Vec::new(),
        };
        write_atomic_path(
            &manifest_path,
            &self.root,
            &serde_json::to_string_pretty(&manifest)
                .map_err(|error| StorageError::Io(error.to_string()))?,
        )?;
        *self
            .validated_manifest
            .lock()
            .map_err(|_| StorageError::Conflict("segment manifest 缓存锁中毒".into()))? =
            Some((name.into(), manifest));
        Ok(manifest_path)
    }
}

fn encode_segment(events: &[qx_core::Event]) -> Result<String, StorageError> {
    let mut encoded = Vec::new();
    for event in events {
        serde_json::to_writer(&mut encoded, event)
            .map_err(|error| StorageError::Io(error.to_string()))?;
        encoded.push(b'\n');
    }
    String::from_utf8(encoded)
        .map_err(|error| StorageError::Io(format!("segment JSON 非 UTF-8: {error}")))
}

fn segment_line_prefix_len(encoded: &str, lines: usize) -> Option<usize> {
    if lines == 0 {
        return Some(0);
    }
    encoded
        .match_indices('\n')
        .nth(lines - 1)
        .map(|(index, _)| index + 1)
}

/// Append uncommitted JSONL bytes durably before the manifest commit point advances.
fn append_segment_bytes(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| StorageError::Io(error.to_string()))?;
    file.write_all(bytes)
        .map_err(|error| StorageError::Io(error.to_string()))?;
    file.sync_all()
        .map_err(|error| StorageError::Io(error.to_string()))
}

#[cfg(test)]
#[path = "segmented_event_log/crash_tests.rs"]
mod crash_tests;
