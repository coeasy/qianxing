//! Kernel 之外的可恢复文件存储。
//!
//! 文件名由调用方提供，但内容必须先经过 EventLog 的序号、时间和 JSON 校验；写入
//! 使用临时文件+rename，避免进程中断留下半个事实日志。

use qx_control::{AuditRecord, ControlCommand, ControlPlane};
use qx_core::{Event, EventLog, Fnv1a, QxError, QxResult};
use qx_scheduler::{JobRun, JobSpec, JobStatus, Scheduler};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "sqlite")]
mod sqlite;
#[cfg(feature = "sqlite")]
pub use sqlite::{
    SqliteAuditStore, SqliteConsumerStateStore, SqliteControlCommandQueue, SqliteControlStore,
    SqliteJobQueue, SqliteOutboxStore, SqliteSnapshotStore, SqliteTokenBucket,
};

#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "postgres")]
pub use postgres::{
    PostgresAuditStore, PostgresConsumerStateStore, PostgresControlCommandQueue,
    PostgresControlStore, PostgresEventLogStore, PostgresJobQueue, PostgresOutboxStore,
    PostgresSnapshotStore, PostgresStorage,
};

#[cfg(feature = "nats")]
mod nats;
#[cfg(feature = "nats")]
pub use nats::{NatsConsumerBatchReport, NatsJetStreamConsumer, NatsJetStreamPublisher};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct EventLogFileStore {
    root: PathBuf,
}

#[derive(Debug)]
pub enum StorageError {
    Io(String),
    InvalidName(String),
    NonAppendOnly(String),
    Conflict(String),
    NotFound(String),
    LeaseHeld { run_id: u64, owner: String },
    LeaseExpired { run_id: u64 },
    Unauthorized(String),
    Core(QxError),
}

impl From<StorageError> for QxError {
    fn from(error: StorageError) -> Self {
        QxError::Permanent(format!("存储失败: {error:?}"))
    }
}

impl EventLogFileStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn write(&self, name: &str, log: &EventLog) -> Result<PathBuf, StorageError> {
        let path = self.path(name)?;
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.root.join(format!(".{name}.write.lock")))?;
        if path.exists() {
            let existing = self.read(name)?;
            if existing.len() > log.len()
                || existing
                    .events()
                    .iter()
                    .zip(log.events())
                    .any(|(old, new)| old != new)
            {
                return Err(StorageError::NonAppendOnly(name.into()));
            }
        }
        let content = log.to_json().map_err(StorageError::Core)?;
        let temp = self.root.join(format!(
            ".{name}.json.tmp.{}",
            TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&temp, content).map_err(|error| StorageError::Io(error.to_string()))?;
        sync_file(&temp)?;
        std::fs::rename(&temp, &path).map_err(|error| StorageError::Io(error.to_string()))?;
        Ok(path)
    }

    pub fn read(&self, name: &str) -> Result<EventLog, StorageError> {
        let path = self.path(name)?;
        let content =
            std::fs::read_to_string(path).map_err(|error| StorageError::Io(error.to_string()))?;
        EventLog::from_json(&content).map_err(StorageError::Core)
    }

    /// 读取一个可选的事件日志；首次启动时不存在文件不视为错误。
    ///
    /// 运行时编排器需要区分“首次启动”和“已有日志损坏”。因此不能用
    /// `read(...).unwrap_or_default()` 把 JSON/校验错误吞掉。
    pub fn read_if_exists(&self, name: &str) -> Result<Option<EventLog>, StorageError> {
        let path = self.path(name)?;
        match std::fs::read_to_string(path) {
            Ok(content) => EventLog::from_json(&content)
                .map(Some)
                .map_err(StorageError::Core),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(StorageError::Io(error.to_string())),
        }
    }

    pub fn list(&self) -> Result<Vec<PathBuf>, StorageError> {
        let mut paths = Vec::new();
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(paths),
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        for entry in entries {
            let path = entry
                .map_err(|error| StorageError::Io(error.to_string()))?
                .path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                paths.push(path);
            }
        }
        paths.sort();
        Ok(paths)
    }

    fn path(&self, name: &str) -> Result<PathBuf, StorageError> {
        if name.is_empty()
            || name.contains('/')
            || name.contains('\\')
            || name.contains("..")
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            return Err(StorageError::InvalidName(name.into()));
        }
        Ok(self.root.join(format!("{name}.json")))
    }
}

#[derive(Clone, Debug)]
pub struct SegmentedEventLogStore {
    root: PathBuf,
    max_events_per_segment: usize,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct SegmentManifest {
    schema_version: u32,
    name: String,
    event_count: usize,
    next_seq: u64,
    digest: u64,
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
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn write(&self, name: &str, log: &EventLog) -> Result<PathBuf, StorageError> {
        validate_segment_name(name)?;
        log.validate().map_err(StorageError::Core)?;
        std::fs::create_dir_all(self.root.join("segments"))
            .map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.root.join(format!(".{name}.segments.lock")))?;
        if let Some(existing) = self.read_if_exists(name)? {
            if existing.len() > log.len()
                || existing
                    .events()
                    .iter()
                    .zip(log.events())
                    .any(|(old, new)| old != new)
            {
                return Err(StorageError::NonAppendOnly(name.into()));
            }
        }

        let mut segments = Vec::new();
        for (index, chunk) in log.events().chunks(self.max_events_per_segment).enumerate() {
            let segment_name = format!("{name}-{index:016}.jsonl");
            let segment_path = self.root.join("segments").join(&segment_name);
            let content = chunk
                .iter()
                .map(|event| {
                    serde_json::to_string(event)
                        .map(|json| format!("{json}\n"))
                        .map_err(|error| StorageError::Io(error.to_string()))
                })
                .collect::<Result<String, _>>()?;
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
                    // Only the last, not-yet-full segment may grow. The
                    // manifest lock makes this a single-writer append
                    // boundary; full segments remain immutable forever.
                    write_atomic_path(&segment_path, &self.root, &content)?;
                }
            } else {
                write_atomic_path(&segment_path, &self.root, &content)?;
            }
            segments.push(segment_name);
        }
        let manifest = SegmentManifest {
            schema_version: 1,
            name: name.into(),
            event_count: log.len(),
            next_seq: log.next_seq(),
            digest: log.digest(),
            segments,
        };
        let manifest_path = self.manifest_path(name)?;
        write_atomic_path(
            &manifest_path,
            &self.root,
            &serde_json::to_string_pretty(&manifest)
                .map_err(|error| StorageError::Io(error.to_string()))?,
        )?;
        Ok(manifest_path)
    }

    pub fn read(&self, name: &str) -> Result<EventLog, StorageError> {
        self.read_if_exists(name)?
            .ok_or_else(|| StorageError::NotFound(format!("segmented event log {name} 不存在")))
    }

    pub fn read_if_exists(&self, name: &str) -> Result<Option<EventLog>, StorageError> {
        validate_segment_name(name)?;
        let path = self.manifest_path(name)?;
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        let manifest: SegmentManifest = serde_json::from_str(&content)
            .map_err(|error| StorageError::Io(format!("segment manifest 无效: {error}")))?;
        if manifest.schema_version != 1
            || manifest.name != name
            || manifest.segments.is_empty() != (manifest.event_count == 0)
        {
            return Err(StorageError::Conflict(
                "segment manifest 身份或版本非法".into(),
            ));
        }
        let mut log = EventLog::new();
        for segment_name in &manifest.segments {
            if !segment_name.starts_with(&format!("{name}-")) || !segment_name.ends_with(".jsonl") {
                return Err(StorageError::InvalidName(segment_name.clone()));
            }
            let segment = std::fs::read_to_string(self.root.join("segments").join(segment_name))
                .map_err(|error| StorageError::Io(error.to_string()))?;
            for line in segment.lines().filter(|line| !line.trim().is_empty()) {
                let event: Event = serde_json::from_str(line)
                    .map_err(|error| StorageError::Io(format!("segment event 无效: {error}")))?;
                log.append(event);
            }
        }
        log.validate().map_err(StorageError::Core)?;
        if log.len() != manifest.event_count
            || log.next_seq() != manifest.next_seq
            || log.digest() != manifest.digest
        {
            return Err(StorageError::Conflict(
                "segment manifest 与事件内容摘要不一致".into(),
            ));
        }
        Ok(Some(log))
    }

    fn manifest_path(&self, name: &str) -> Result<PathBuf, StorageError> {
        validate_segment_name(name)?;
        Ok(self.root.join(format!("{name}.manifest.json")))
    }
}

impl EventLogStore for SegmentedEventLogStore {
    fn save(&self, name: &str, log: &EventLog) -> QxResult<()> {
        self.write(name, log).map(|_| ()).map_err(Into::into)
    }

    fn load(&self, name: &str) -> QxResult<EventLog> {
        self.read(name).map_err(Into::into)
    }
}

fn validate_segment_name(name: &str) -> Result<(), StorageError> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(StorageError::InvalidName(name.into()));
    }
    Ok(())
}

/// 给恢复编排器用的轻量接口，隐藏具体文件目录。
pub trait EventLogStore {
    fn save(&self, name: &str, log: &EventLog) -> QxResult<()>;
    fn load(&self, name: &str) -> QxResult<EventLog>;
}

impl EventLogStore for EventLogFileStore {
    fn save(&self, name: &str, log: &EventLog) -> QxResult<()> {
        self.write(name, log).map(|_| ()).map_err(Into::into)
    }

    fn load(&self, name: &str) -> QxResult<EventLog> {
        self.read(name).map_err(Into::into)
    }
}

/// 已经提交的交易事实等待投递到消息系统的出站事件。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OutboxEvent {
    pub event_id: String,
    pub topic: String,
    pub partition_key: String,
    pub sequence: u64,
    #[serde(default = "default_outbox_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub trace_id: String,
    pub payload: String,
    pub created_ts: u64,
    #[serde(default)]
    pub attempts: u32,
}

impl OutboxEvent {
    pub fn validate(&self) -> Result<(), StorageError> {
        if self.event_id.trim().is_empty()
            || self.topic.trim().is_empty()
            || self.partition_key.trim().is_empty()
            || self.payload.is_empty()
            || self.schema_version == 0
        {
            return Err(StorageError::Conflict(
                "Outbox event_id、topic、partition_key、schema_version 和 payload 不能为空".into(),
            ));
        }
        validate_outbox_name(&self.event_id, "event_id")?;
        validate_outbox_name(&self.topic, "topic")
    }

    /// 比较事件事实本身；`attempts` 是 relay 的可变投递元数据，不参与幂等判断。
    pub fn same_fact(&self, other: &Self) -> bool {
        self.event_id == other.event_id
            && self.topic == other.topic
            && self.partition_key == other.partition_key
            && self.sequence == other.sequence
            && self.schema_version == other.schema_version
            && self.trace_id == other.trace_id
            && self.payload == other.payload
            && self.created_ts == other.created_ts
    }
}

pub fn project_event_log_to_outbox(
    log_name: &str,
    log: &EventLog,
) -> Result<Vec<OutboxEvent>, StorageError> {
    if log_name.trim().is_empty()
        || !log_name
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
    {
        return Err(StorageError::InvalidName(log_name.into()));
    }
    log.validate().map_err(StorageError::Core)?;
    log.events()
        .iter()
        .map(|event| {
            let outbox = OutboxEvent {
                event_id: format!("{log_name}:{}", event.seq),
                topic: "qx.eventlog".into(),
                partition_key: if event.correlation_id.is_empty() {
                    log_name.into()
                } else {
                    event.correlation_id.clone()
                },
                sequence: event.seq,
                schema_version: 1,
                trace_id: event.correlation_id.clone(),
                payload: serde_json::to_string(event).map_err(|error| {
                    StorageError::Io(format!("EventLog Outbox 序列化失败: {error}"))
                })?,
                created_ts: event.ts,
                attempts: 0,
            };
            outbox.validate()?;
            Ok(outbox)
        })
        .collect()
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OutboxLease {
    pub event_id: String,
    pub owner: String,
    pub expires_ts: u64,
    #[serde(default = "default_fencing_token")]
    pub fencing_token: u64,
}

/// 文件、SQLite、PostgreSQL 和 MQ relay 共用的出站事件语义。
pub trait OutboxStore: Send + Sync {
    fn append_outbox(&self, event: OutboxEvent) -> Result<(), StorageError>;
    fn available_outbox(&self, now: u64) -> Result<Vec<OutboxEvent>, StorageError>;
    fn claim_outbox(
        &self,
        event_id: &str,
        owner: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<OutboxLease, StorageError>;
    fn ack_outbox(
        &self,
        event_id: &str,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<(), StorageError>;
    fn retry_outbox(
        &self,
        event_id: &str,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<(), StorageError>;
}

/// MQ/HTTP/Webhook 等真实投递器的最小边界。发布成功后 relay 才确认 Outbox，
/// 因而天然是 at-least-once；消费者必须使用 `event_id` 幂等。
pub trait OutboxPublisher: Send + Sync {
    fn publish(&self, event: &OutboxEvent) -> Result<(), String>;
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct OutboxRelayReport {
    pub scanned: u64,
    pub published: u64,
    pub retried: u64,
    pub lease_conflicts: u64,
    pub publish_failures: u64,
    pub last_error: Option<String>,
}

/// 通用 Outbox relay。它不关心 NATS、Redpanda 或 HTTP 的具体 SDK，负责保证
/// claim → publish → ack 的生命周期；发布失败只释放租约并递增 attempts。
pub struct OutboxRelay<S, P> {
    store: S,
    publisher: P,
    owner: String,
    lease_seconds: u64,
}

impl<S, P> OutboxRelay<S, P>
where
    S: OutboxStore,
    P: OutboxPublisher,
{
    pub fn new(
        store: S,
        publisher: P,
        owner: impl Into<String>,
        lease_seconds: u64,
    ) -> Result<Self, StorageError> {
        let owner = owner.into();
        if owner.trim().is_empty() || lease_seconds == 0 {
            return Err(StorageError::Conflict(
                "Outbox relay owner 和租约时长不能为空".into(),
            ));
        }
        Ok(Self {
            store,
            publisher,
            owner,
            lease_seconds,
        })
    }

    pub fn pump_once(&self, now: u64, limit: usize) -> Result<OutboxRelayReport, StorageError> {
        if limit == 0 {
            return Ok(OutboxRelayReport::default());
        }
        let mut report = OutboxRelayReport::default();
        for event in self.store.available_outbox(now)?.into_iter().take(limit) {
            report.scanned += 1;
            let lease =
                match self
                    .store
                    .claim_outbox(&event.event_id, &self.owner, now, self.lease_seconds)
                {
                    Ok(lease) => lease,
                    Err(StorageError::LeaseHeld { .. }) => {
                        report.lease_conflicts += 1;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
            match self.publisher.publish(&event) {
                Ok(()) => {
                    self.store.ack_outbox(
                        &event.event_id,
                        &self.owner,
                        lease.fencing_token,
                        now,
                    )?;
                    report.published += 1;
                }
                Err(error) => {
                    self.store.retry_outbox(
                        &event.event_id,
                        &self.owner,
                        lease.fencing_token,
                        now,
                    )?;
                    report.publish_failures += 1;
                    report.last_error = Some(error);
                    report.retried += 1;
                }
            }
        }
        Ok(report)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ConsumerCheckpoint {
    pub group_id: String,
    pub topic: String,
    pub partition_key: String,
    pub offset: u64,
    pub event_id: String,
    pub updated_ts: u64,
}

impl ConsumerCheckpoint {
    pub fn validate(&self) -> Result<(), StorageError> {
        for (value, field) in [
            (&self.group_id, "group_id"),
            (&self.topic, "topic"),
            (&self.partition_key, "partition_key"),
            (&self.event_id, "event_id"),
        ] {
            validate_outbox_name(value, field)?;
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DeadLetterRecord {
    pub group_id: String,
    pub topic: String,
    pub partition_key: String,
    pub event_id: String,
    pub offset: u64,
    pub attempts: u32,
    pub error: String,
    pub failed_ts: u64,
    pub event: OutboxEvent,
}

impl DeadLetterRecord {
    pub fn validate(&self) -> Result<(), StorageError> {
        ConsumerCheckpoint {
            group_id: self.group_id.clone(),
            topic: self.topic.clone(),
            partition_key: self.partition_key.clone(),
            offset: self.offset,
            event_id: self.event_id.clone(),
            updated_ts: self.failed_ts,
        }
        .validate()?;
        self.event.validate()
    }

    pub fn validate_for(&self, checkpoint: &ConsumerCheckpoint) -> Result<(), StorageError> {
        self.validate()?;
        if self.group_id != checkpoint.group_id
            || self.topic != checkpoint.topic
            || self.partition_key != checkpoint.partition_key
            || self.event_id != checkpoint.event_id
            || self.offset != checkpoint.offset
        {
            return Err(StorageError::Conflict(
                "dead-letter record 与 consumer checkpoint 不一致".into(),
            ));
        }
        Ok(())
    }
}

pub trait ConsumerStateStore: Send + Sync {
    fn load_checkpoint(
        &self,
        group_id: &str,
        topic: &str,
        partition_key: &str,
    ) -> Result<Option<ConsumerCheckpoint>, StorageError>;
    fn is_processed(&self, group_id: &str, event_id: &str) -> Result<bool, StorageError>;
    fn commit_processed(&self, checkpoint: ConsumerCheckpoint) -> Result<(), StorageError>;
    fn append_dead_letter(&self, record: DeadLetterRecord) -> Result<(), StorageError>;
    fn dead_letters(
        &self,
        group_id: &str,
        limit: usize,
    ) -> Result<Vec<DeadLetterRecord>, StorageError>;
}

/// A deterministic projection produced by an in-process consumer reducer.
///
/// The projection is deliberately a durable JSON value rather than an
/// arbitrary callback into a database. Concrete SQLite/PostgreSQL stores write
/// this record and the processed marker/checkpoint in one transaction. This
/// gives reducers a safe reference implementation for the
/// "business projection + consumer offset" atomicity boundary; an external
/// process handler remains at-least-once and must own its own transaction.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ConsumerProjection {
    pub group_id: String,
    pub projection_key: String,
    pub topic: String,
    pub partition_key: String,
    pub offset: u64,
    pub event_id: String,
    pub payload: String,
    pub updated_ts: u64,
}

impl ConsumerProjection {
    pub fn for_checkpoint(
        checkpoint: &ConsumerCheckpoint,
        projection_key: impl Into<String>,
        payload: impl Into<String>,
    ) -> Self {
        Self {
            group_id: checkpoint.group_id.clone(),
            projection_key: projection_key.into(),
            topic: checkpoint.topic.clone(),
            partition_key: checkpoint.partition_key.clone(),
            offset: checkpoint.offset,
            event_id: checkpoint.event_id.clone(),
            payload: payload.into(),
            updated_ts: checkpoint.updated_ts,
        }
    }

    pub fn validate_for(&self, checkpoint: &ConsumerCheckpoint) -> Result<(), StorageError> {
        checkpoint.validate()?;
        for (value, field) in [
            (&self.group_id, "projection.group_id"),
            (&self.projection_key, "projection_key"),
            (&self.topic, "projection.topic"),
            (&self.partition_key, "projection.partition_key"),
            (&self.event_id, "projection.event_id"),
        ] {
            validate_outbox_name(value, field)?;
        }
        if self.group_id != checkpoint.group_id
            || self.topic != checkpoint.topic
            || self.partition_key != checkpoint.partition_key
            || self.offset != checkpoint.offset
            || self.event_id != checkpoint.event_id
            || self.updated_ts != checkpoint.updated_ts
        {
            return Err(StorageError::Conflict(
                "consumer projection 与 checkpoint 不一致".into(),
            ));
        }
        if self.payload.len() > 16 * 1024 * 1024 {
            return Err(StorageError::Conflict(
                "consumer projection payload 超过 16 MiB".into(),
            ));
        }
        serde_json::from_str::<serde_json::Value>(&self.payload).map_err(|error| {
            StorageError::Conflict(format!("consumer projection JSON 非法: {error}"))
        })?;
        Ok(())
    }
}

/// Storage boundary for reducers that need a durable projection and the
/// consumer checkpoint to commit atomically. The read method is intentionally
/// small: domain reducers can deserialize the projection into their own model
/// without coupling qx-storage to business tables.
pub trait TransactionalConsumerStateStore: ConsumerStateStore {
    fn commit_processed_with_projection(
        &self,
        checkpoint: ConsumerCheckpoint,
        projection: ConsumerProjection,
    ) -> Result<(), StorageError>;

    fn append_dead_letter_and_commit(
        &self,
        record: DeadLetterRecord,
        checkpoint: ConsumerCheckpoint,
    ) -> Result<(), StorageError>;

    fn load_projection(
        &self,
        group_id: &str,
        projection_key: &str,
    ) -> Result<Option<ConsumerProjection>, StorageError>;
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ConsumerOutcome {
    Applied,
    Duplicate,
    Retried { error: String },
    DeadLettered,
}

pub struct ConsumerEngine<S> {
    store: S,
    group_id: String,
    max_attempts: u32,
}

impl<S> ConsumerEngine<S>
where
    S: ConsumerStateStore,
{
    pub fn new(
        store: S,
        group_id: impl Into<String>,
        max_attempts: u32,
    ) -> Result<Self, StorageError> {
        let group_id = group_id.into();
        validate_outbox_name(&group_id, "group_id")?;
        if max_attempts == 0 {
            return Err(StorageError::Conflict(
                "consumer max_attempts 必须大于 0".into(),
            ));
        }
        Ok(Self {
            store,
            group_id,
            max_attempts,
        })
    }

    pub fn consume<F>(
        &self,
        event: &OutboxEvent,
        offset: u64,
        delivery_attempt: u32,
        now: u64,
        handler: F,
    ) -> Result<ConsumerOutcome, StorageError>
    where
        F: FnOnce(&OutboxEvent) -> Result<(), String>,
    {
        event.validate()?;
        if self.store.is_processed(&self.group_id, &event.event_id)? {
            return Ok(ConsumerOutcome::Duplicate);
        }
        let checkpoint = ConsumerCheckpoint {
            group_id: self.group_id.clone(),
            topic: event.topic.clone(),
            partition_key: event.partition_key.clone(),
            offset,
            event_id: event.event_id.clone(),
            updated_ts: now,
        };
        checkpoint.validate()?;
        if let Some(previous) =
            self.store
                .load_checkpoint(&self.group_id, &event.topic, &event.partition_key)?
        {
            if previous.offset > offset {
                return Err(StorageError::Conflict(format!(
                    "consumer checkpoint 回退: {} > {}",
                    previous.offset, offset
                )));
            }
        }
        if let Err(error) = handler(event) {
            if delivery_attempt < self.max_attempts {
                return Ok(ConsumerOutcome::Retried { error });
            }
            self.store.append_dead_letter(DeadLetterRecord {
                group_id: self.group_id.clone(),
                topic: event.topic.clone(),
                partition_key: event.partition_key.clone(),
                event_id: event.event_id.clone(),
                offset,
                attempts: delivery_attempt,
                error,
                failed_ts: now,
                event: event.clone(),
            })?;
            // Dead-lettering is a terminal source-consumer decision. Persist the
            // processed marker/checkpoint as well, so a broker can acknowledge
            // the source message without redelivering it forever. The complete
            // failed event remains available through `dead_letters`.
            self.store.commit_processed(checkpoint)?;
            return Ok(ConsumerOutcome::DeadLettered);
        }
        self.store.commit_processed(checkpoint)?;
        Ok(ConsumerOutcome::Applied)
    }

    /// Consume through the atomic projection boundary. The reducer computes a
    /// deterministic projection; the storage backend commits that projection,
    /// processed event id and checkpoint in one unit. This is the supported
    /// in-process path when a business read model must not get ahead of (or
    /// fall behind) its consumer offset.
    pub fn consume_with_projection<F>(
        &self,
        event: &OutboxEvent,
        offset: u64,
        delivery_attempt: u32,
        now: u64,
        handler: F,
    ) -> Result<ConsumerOutcome, StorageError>
    where
        S: TransactionalConsumerStateStore,
        F: FnOnce(&OutboxEvent, &ConsumerCheckpoint) -> Result<ConsumerProjection, String>,
    {
        event.validate()?;
        if self.store.is_processed(&self.group_id, &event.event_id)? {
            return Ok(ConsumerOutcome::Duplicate);
        }
        let checkpoint = ConsumerCheckpoint {
            group_id: self.group_id.clone(),
            topic: event.topic.clone(),
            partition_key: event.partition_key.clone(),
            offset,
            event_id: event.event_id.clone(),
            updated_ts: now,
        };
        checkpoint.validate()?;
        if let Some(previous) =
            self.store
                .load_checkpoint(&self.group_id, &event.topic, &event.partition_key)?
        {
            if previous.offset > offset {
                return Err(StorageError::Conflict(format!(
                    "consumer checkpoint 回退: {} > {}",
                    previous.offset, offset
                )));
            }
        }
        let projection = match handler(event, &checkpoint) {
            Ok(projection) => projection,
            Err(error) => {
                if delivery_attempt < self.max_attempts {
                    return Ok(ConsumerOutcome::Retried { error });
                }
                let record = DeadLetterRecord {
                    group_id: self.group_id.clone(),
                    topic: event.topic.clone(),
                    partition_key: event.partition_key.clone(),
                    event_id: event.event_id.clone(),
                    offset,
                    attempts: delivery_attempt,
                    error,
                    failed_ts: now,
                    event: event.clone(),
                };
                self.store
                    .append_dead_letter_and_commit(record, checkpoint)?;
                return Ok(ConsumerOutcome::DeadLettered);
            }
        };
        projection.validate_for(&checkpoint)?;
        self.store
            .commit_processed_with_projection(checkpoint, projection)?;
        Ok(ConsumerOutcome::Applied)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
struct FileConsumerState {
    checkpoint: Option<ConsumerCheckpoint>,
    processed_event_ids: BTreeSet<String>,
    dead_letters: Vec<DeadLetterRecord>,
    #[serde(default)]
    projections: std::collections::BTreeMap<String, ConsumerProjection>,
}

#[derive(Clone, Debug)]
pub struct FileConsumerStateStore {
    root: PathBuf,
}

impl FileConsumerStateStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn state_key(
        &self,
        group_id: &str,
        topic: &str,
        partition_key: &str,
    ) -> Result<String, StorageError> {
        validate_outbox_name(group_id, "group_id")?;
        validate_outbox_name(topic, "topic")?;
        validate_outbox_name(partition_key, "partition_key")?;
        Ok(outbox_file_key(&format!(
            "{group_id}|{topic}|{partition_key}"
        )))
    }

    fn path_for(
        &self,
        group_id: &str,
        topic: &str,
        partition_key: &str,
    ) -> Result<PathBuf, StorageError> {
        let key = self.state_key(group_id, topic, partition_key)?;
        Ok(self.root.join("consumers").join(format!("{key}.json")))
    }

    fn lock_for(
        &self,
        group_id: &str,
        topic: &str,
        partition_key: &str,
    ) -> Result<PathBuf, StorageError> {
        let key = self.state_key(group_id, topic, partition_key)?;
        Ok(self.root.join("consumers").join(format!("{key}.lock")))
    }

    fn read_state(&self, path: &Path) -> Result<FileConsumerState, StorageError> {
        match std::fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content)
                .map_err(|error| StorageError::Io(format!("consumer 状态解析失败: {error}"))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(FileConsumerState::default())
            }
            Err(error) => Err(StorageError::Io(error.to_string())),
        }
    }

    fn write_state(&self, path: &Path, state: &FileConsumerState) -> Result<(), StorageError> {
        let content = serde_json::to_string(state)
            .map_err(|error| StorageError::Io(format!("consumer 状态序列化失败: {error}")))?;
        write_atomic_path(path, &self.root, &content)
    }
}

impl ConsumerStateStore for FileConsumerStateStore {
    fn load_checkpoint(
        &self,
        group_id: &str,
        topic: &str,
        partition_key: &str,
    ) -> Result<Option<ConsumerCheckpoint>, StorageError> {
        let path = self.path_for(group_id, topic, partition_key)?;
        Ok(self.read_state(&path)?.checkpoint)
    }

    fn is_processed(&self, group_id: &str, event_id: &str) -> Result<bool, StorageError> {
        validate_outbox_name(group_id, "group_id")?;
        validate_outbox_name(event_id, "event_id")?;
        let dir = self.root.join("consumers");
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        for entry in entries {
            let path = entry
                .map_err(|error| StorageError::Io(error.to_string()))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let state = self.read_state(&path)?;
            if state.processed_event_ids.contains(event_id)
                && state
                    .checkpoint
                    .as_ref()
                    .is_some_and(|checkpoint| checkpoint.group_id == group_id)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn commit_processed(&self, checkpoint: ConsumerCheckpoint) -> Result<(), StorageError> {
        checkpoint.validate()?;
        let path = self.path_for(
            &checkpoint.group_id,
            &checkpoint.topic,
            &checkpoint.partition_key,
        )?;
        std::fs::create_dir_all(self.root.join("consumers"))
            .map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.lock_for(
            &checkpoint.group_id,
            &checkpoint.topic,
            &checkpoint.partition_key,
        )?)?;
        let mut state = self.read_state(&path)?;
        if state.processed_event_ids.contains(&checkpoint.event_id) {
            return Ok(());
        }
        if let Some(previous) = state.checkpoint.as_ref() {
            if previous.offset > checkpoint.offset {
                return Err(StorageError::Conflict("consumer checkpoint 回退".into()));
            }
            if previous.offset == checkpoint.offset && previous.event_id != checkpoint.event_id {
                return Err(StorageError::Conflict(
                    "consumer 相同 offset 对应不同 event_id".into(),
                ));
            }
        }
        state
            .processed_event_ids
            .insert(checkpoint.event_id.clone());
        state.checkpoint = Some(checkpoint);
        self.write_state(&path, &state)
    }

    fn append_dead_letter(&self, record: DeadLetterRecord) -> Result<(), StorageError> {
        record.validate()?;
        let path = self.path_for(&record.group_id, &record.topic, &record.partition_key)?;
        std::fs::create_dir_all(self.root.join("consumers"))
            .map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.lock_for(
            &record.group_id,
            &record.topic,
            &record.partition_key,
        )?)?;
        let mut state = self.read_state(&path)?;
        if !state.dead_letters.iter().any(|existing| {
            existing.event_id == record.event_id && existing.attempts == record.attempts
        }) {
            state.dead_letters.push(record);
        }
        self.write_state(&path, &state)
    }

    fn dead_letters(
        &self,
        group_id: &str,
        limit: usize,
    ) -> Result<Vec<DeadLetterRecord>, StorageError> {
        validate_outbox_name(group_id, "group_id")?;
        let dir = self.root.join("consumers");
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        let mut result = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|error| StorageError::Io(error.to_string()))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let state = self.read_state(&path)?;
            result.extend(
                state
                    .dead_letters
                    .into_iter()
                    .filter(|record| record.group_id == group_id),
            );
        }
        result.sort_by_key(|record| (record.failed_ts, record.event_id.clone()));
        result.truncate(limit);
        Ok(result)
    }
}

impl TransactionalConsumerStateStore for FileConsumerStateStore {
    fn commit_processed_with_projection(
        &self,
        checkpoint: ConsumerCheckpoint,
        projection: ConsumerProjection,
    ) -> Result<(), StorageError> {
        projection.validate_for(&checkpoint)?;
        let path = self.path_for(
            &checkpoint.group_id,
            &checkpoint.topic,
            &checkpoint.partition_key,
        )?;
        std::fs::create_dir_all(self.root.join("consumers"))
            .map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.lock_for(
            &checkpoint.group_id,
            &checkpoint.topic,
            &checkpoint.partition_key,
        )?)?;
        let mut state = self.read_state(&path)?;
        if state.processed_event_ids.contains(&checkpoint.event_id) {
            return Ok(());
        }
        if let Some(previous) = state.checkpoint.as_ref() {
            if previous.offset > checkpoint.offset {
                return Err(StorageError::Conflict("consumer checkpoint 回退".into()));
            }
            if previous.offset == checkpoint.offset && previous.event_id != checkpoint.event_id {
                return Err(StorageError::Conflict(
                    "consumer 相同 offset 对应不同 event_id".into(),
                ));
            }
        }
        if let Some(previous) = state.projections.get(&projection.projection_key) {
            if previous.offset > projection.offset
                || (previous.offset == projection.offset
                    && previous.event_id != projection.event_id)
            {
                return Err(StorageError::Conflict(
                    "consumer projection 顺序或 event_id 非法".into(),
                ));
            }
        }
        state
            .processed_event_ids
            .insert(checkpoint.event_id.clone());
        state
            .projections
            .insert(projection.projection_key.clone(), projection);
        state.checkpoint = Some(checkpoint);
        self.write_state(&path, &state)
    }

    fn append_dead_letter_and_commit(
        &self,
        record: DeadLetterRecord,
        checkpoint: ConsumerCheckpoint,
    ) -> Result<(), StorageError> {
        record.validate_for(&checkpoint)?;
        let path = self.path_for(
            &checkpoint.group_id,
            &checkpoint.topic,
            &checkpoint.partition_key,
        )?;
        std::fs::create_dir_all(self.root.join("consumers"))
            .map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.lock_for(
            &checkpoint.group_id,
            &checkpoint.topic,
            &checkpoint.partition_key,
        )?)?;
        let mut state = self.read_state(&path)?;
        if state.processed_event_ids.contains(&checkpoint.event_id) {
            return Ok(());
        }
        if let Some(previous) = state.checkpoint.as_ref() {
            if previous.offset > checkpoint.offset {
                return Err(StorageError::Conflict("consumer checkpoint 回退".into()));
            }
            if previous.offset == checkpoint.offset && previous.event_id != checkpoint.event_id {
                return Err(StorageError::Conflict(
                    "consumer 相同 offset 对应不同 event_id".into(),
                ));
            }
        }
        if !state.dead_letters.iter().any(|existing| {
            existing.event_id == record.event_id && existing.attempts == record.attempts
        }) {
            state.dead_letters.push(record);
        }
        state
            .processed_event_ids
            .insert(checkpoint.event_id.clone());
        state.checkpoint = Some(checkpoint);
        self.write_state(&path, &state)
    }

    fn load_projection(
        &self,
        group_id: &str,
        projection_key: &str,
    ) -> Result<Option<ConsumerProjection>, StorageError> {
        validate_outbox_name(group_id, "group_id")?;
        validate_outbox_name(projection_key, "projection_key")?;
        let dir = self.root.join("consumers");
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        for entry in entries {
            let path = entry
                .map_err(|error| StorageError::Io(error.to_string()))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            if let Some(projection) = self.read_state(&path)?.projections.get(projection_key) {
                if projection.group_id == group_id {
                    return Ok(Some(projection.clone()));
                }
            }
        }
        Ok(None)
    }
}

#[derive(Clone, Debug)]
pub struct FileOutboxStore {
    root: PathBuf,
}

impl FileOutboxStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn append(&self, event: OutboxEvent) -> Result<(), StorageError> {
        event.validate()?;
        self.ensure_dirs()?;
        let path = self.event_path(&event.event_id)?;
        let _lock = acquire_storage_lock(self.root.join("outbox.append.lock"))?;
        if path.exists() {
            let existing = self.read_json::<OutboxEvent>(&path)?;
            if existing.same_fact(&event) {
                return Ok(());
            }
            return Err(StorageError::Conflict(format!(
                "event_id {} 已被不同 Outbox 事件占用",
                event.event_id
            )));
        }
        let content = serde_json::to_string(&event)
            .map_err(|error| StorageError::Io(format!("Outbox 事件序列化失败: {error}")))?;
        write_atomic_path(&path, &self.root, &content)
    }

    pub fn available(&self, now: u64) -> Result<Vec<OutboxEvent>, StorageError> {
        self.ensure_dirs()?;
        let mut events = Vec::new();
        for entry in std::fs::read_dir(self.root.join("outbox/events"))
            .map_err(|error| StorageError::Io(error.to_string()))?
        {
            let path = entry
                .map_err(|error| StorageError::Io(error.to_string()))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let event: OutboxEvent = self.read_json(&path)?;
            event.validate()?;
            let lease_path = self.lease_path(&event.event_id)?;
            if !lease_path.exists() || self.read_json::<OutboxLease>(&lease_path)?.expires_ts <= now
            {
                events.push(event);
            }
        }
        events.sort_by_key(|event| (event.created_ts, event.sequence, event.event_id.clone()));
        Ok(events)
    }

    pub fn claim(
        &self,
        event_id: &str,
        owner: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<OutboxLease, StorageError> {
        validate_outbox_name(event_id, "event_id")?;
        if owner.trim().is_empty() || lease_seconds == 0 {
            return Err(StorageError::Conflict(
                "Outbox worker 和租约时长不能为空".into(),
            ));
        }
        self.ensure_dirs()?;
        let _lock = acquire_storage_lock(self.lock_path(event_id)?)?;
        if !self.event_path(event_id)?.exists() {
            return Err(StorageError::NotFound(format!(
                "Outbox event_id {event_id}"
            )));
        }
        let lease_path = self.lease_path(event_id)?;
        let mut fencing_token = 1;
        if lease_path.exists() {
            let current: OutboxLease = self.read_json(&lease_path)?;
            if current.expires_ts > now && current.owner != owner {
                return Err(StorageError::LeaseHeld {
                    run_id: 0,
                    owner: current.owner,
                });
            }
            if current.expires_ts <= now {
                fencing_token = current.fencing_token.saturating_add(1).max(1);
                std::fs::remove_file(&lease_path)
                    .map_err(|error| StorageError::Io(error.to_string()))?;
            } else {
                fencing_token = current.fencing_token.max(1);
            }
        }
        let lease = OutboxLease {
            event_id: event_id.into(),
            owner: owner.into(),
            expires_ts: now.saturating_add(lease_seconds),
            fencing_token,
        };
        let content = serde_json::to_string(&lease)
            .map_err(|error| StorageError::Io(format!("Outbox 租约序列化失败: {error}")))?;
        write_atomic_path(&lease_path, &self.root, &content)?;
        Ok(lease)
    }

    pub fn ack(
        &self,
        event_id: &str,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<(), StorageError> {
        let _lock = acquire_storage_lock(self.lock_path(event_id)?)?;
        let lease = self.read_json::<OutboxLease>(&self.lease_path(event_id)?)?;
        validate_outbox_lease(&lease, event_id, owner, fencing_token, now)?;
        std::fs::remove_file(self.event_path(event_id)?)
            .map_err(|error| StorageError::Io(error.to_string()))?;
        std::fs::remove_file(self.lease_path(event_id)?)
            .map_err(|error| StorageError::Io(error.to_string()))?;
        Ok(())
    }

    pub fn retry(
        &self,
        event_id: &str,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<(), StorageError> {
        let _lock = acquire_storage_lock(self.lock_path(event_id)?)?;
        let lease = self.read_json::<OutboxLease>(&self.lease_path(event_id)?)?;
        validate_outbox_lease(&lease, event_id, owner, fencing_token, now)?;
        let path = self.event_path(event_id)?;
        let mut event = self.read_json::<OutboxEvent>(&path)?;
        event.attempts = event.attempts.saturating_add(1);
        let content = serde_json::to_string(&event)
            .map_err(|error| StorageError::Io(format!("Outbox 重试事件序列化失败: {error}")))?;
        write_atomic_path(&path, &self.root, &content)?;
        std::fs::remove_file(self.lease_path(event_id)?)
            .map_err(|error| StorageError::Io(error.to_string()))?;
        Ok(())
    }

    fn ensure_dirs(&self) -> Result<(), StorageError> {
        for path in ["outbox/events", "outbox/leases", "outbox/locks"] {
            std::fs::create_dir_all(self.root.join(path))
                .map_err(|error| StorageError::Io(error.to_string()))?;
        }
        Ok(())
    }

    fn event_path(&self, event_id: &str) -> Result<PathBuf, StorageError> {
        validate_outbox_name(event_id, "event_id")?;
        Ok(self
            .root
            .join("outbox/events")
            .join(format!("{}.json", outbox_file_key(event_id))))
    }

    fn lease_path(&self, event_id: &str) -> Result<PathBuf, StorageError> {
        validate_outbox_name(event_id, "event_id")?;
        Ok(self
            .root
            .join("outbox/leases")
            .join(format!("{}.json", outbox_file_key(event_id))))
    }

    fn lock_path(&self, event_id: &str) -> Result<PathBuf, StorageError> {
        validate_outbox_name(event_id, "event_id")?;
        Ok(self
            .root
            .join("outbox/locks")
            .join(format!("{}.lock", outbox_file_key(event_id))))
    }

    fn read_json<T: DeserializeOwned>(&self, path: &Path) -> Result<T, StorageError> {
        let text =
            std::fs::read_to_string(path).map_err(|error| StorageError::Io(error.to_string()))?;
        serde_json::from_str(&text).map_err(|error| StorageError::Io(error.to_string()))
    }
}

impl OutboxStore for FileOutboxStore {
    fn append_outbox(&self, event: OutboxEvent) -> Result<(), StorageError> {
        self.append(event)
    }

    fn available_outbox(&self, now: u64) -> Result<Vec<OutboxEvent>, StorageError> {
        self.available(now)
    }

    fn claim_outbox(
        &self,
        event_id: &str,
        owner: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<OutboxLease, StorageError> {
        self.claim(event_id, owner, now, lease_seconds)
    }

    fn ack_outbox(
        &self,
        event_id: &str,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<(), StorageError> {
        self.ack(event_id, owner, fencing_token, now)
    }

    fn retry_outbox(
        &self,
        event_id: &str,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<(), StorageError> {
        self.retry(event_id, owner, fencing_token, now)
    }
}

fn validate_outbox_name(value: &str, field: &str) -> Result<(), StorageError> {
    if value.is_empty()
        || value.len() > 240
        || value.contains('/')
        || value.contains('\\')
        || value.contains("..")
        || !value
            .chars()
            .all(|item| item.is_ascii_alphanumeric() || matches!(item, '-' | '_' | '.' | ':' | '@'))
    {
        return Err(StorageError::InvalidName(format!("Outbox {field} 非法")));
    }
    Ok(())
}

fn outbox_file_key(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn validate_outbox_lease(
    lease: &OutboxLease,
    event_id: &str,
    owner: &str,
    fencing_token: u64,
    now: u64,
) -> Result<(), StorageError> {
    if lease.event_id != event_id || lease.owner != owner || lease.fencing_token != fencing_token {
        return Err(StorageError::Unauthorized(format!(
            "Outbox event_id {event_id} 的 worker 或 fencing token 无效"
        )));
    }
    if lease.expires_ts <= now {
        return Err(StorageError::LeaseExpired { run_id: 0 });
    }
    Ok(())
}

fn default_outbox_schema_version() -> u32 {
    1
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct QueuedJob {
    pub job: JobSpec,
    pub run: JobRun,
    pub enqueued_ts: u64,
}

impl QueuedJob {
    fn validate(&self) -> Result<(), StorageError> {
        self.job
            .validate()
            .map_err(|error| StorageError::Conflict(format!("队列 JobSpec 非法: {error:?}")))?;
        if self.run.job_id != self.job.job_id
            || self.run.run_id != self.job.stable_key(&self.run.trading_day)
            || !matches!(self.run.status, JobStatus::Running)
        {
            return Err(StorageError::Conflict(
                "队列任务与 JobRun 身份不一致".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct JobLease {
    pub run_id: u64,
    pub owner: String,
    pub expires_ts: u64,
    #[serde(default = "default_fencing_token")]
    pub fencing_token: u64,
}

/// 可恢复的控制命令队列。它只负责命令的排队、租约与确认，
/// 不执行交易副作用；执行结果仍必须回写 `ControlPlane` 和 EventLog。
///
/// 文件后端用于单机/开发部署，生产多进程或多节点可替换为 SQLite/MQ，
/// 但必须保留 command_id 幂等、租约过期接管和 fencing token 语义。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct QueuedControlCommand {
    pub command: ControlCommand,
    pub enqueued_ts: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ControlCommandLease {
    pub command_id: u64,
    pub owner: String,
    pub expires_ts: u64,
    #[serde(default = "default_fencing_token")]
    pub fencing_token: u64,
}

#[derive(Clone, Debug)]
pub struct ControlCommandQueue {
    root: PathBuf,
}

/// 文件/SQLite/MQ 控制命令队列必须共同实现的语义边界。
pub trait ControlCommandQueueBackend: Send + Sync {
    fn enqueue_command(
        &self,
        command: ControlCommand,
        enqueued_ts: u64,
    ) -> Result<PathBuf, StorageError>;
    fn available_commands(&self, now: u64) -> Result<Vec<QueuedControlCommand>, StorageError>;
    fn claim_command(
        &self,
        command_id: u64,
        owner: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<ControlCommandLease, StorageError>;
    fn ack_command_at(
        &self,
        command_id: u64,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<PathBuf, StorageError>;
}

impl ControlCommandQueue {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn enqueue(
        &self,
        command: ControlCommand,
        enqueued_ts: u64,
    ) -> Result<PathBuf, StorageError> {
        command
            .validate()
            .map_err(|error| StorageError::Conflict(format!("控制命令非法: {error:?}")))?;
        let queued = QueuedControlCommand {
            command,
            enqueued_ts,
        };
        let path = self.command_path(queued.command.command_id)?;
        self.ensure_dirs()?;
        let _enqueue_lock = acquire_storage_lock(
            self.root
                .join("commands")
                .join(format!("{}.enqueue.lock", queued.command.command_id)),
        )?;
        if path.exists() {
            let existing = self.read_json::<QueuedControlCommand>(&path)?;
            if existing.command == queued.command {
                return Ok(path);
            }
            return Err(StorageError::Conflict(format!(
                "command_id {} 已被不同命令占用",
                queued.command.command_id
            )));
        }
        write_atomic_path(
            &path,
            &self.root,
            &serde_json::to_string(&queued)
                .map_err(|error| StorageError::Io(format!("控制命令序列化失败: {error}")))?,
        )?;
        Ok(path)
    }

    pub fn pending(&self) -> Result<Vec<QueuedControlCommand>, StorageError> {
        let dir = self.root.join("commands");
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        let mut commands = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|error| StorageError::Io(error.to_string()))?
                .path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".lease.json"))
            {
                let queued: QueuedControlCommand = self.read_json(&path)?;
                queued.command.validate().map_err(|error| {
                    StorageError::Conflict(format!("队列控制命令非法: {error:?}"))
                })?;
                commands.push(queued);
            }
        }
        commands.sort_by_key(|queued| (queued.enqueued_ts, queued.command.command_id));
        Ok(commands)
    }

    pub fn available(&self, now: u64) -> Result<Vec<QueuedControlCommand>, StorageError> {
        let mut available = Vec::new();
        for command in self.pending()? {
            let lease_path = self.lease_path(command.command.command_id)?;
            let is_available = if !lease_path.exists() {
                true
            } else {
                self.read_json::<ControlCommandLease>(&lease_path)?
                    .expires_ts
                    <= now
            };
            if is_available {
                available.push(command);
            }
        }
        Ok(available)
    }

    pub fn claim(
        &self,
        command_id: u64,
        owner: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<ControlCommandLease, StorageError> {
        if command_id == 0 || owner.trim().is_empty() || lease_seconds == 0 {
            return Err(StorageError::Conflict(
                "command_id、worker 和租约时长不能为空".into(),
            ));
        }
        self.ensure_dirs()?;
        let _claim_lock = self.acquire_claim_lock(command_id)?;
        let command_path = self.command_path(command_id)?;
        if !command_path.exists() {
            return Err(StorageError::NotFound(format!("command_id {command_id}")));
        }
        let lease_path = self.lease_path(command_id)?;
        let mut fencing_token = 1;
        if lease_path.exists() {
            let current: ControlCommandLease = self.read_json(&lease_path)?;
            if current.expires_ts > now && current.owner != owner {
                return Err(StorageError::LeaseHeld {
                    run_id: command_id,
                    owner: current.owner,
                });
            }
            if current.expires_ts <= now {
                fencing_token = current.fencing_token.saturating_add(1).max(1);
                std::fs::remove_file(&lease_path)
                    .map_err(|error| StorageError::Io(error.to_string()))?;
            } else if current.owner == owner {
                let lease = ControlCommandLease {
                    command_id,
                    owner: owner.into(),
                    expires_ts: now.saturating_add(lease_seconds),
                    fencing_token: current.fencing_token.max(1),
                };
                write_atomic_path(
                    &lease_path,
                    &self.root,
                    &serde_json::to_string(&lease)
                        .map_err(|error| StorageError::Io(error.to_string()))?,
                )?;
                return Ok(lease);
            }
        }
        let lease = ControlCommandLease {
            command_id,
            owner: owner.into(),
            expires_ts: now.saturating_add(lease_seconds),
            fencing_token,
        };
        let mut file = std::fs::OpenOptions::new();
        file.write(true).create_new(true);
        let mut handle = file.open(&lease_path).map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists => StorageError::LeaseHeld {
                run_id: command_id,
                owner: "concurrent-worker".into(),
            },
            _ => StorageError::Io(error.to_string()),
        })?;
        let content =
            serde_json::to_string(&lease).map_err(|error| StorageError::Io(error.to_string()))?;
        handle
            .write_all(content.as_bytes())
            .map_err(|error| StorageError::Io(error.to_string()))?;
        handle
            .sync_all()
            .map_err(|error| StorageError::Io(error.to_string()))?;
        Ok(lease)
    }

    pub fn ack_at(
        &self,
        command_id: u64,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<PathBuf, StorageError> {
        let _claim_lock = self.acquire_claim_lock(command_id)?;
        let lease_path = self.lease_path(command_id)?;
        let lease: ControlCommandLease = self.read_json(&lease_path)?;
        if lease.owner != owner || lease.fencing_token != fencing_token {
            return Err(StorageError::Unauthorized(format!(
                "command_id {command_id} 的租约不属于 worker {owner}"
            )));
        }
        if lease.expires_ts <= now {
            return Err(StorageError::LeaseExpired { run_id: command_id });
        }
        let command_path = self.command_path(command_id)?;
        std::fs::remove_file(&command_path).map_err(|error| StorageError::Io(error.to_string()))?;
        std::fs::remove_file(&lease_path).map_err(|error| StorageError::Io(error.to_string()))?;
        Ok(command_path)
    }

    fn ensure_dirs(&self) -> Result<(), StorageError> {
        std::fs::create_dir_all(self.root.join("commands"))
            .map_err(|error| StorageError::Io(error.to_string()))
    }

    fn command_path(&self, command_id: u64) -> Result<PathBuf, StorageError> {
        if command_id == 0 {
            return Err(StorageError::InvalidName("command_id 不能为 0".into()));
        }
        Ok(self
            .root
            .join("commands")
            .join(format!("{command_id}.json")))
    }

    fn lease_path(&self, command_id: u64) -> Result<PathBuf, StorageError> {
        Ok(self
            .root
            .join("commands")
            .join(format!("{command_id}.lease.json")))
    }

    fn acquire_claim_lock(&self, command_id: u64) -> Result<StorageLock, StorageError> {
        std::fs::create_dir_all(self.root.join("commands"))
            .map_err(|error| StorageError::Io(error.to_string()))?;
        acquire_storage_lock(
            self.root
                .join("commands")
                .join(format!("{command_id}.claim.lock")),
        )
    }

    fn read_json<T: for<'de> Deserialize<'de>>(&self, path: &Path) -> Result<T, StorageError> {
        let content =
            std::fs::read_to_string(path).map_err(|error| StorageError::Io(error.to_string()))?;
        serde_json::from_str(&content).map_err(|error| StorageError::Io(error.to_string()))
    }
}

impl ControlCommandQueueBackend for ControlCommandQueue {
    fn enqueue_command(
        &self,
        command: ControlCommand,
        enqueued_ts: u64,
    ) -> Result<PathBuf, StorageError> {
        self.enqueue(command, enqueued_ts)
    }

    fn available_commands(&self, now: u64) -> Result<Vec<QueuedControlCommand>, StorageError> {
        self.available(now)
    }

    fn claim_command(
        &self,
        command_id: u64,
        owner: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<ControlCommandLease, StorageError> {
        self.claim(command_id, owner, now, lease_seconds)
    }

    fn ack_command_at(
        &self,
        command_id: u64,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<PathBuf, StorageError> {
        self.ack_at(command_id, owner, fencing_token, now)
    }
}

fn default_fencing_token() -> u64 {
    1
}

/// 追加式持久化审计记录。
///
/// 每条记录都携带前一条记录摘要和自身摘要；恢复、查询和追加都会先验证整条链，
/// 因此文件被截断、重排或篡改时会显式失败，而不会返回看似完整的审计结果。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    pub sequence: u64,
    pub record: AuditRecord,
    pub previous_hash: u64,
    pub entry_hash: u64,
}

#[derive(Clone, Debug)]
pub struct AuditFileStore {
    root: PathBuf,
}

/// 审计持久化后端契约；数据库实现必须保持文件后端的链校验和游标语义。
pub trait AuditStore {
    fn append_record(&self, record: AuditRecord) -> Result<PathBuf, StorageError>;
    fn read_entries(&self) -> Result<Vec<AuditEntry>, StorageError>;
    fn query_command_entries(&self, command_id: u64) -> Result<Vec<AuditEntry>, StorageError>;
    fn entries_after(&self, sequence: u64) -> Result<Vec<AuditEntry>, StorageError>;
}

impl AuditFileStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 追加一条审计记录；重复写入完全相同的末尾记录是幂等的。
    pub fn append(&self, record: AuditRecord) -> Result<PathBuf, StorageError> {
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.root.join("audit.append.lock"))?;
        self.append_unlocked(record)
    }

    fn append_unlocked(&self, record: AuditRecord) -> Result<PathBuf, StorageError> {
        let mut entries = self.read()?;
        if entries.last().is_some_and(|entry| entry.record == record) {
            return Ok(self.path());
        }
        let previous_hash = entries.last().map_or(0, |entry| entry.entry_hash);
        let entry = AuditEntry {
            sequence: entries.len() as u64,
            previous_hash,
            entry_hash: audit_entry_hash(entries.len() as u64, previous_hash, &record),
            record,
        };
        entries.push(entry);
        let content = serde_json::to_string(&entries)
            .map_err(|error| StorageError::Io(format!("审计序列化失败: {error}")))?;
        write_atomic_path(&self.path(), &self.root, &content)?;
        Ok(self.path())
    }

    /// 将控制面当前审计尾部同步到持久化链；已存在的前缀必须逐条一致。
    pub fn sync_control(&self, plane: &ControlPlane) -> Result<usize, StorageError> {
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.root.join("audit.append.lock"))?;
        let existing = self.read()?;
        let records = plane.audit();
        if existing.len() > records.len()
            || existing
                .iter()
                .zip(records)
                .any(|(entry, record)| entry.record != *record)
        {
            return Err(StorageError::Conflict(
                "控制面审计与持久化审计前缀不一致".into(),
            ));
        }
        let mut appended = 0;
        for record in records.iter().skip(existing.len()) {
            self.append_unlocked(record.clone())?;
            appended += 1;
        }
        Ok(appended)
    }

    pub fn read(&self) -> Result<Vec<AuditEntry>, StorageError> {
        let path = self.path();
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        let entries: Vec<AuditEntry> = serde_json::from_str(&content)
            .map_err(|error| StorageError::Io(format!("审计 JSON 非法: {error}")))?;
        validate_audit_chain(&entries)?;
        Ok(entries)
    }

    pub fn query_command(&self, command_id: u64) -> Result<Vec<AuditEntry>, StorageError> {
        Ok(self
            .read()?
            .into_iter()
            .filter(|entry| entry.record.command_id == command_id)
            .collect())
    }

    pub fn after(&self, sequence: u64) -> Result<Vec<AuditEntry>, StorageError> {
        Ok(self
            .read()?
            .into_iter()
            .filter(|entry| entry.sequence > sequence)
            .collect())
    }

    fn path(&self) -> PathBuf {
        self.root.join("audit.json")
    }
}

struct StorageLock {
    path: PathBuf,
}

impl Drop for StorageLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn acquire_storage_lock(path: PathBuf) -> Result<StorageLock, StorageError> {
    // A short bounded retry turns normal concurrent writers into serialized
    // appends while still returning a visible conflict if a crashed process
    // leaves a stale lock behind.
    for _ in 0..100 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(StorageLock { path }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => return Err(StorageError::Io(error.to_string())),
        }
    }
    Err(StorageError::Conflict(
        "存储追加锁被占用，调用方应在恢复后重试".into(),
    ))
}

impl AuditStore for AuditFileStore {
    fn append_record(&self, record: AuditRecord) -> Result<PathBuf, StorageError> {
        self.append(record)
    }

    fn read_entries(&self) -> Result<Vec<AuditEntry>, StorageError> {
        self.read()
    }

    fn query_command_entries(&self, command_id: u64) -> Result<Vec<AuditEntry>, StorageError> {
        self.query_command(command_id)
    }

    fn entries_after(&self, sequence: u64) -> Result<Vec<AuditEntry>, StorageError> {
        self.after(sequence)
    }
}

fn audit_entry_hash(sequence: u64, previous_hash: u64, record: &AuditRecord) -> u64 {
    let mut hash = Fnv1a::new();
    hash.write_u64(sequence);
    hash.write_u64(previous_hash);
    hash.write_u64(record.command_id);
    hash.write_text(&record.request_id);
    hash.write_text(&record.operator_id);
    hash.write_u64(record.command_digest);
    hash.write_text(&format!("{:?}", record.status));
    hash.write_text(&record.result_code);
    hash.write_u64(record.ts);
    hash.finish()
}

fn validate_audit_chain(entries: &[AuditEntry]) -> Result<(), StorageError> {
    let mut previous_hash = 0;
    for (index, entry) in entries.iter().enumerate() {
        let sequence = index as u64;
        if entry.sequence != sequence || entry.previous_hash != previous_hash {
            return Err(StorageError::Conflict(format!(
                "审计链序号或前置摘要非法: expected {}",
                sequence
            )));
        }
        let expected = audit_entry_hash(sequence, previous_hash, &entry.record);
        if entry.entry_hash != expected {
            return Err(StorageError::Conflict(format!(
                "审计记录 {} 摘要不一致",
                sequence
            )));
        }
        previous_hash = entry.entry_hash;
    }
    Ok(())
}

/// 单机可恢复任务队列：用原子 JSON 文件模拟队列、租约和确认语义。
///
/// 该实现适合开发、单机 worker 和故障注入；生产多进程/多节点应替换为数据库或
/// 消息队列实现，但必须保持 `run_id` 幂等、租约过期接管和确认前不丢任务的语义。
#[derive(Clone, Debug)]
pub struct FileJobQueue {
    root: PathBuf,
}

struct ClaimLock {
    path: PathBuf,
}

impl Drop for ClaimLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 可替换任务队列契约。数据库/消息队列实现必须保留幂等键、租约、过期接管和确认语义。
pub trait JobQueueBackend {
    fn enqueue_job(
        &self,
        job: JobSpec,
        run: JobRun,
        enqueued_ts: u64,
    ) -> Result<PathBuf, StorageError>;
    fn available_jobs(&self, now: u64) -> Result<Vec<QueuedJob>, StorageError>;
    fn claim_job(
        &self,
        run_id: u64,
        worker: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<JobLease, StorageError>;
    fn ack_job(&self, run_id: u64, worker: &str) -> Result<PathBuf, StorageError>;
    fn ack_job_at(
        &self,
        run_id: u64,
        worker: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<PathBuf, StorageError>;
    fn recover_expired_leases(&self, now: u64) -> Result<Vec<u64>, StorageError>;
}

impl FileJobQueue {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn enqueue(
        &self,
        job: JobSpec,
        run: JobRun,
        enqueued_ts: u64,
    ) -> Result<PathBuf, StorageError> {
        let envelope = QueuedJob {
            job,
            run,
            enqueued_ts,
        };
        envelope.validate()?;
        let path = self.queue_path(envelope.run.run_id);
        if path.exists() {
            let existing = self.read_json::<QueuedJob>(&path)?;
            if existing.job == envelope.job && existing.run == envelope.run {
                return Ok(path);
            }
            return Err(StorageError::Conflict(format!(
                "run_id {} 已被不同任务占用",
                envelope.run.run_id
            )));
        }
        self.ensure_dirs()?;
        self.write_atomic(
            &path,
            &serde_json::to_string(&envelope)
                .map_err(|error| StorageError::Io(format!("任务序列化失败: {error}")))?,
        )?;
        Ok(path)
    }

    pub fn pending(&self) -> Result<Vec<QueuedJob>, StorageError> {
        let mut jobs = Vec::new();
        let dir = self.root.join("queue");
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(jobs),
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        for entry in entries {
            let path = entry
                .map_err(|error| StorageError::Io(error.to_string()))?
                .path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                let job: QueuedJob = self.read_json(&path)?;
                job.validate()?;
                jobs.push(job);
            }
        }
        jobs.sort_by_key(|job: &QueuedJob| (job.run.trading_day.clone(), job.run.run_id));
        Ok(jobs)
    }

    pub fn available(&self, now: u64) -> Result<Vec<QueuedJob>, StorageError> {
        let mut jobs = Vec::new();
        for job in self.pending()? {
            let lease_path = self.lease_path(job.run.run_id);
            let available = if !lease_path.exists() {
                true
            } else {
                self.read_json::<JobLease>(&lease_path)?.expires_ts <= now
            };
            if available {
                jobs.push(job);
            }
        }
        Ok(jobs)
    }

    pub fn claim(
        &self,
        run_id: u64,
        worker: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<JobLease, StorageError> {
        if worker.trim().is_empty() || lease_seconds == 0 {
            return Err(StorageError::Conflict("worker 和租约时长不能为空".into()));
        }
        let queue_path = self.queue_path(run_id);
        self.ensure_dirs()?;
        let _claim_lock = self.acquire_claim_lock(run_id)?;
        if !queue_path.exists() {
            return Err(StorageError::NotFound(format!("run_id {run_id}")));
        }
        let queued: QueuedJob = self.read_json(&queue_path)?;
        queued.validate()?;
        let lease_path = self.lease_path(run_id);
        let mut fencing_token = 1;
        if lease_path.exists() {
            let current: JobLease = self.read_json(&lease_path)?;
            if current.expires_ts > now && current.owner != worker {
                return Err(StorageError::LeaseHeld {
                    run_id,
                    owner: current.owner,
                });
            }
            if current.expires_ts <= now {
                fencing_token = current.fencing_token.saturating_add(1).max(1);
                std::fs::remove_file(&lease_path)
                    .map_err(|error| StorageError::Io(error.to_string()))?;
            } else if current.owner == worker {
                let lease = JobLease {
                    run_id,
                    owner: worker.into(),
                    expires_ts: now.saturating_add(lease_seconds),
                    fencing_token: current.fencing_token.max(1),
                };
                self.write_atomic(
                    &lease_path,
                    &serde_json::to_string(&lease)
                        .map_err(|error| StorageError::Io(error.to_string()))?,
                )?;
                return Ok(lease);
            }
        }
        let lease = JobLease {
            run_id,
            owner: worker.into(),
            expires_ts: now.saturating_add(lease_seconds),
            fencing_token,
        };
        let mut file = std::fs::OpenOptions::new();
        file.write(true).create_new(true);
        let mut handle = file.open(&lease_path).map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists => StorageError::LeaseHeld {
                run_id,
                owner: "concurrent-worker".into(),
            },
            _ => StorageError::Io(error.to_string()),
        })?;
        let content =
            serde_json::to_string(&lease).map_err(|error| StorageError::Io(error.to_string()))?;
        handle
            .write_all(content.as_bytes())
            .map_err(|error| StorageError::Io(error.to_string()))?;
        handle
            .sync_all()
            .map_err(|error| StorageError::Io(error.to_string()))?;
        Ok(lease)
    }

    pub fn ack(&self, run_id: u64, worker: &str) -> Result<PathBuf, StorageError> {
        let lease_path = self.lease_path(run_id);
        let lease: JobLease = self.read_json(&lease_path)?;
        if lease.owner != worker {
            return Err(StorageError::Unauthorized(format!(
                "worker {} 不能确认 worker {} 的任务",
                worker, lease.owner
            )));
        }
        self.ack_files(run_id, worker, lease.fencing_token)
    }

    /// 严格确认路径：校验 worker、fencing token 和逻辑时间，拒绝过期租约。
    pub fn ack_at(
        &self,
        run_id: u64,
        worker: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<PathBuf, StorageError> {
        let lease_path = self.lease_path(run_id);
        let lease: JobLease = self.read_json(&lease_path)?;
        if lease.expires_ts <= now {
            return Err(StorageError::LeaseExpired { run_id });
        }
        if lease.owner != worker || lease.fencing_token != fencing_token {
            return Err(StorageError::Unauthorized(format!(
                "worker {} 的租约 fencing token 无效",
                worker
            )));
        }
        self.ack_files(run_id, worker, fencing_token)
    }

    fn ack_files(
        &self,
        run_id: u64,
        worker: &str,
        fencing_token: u64,
    ) -> Result<PathBuf, StorageError> {
        let lease_path = self.lease_path(run_id);
        let lease: JobLease = self.read_json(&lease_path)?;
        if lease.owner != worker || lease.fencing_token != fencing_token {
            return Err(StorageError::Unauthorized(format!(
                "worker {} 不能确认当前租约",
                worker
            )));
        }
        let source = self.queue_path(run_id);
        let target = self.done_path(run_id);
        self.ensure_dirs()?;
        let _claim_lock = self.acquire_claim_lock(run_id)?;
        let queued: QueuedJob = self.read_json(&source)?;
        queued.validate()?;
        if target.exists() {
            std::fs::remove_file(&source).ok();
        } else {
            std::fs::rename(&source, &target)
                .map_err(|error| StorageError::Io(error.to_string()))?;
        }
        std::fs::remove_file(lease_path).map_err(|error| StorageError::Io(error.to_string()))?;
        Ok(target)
    }

    /// 返回已过期任务；保留旧租约直到下一次 `claim`，以便递增 fencing token。
    pub fn recover_expired(&self, now: u64) -> Result<Vec<u64>, StorageError> {
        let mut recovered = Vec::new();
        let dir = self.root.join("leases");
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(recovered),
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        for entry in entries {
            let path = entry
                .map_err(|error| StorageError::Io(error.to_string()))?
                .path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let lease: JobLease = self.read_json(&path)?;
            if lease.expires_ts <= now {
                recovered.push(lease.run_id);
            }
        }
        recovered.sort_unstable();
        Ok(recovered)
    }

    fn ensure_dirs(&self) -> Result<(), StorageError> {
        for dir in ["queue", "leases", "done", "locks"] {
            std::fs::create_dir_all(self.root.join(dir))
                .map_err(|error| StorageError::Io(error.to_string()))?;
        }
        Ok(())
    }

    fn queue_path(&self, run_id: u64) -> PathBuf {
        self.root.join("queue").join(format!("{run_id}.json"))
    }

    fn lease_path(&self, run_id: u64) -> PathBuf {
        self.root.join("leases").join(format!("{run_id}.json"))
    }

    fn acquire_claim_lock(&self, run_id: u64) -> Result<ClaimLock, StorageError> {
        let path = self.root.join("locks").join(format!("{run_id}.lock"));
        let result = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        match result {
            Ok(_) => Ok(ClaimLock { path }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(StorageError::LeaseHeld {
                    run_id,
                    owner: "concurrent-lease-operation".into(),
                })
            }
            Err(error) => Err(StorageError::Io(error.to_string())),
        }
    }

    fn done_path(&self, run_id: u64) -> PathBuf {
        self.root.join("done").join(format!("{run_id}.json"))
    }

    fn read_json<T: for<'de> Deserialize<'de>>(&self, path: &Path) -> Result<T, StorageError> {
        let text =
            std::fs::read_to_string(path).map_err(|error| StorageError::Io(error.to_string()))?;
        serde_json::from_str(&text).map_err(|error| StorageError::Io(error.to_string()))
    }

    fn write_atomic(&self, path: &Path, content: &str) -> Result<(), StorageError> {
        write_atomic_path(path, &self.root, content)
    }
}

impl JobQueueBackend for FileJobQueue {
    fn enqueue_job(
        &self,
        job: JobSpec,
        run: JobRun,
        enqueued_ts: u64,
    ) -> Result<PathBuf, StorageError> {
        self.enqueue(job, run, enqueued_ts)
    }

    fn available_jobs(&self, now: u64) -> Result<Vec<QueuedJob>, StorageError> {
        self.available(now)
    }

    fn claim_job(
        &self,
        run_id: u64,
        worker: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<JobLease, StorageError> {
        self.claim(run_id, worker, now, lease_seconds)
    }

    fn ack_job(&self, run_id: u64, worker: &str) -> Result<PathBuf, StorageError> {
        self.ack(run_id, worker)
    }

    fn ack_job_at(
        &self,
        run_id: u64,
        worker: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<PathBuf, StorageError> {
        self.ack_at(run_id, worker, fencing_token, now)
    }

    fn recover_expired_leases(&self, now: u64) -> Result<Vec<u64>, StorageError> {
        self.recover_expired(now)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TokenBucketState {
    tokens: u64,
    last_ts: u64,
}

/// 共享文件系统上的持久化令牌桶。
///
/// 这是 API/worker 在没有外部缓存时的跨进程限流后端；它使用同一套原子锁和
/// 临时文件替换语义。高可用集群仍应接入具备事务/租约能力的外部存储。
#[derive(Clone, Debug)]
pub struct FileTokenBucket {
    root: PathBuf,
    name: String,
    capacity: u64,
    refill_per_second: u64,
}

impl FileTokenBucket {
    pub fn new(
        root: impl Into<PathBuf>,
        name: impl Into<String>,
        capacity: u64,
        refill_per_second: u64,
    ) -> Result<Self, StorageError> {
        let name = name.into();
        if name.trim().is_empty()
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            || capacity == 0
        {
            return Err(StorageError::InvalidName(name));
        }
        Ok(Self {
            root: root.into(),
            name,
            capacity,
            refill_per_second,
        })
    }

    pub fn try_acquire(&self, now: u64, weight: u64) -> Result<bool, StorageError> {
        if weight == 0 || weight > self.capacity {
            return Err(StorageError::Conflict(
                "令牌桶请求权重必须在 1..=capacity 内".into(),
            ));
        }
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::Io(error.to_string()))?;
        let lock = self.root.join(format!("{}.lock", self.name));
        let _guard = acquire_storage_lock(lock)?;
        let path = self.root.join(format!("{}.json", self.name));
        let mut state = match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str::<TokenBucketState>(&content)
                .map_err(|error| StorageError::Io(format!("令牌桶状态非法: {error}")))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => TokenBucketState {
                tokens: self.capacity,
                last_ts: now,
            },
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        if state.tokens > self.capacity {
            return Err(StorageError::Conflict("令牌桶余额超过容量".into()));
        }
        let elapsed = now.saturating_sub(state.last_ts);
        state.tokens = state
            .tokens
            .saturating_add(elapsed.saturating_mul(self.refill_per_second))
            .min(self.capacity);
        state.last_ts = now;
        let granted = state.tokens >= weight;
        if granted {
            state.tokens -= weight;
        }
        let content = serde_json::to_string(&state)
            .map_err(|error| StorageError::Io(format!("令牌桶序列化失败: {error}")))?;
        write_atomic_path(&path, &self.root, &content)?;
        Ok(granted)
    }
}

#[derive(Clone, Debug)]
pub struct JsonStateStore {
    root: PathBuf,
}

impl JsonStateStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 保存一个受路径约束、原子替换的 JSON 状态文件。
    ///
    /// 运行时报告、对账结果等非 Kernel 事实可以复用这个边界；调用方仍应
    /// 把真正的交易事实写入 EventLog，不能用状态文件替代事件追加。
    pub fn save_json_at<T: Serialize>(
        &self,
        relative_path: impl AsRef<Path>,
        value: &T,
    ) -> Result<PathBuf, StorageError> {
        let path = self.state_path(relative_path)?;
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.root.join(".json-state.write.lock"))?;
        let content = serde_json::to_string_pretty(value)
            .map_err(|error| StorageError::Io(format!("JSON 状态序列化失败: {error}")))?;
        write_atomic_path(&path, &self.root, &content)?;
        Ok(path)
    }

    pub fn load_json_at<T: DeserializeOwned>(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<T, StorageError> {
        let path = self.state_path(relative_path)?;
        let content =
            std::fs::read_to_string(path).map_err(|error| StorageError::Io(error.to_string()))?;
        serde_json::from_str(&content)
            .map_err(|error| StorageError::Io(format!("JSON 状态解析失败: {error}")))
    }

    pub fn save_control(&self, plane: &ControlPlane) -> Result<PathBuf, StorageError> {
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.root.join(".control-plane.write.lock"))?;
        self.save_control_unlocked(plane)
    }

    /// 在同一把文件锁内读取、修改并原子保存控制面状态。
    ///
    /// API 接收命令和执行器回写终态都必须通过这个事务边界，避免两个进程
    /// 分别基于旧快照保存而互相覆盖命令或审计尾部。
    pub fn update_control<T, F>(&self, update: F) -> Result<(ControlPlane, T), StorageError>
    where
        F: FnOnce(&mut ControlPlane) -> Result<T, String>,
    {
        let (plane, result) = self.transact_control(update)?;
        result.map(|value| (plane, value)).map_err(StorageError::Io)
    }

    /// 控制面事务的保留错误类型版本。业务拒绝（重复请求/权限不足等）不会
    /// 被误包装成存储故障，也不会写入半成品状态；只有成功变更才会原子保存。
    pub fn transact_control<T, E, F>(
        &self,
        update: F,
    ) -> Result<(ControlPlane, Result<T, E>), StorageError>
    where
        F: FnOnce(&mut ControlPlane) -> Result<T, E>,
    {
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.root.join(".control-plane.write.lock"))?;
        let path = self.root.join("control-plane.json");
        let mut plane = match std::fs::read_to_string(&path) {
            Ok(content) => ControlPlane::from_json(&content).map_err(StorageError::Io)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ControlPlane::default(),
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        let result = update(&mut plane);
        if result.is_ok() {
            self.save_control_unlocked(&plane)?;
        }
        Ok((plane, result))
    }

    pub fn load_control(&self) -> Result<ControlPlane, StorageError> {
        let text = self.load_text("control-plane")?;
        ControlPlane::from_json(&text).map_err(StorageError::Io)
    }

    /// 加载可选的控制面状态；首次启动没有文件时返回空控制面。
    pub fn load_control_if_exists(&self) -> Result<Option<ControlPlane>, StorageError> {
        let path = self.root.join("control-plane.json");
        match std::fs::read_to_string(path) {
            Ok(text) => ControlPlane::from_json(&text)
                .map(Some)
                .map_err(StorageError::Io),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(StorageError::Io(error.to_string())),
        }
    }

    pub fn save_scheduler(&self, scheduler: &Scheduler) -> Result<PathBuf, StorageError> {
        self.save_scheduler_at("scheduler.json", scheduler)
    }

    pub fn load_scheduler(&self) -> Result<Scheduler, StorageError> {
        self.load_scheduler_at("scheduler.json")
    }

    /// 将调度状态保存到 data root 下的显式相对路径；路径越界会被拒绝。
    /// 这让多个运行拓扑可以在同一个 data root 中拥有独立的调度状态文件。
    pub fn save_scheduler_at(
        &self,
        relative_path: impl AsRef<Path>,
        scheduler: &Scheduler,
    ) -> Result<PathBuf, StorageError> {
        let path = self.state_path(relative_path)?;
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.root.join(".scheduler.write.lock"))?;
        let content = scheduler.to_json().map_err(StorageError::Io)?;
        write_atomic_path(&path, &self.root, &content)?;
        Ok(path)
    }

    pub fn load_scheduler_at(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<Scheduler, StorageError> {
        let path = self.state_path(relative_path)?;
        let text =
            std::fs::read_to_string(path).map_err(|error| StorageError::Io(error.to_string()))?;
        Scheduler::from_json(&text).map_err(StorageError::Io)
    }

    /// 在调度状态文件锁内读取、修改并原子保存；用于 Scheduler 与 Strategy
    /// 进程对同一 JobRun 的异步状态推进，避免旧内存快照覆盖新终态。
    pub fn transact_scheduler_at<T, E, F>(
        &self,
        relative_path: impl AsRef<Path>,
        update: F,
    ) -> Result<(Scheduler, Result<T, E>), StorageError>
    where
        F: FnOnce(&mut Scheduler) -> Result<T, E>,
    {
        let path = self.state_path(relative_path)?;
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.root.join(".scheduler.write.lock"))?;
        let mut scheduler = match std::fs::read_to_string(&path) {
            Ok(content) => Scheduler::from_json(&content).map_err(StorageError::Io)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Scheduler::default(),
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        let result = update(&mut scheduler);
        if result.is_ok() {
            let content = scheduler.to_json().map_err(StorageError::Io)?;
            write_atomic_path(&path, &self.root, &content)?;
        }
        Ok((scheduler, result))
    }

    fn state_path(&self, relative_path: impl AsRef<Path>) -> Result<PathBuf, StorageError> {
        let relative_path = relative_path.as_ref();
        if relative_path.as_os_str().is_empty() {
            return Err(StorageError::InvalidName("调度状态路径必须非空".into()));
        }
        if relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(StorageError::InvalidName(
                "调度状态路径不能包含父目录".into(),
            ));
        }
        let path = if relative_path.is_absolute() {
            relative_path.to_path_buf()
        } else {
            self.root.join(relative_path)
        };
        if !path.starts_with(&self.root) {
            return Err(StorageError::InvalidName(
                "调度状态路径越出 data root".into(),
            ));
        }
        Ok(path)
    }

    fn save_text(
        &self,
        name: &str,
        text: Result<String, StorageError>,
    ) -> Result<PathBuf, StorageError> {
        let path = self.root.join(format!("{name}.json"));
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::Io(error.to_string()))?;
        let temp = self.root.join(format!(
            ".{name}.json.tmp.{}",
            TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&temp, text?).map_err(|error| StorageError::Io(error.to_string()))?;
        sync_file(&temp)?;
        std::fs::rename(&temp, &path).map_err(|error| StorageError::Io(error.to_string()))?;
        Ok(path)
    }

    fn save_control_unlocked(&self, plane: &ControlPlane) -> Result<PathBuf, StorageError> {
        self.save_text("control-plane", plane.to_json().map_err(StorageError::Io))
    }

    fn load_text(&self, name: &str) -> Result<String, StorageError> {
        std::fs::read_to_string(self.root.join(format!("{name}.json")))
            .map_err(|error| StorageError::Io(error.to_string()))
    }
}

fn sync_file(path: &Path) -> Result<(), StorageError> {
    #[cfg(not(windows))]
    {
        std::fs::File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| StorageError::Io(error.to_string()))?;
    }
    #[cfg(windows)]
    {
        // Windows antivirus/indexer hooks can reject fsync on a freshly created
        // temp file; rename still provides the crash-safe visibility boundary.
        let _ = path;
    }
    Ok(())
}

fn write_atomic_path(path: &Path, root: &Path, content: &str) -> Result<(), StorageError> {
    let parent = path
        .parent()
        .ok_or_else(|| StorageError::Io("存储路径没有父目录".into()))?;
    if !parent.starts_with(root) {
        return Err(StorageError::InvalidName("存储路径越出根目录".into()));
    }
    std::fs::create_dir_all(parent).map_err(|error| StorageError::Io(error.to_string()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| StorageError::Io("存储文件名非法".into()))?;
    let temp = parent.join(format!(
        ".{name}.tmp.{}",
        TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&temp, content).map_err(|error| StorageError::Io(error.to_string()))?;
    sync_file(&temp)?;
    std::fs::rename(&temp, path).map_err(|error| StorageError::Io(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_control::{CommandKind, ControlCommand, Permission};
    use qx_core::{Event, EventKind, Priority};
    use qx_scheduler::{JobSpec, JobWindow, RetryPolicy, Trigger};
    use std::collections::BTreeMap;

    #[test]
    fn file_store_round_trips_and_rejects_path_escape() {
        let root = std::env::temp_dir().join(format!("qianxing-storage-{}", std::process::id()));
        let store = EventLogFileStore::new(&root);
        let mut log = EventLog::new();
        let seq = log.alloc_seq();
        log.append(Event::new(seq, 1, Priority::POST, EventKind::Settle));
        store.write("run-1", &log).unwrap();
        let restored = store.read("run-1").unwrap();
        assert_eq!(restored.digest(), log.digest());
        let mut extended = log.clone();
        let seq = extended.alloc_seq();
        extended.append(Event::new(seq, 2, Priority::POST, EventKind::Settle));
        store.write("run-1", &extended).unwrap();
        assert!(matches!(
            store.write("run-1", &log),
            Err(StorageError::NonAppendOnly(_))
        ));
        assert!(matches!(
            store.read("../escape"),
            Err(StorageError::InvalidName(_))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn segmented_event_store_is_append_only_and_manifest_verified() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-segmented-storage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = SegmentedEventLogStore::new(&root, 2).unwrap();
        let mut log = EventLog::new();
        for ts in 1..=3 {
            let seq = log.alloc_seq();
            log.append(Event::new(seq, ts, Priority::POST, EventKind::Settle));
        }
        store.write("run", &log).unwrap();
        assert_eq!(store.read("run").unwrap().digest(), log.digest());

        let mut extended = log.clone();
        let seq = extended.alloc_seq();
        extended.append(Event::new(seq, 4, Priority::POST, EventKind::Settle));
        store.write("run", &extended).unwrap();
        assert_eq!(store.read("run").unwrap().len(), 4);
        assert!(matches!(
            store.write("run", &log),
            Err(StorageError::NonAppendOnly(_))
        ));

        std::fs::write(
            root.join("segments").join("run-0000000000000000.jsonl"),
            "{}\n",
        )
        .unwrap();
        let corrupted = store.read("run");
        assert!(corrupted.is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn event_log_store_does_not_silently_overwrite_concurrent_extensions() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-event-concurrent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = EventLogFileStore::new(&root);
        let mut base = EventLog::new();
        let seq = base.alloc_seq();
        base.append(Event::new(seq, 1, Priority::POST, EventKind::Settle));
        store.write("run", &base).unwrap();

        let mut left = base.clone();
        let left_seq = left.alloc_seq();
        left.append(Event::new(left_seq, 2, Priority::POST, EventKind::Settle));
        let mut right = base;
        let right_seq = right.alloc_seq();
        right.append(Event::new(right_seq, 3, Priority::POST, EventKind::Settle));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let first_store = store.clone();
        let first_barrier = barrier.clone();
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_store.write("run", &left)
        });
        let second_store = store.clone();
        let second_barrier = barrier.clone();
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_store.write("run", &right)
        });
        let outcomes = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(result, Err(StorageError::NonAppendOnly(_))))
                .count(),
            1
        );
        assert_eq!(store.read("run").unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn control_state_round_trips_after_restart() {
        let root = std::env::temp_dir().join(format!("qianxing-state-{}", std::process::id()));
        let store = JsonStateStore::new(&root);
        let mut plane = ControlPlane::default();
        plane
            .submit(
                ControlCommand {
                    command_id: 1,
                    request_id: "r1".into(),
                    operator_id: "ops".into(),
                    reason: "test".into(),
                    kind: CommandKind::PauseStrategy,
                    target: "s1".into(),
                    payload: BTreeMap::new(),
                    permission: Permission::Trading,
                    dry_run: true,
                },
                1,
            )
            .unwrap();
        store.save_control(&plane).unwrap();
        let restored = store.load_control().unwrap();
        assert_eq!(restored.command(1).unwrap().request_id, "r1");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn json_state_store_round_trips_nested_report_atomically() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-json-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = JsonStateStore::new(&root);
        let value = serde_json::json!({"status":"degraded","issues":[{"asset":"USDT"}]});
        store.save_json_at("reconcile/main.json", &value).unwrap();
        let restored: serde_json::Value = store.load_json_at("reconcile/main.json").unwrap();
        assert_eq!(restored, value);
        assert!(matches!(
            store.load_json_at::<serde_json::Value>("../escape.json"),
            Err(StorageError::InvalidName(_))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn scheduler_state_round_trips_after_restart() {
        let root =
            std::env::temp_dir().join(format!("qianxing-scheduler-state-{}", std::process::id()));
        let store = JsonStateStore::new(&root);
        let mut scheduler = Scheduler::default();
        scheduler
            .register(JobSpec {
                job_id: "bars".into(),
                job_version: "v1".into(),
                owner: "research".into(),
                enabled: true,
                trigger: Trigger::Cron("0 9 * * 1-5".into()),
                window: JobWindow::Session,
                depends_on: Vec::new(),
                input_refs: vec!["raw-bars".into()],
                output_refs: vec!["bars-v1".into()],
                timeout_seconds: 60,
                retry_policy: RetryPolicy::default(),
                concurrency_key: "bars".into(),
                idempotency_key: "bars-daily".into(),
                permission_scope: "research".into(),
                audit_reason: "scheduler persistence test".into(),
                dry_run: true,
            })
            .unwrap();
        store.save_scheduler(&scheduler).unwrap();
        let restored = store.load_scheduler().unwrap();
        assert_eq!(restored.job("bars").unwrap().job_version, "v1");
        let _ = std::fs::remove_dir_all(root);
    }

    fn queued_job() -> (JobSpec, JobRun) {
        let job = JobSpec {
            job_id: "queue-job".into(),
            job_version: "v1".into(),
            owner: "research".into(),
            enabled: true,
            trigger: Trigger::Manual,
            window: JobWindow::Any,
            depends_on: Vec::new(),
            input_refs: vec!["input".into()],
            output_refs: vec!["output".into()],
            timeout_seconds: 60,
            retry_policy: RetryPolicy::default(),
            concurrency_key: "queue-job".into(),
            idempotency_key: "queue-job-daily".into(),
            permission_scope: "research".into(),
            audit_reason: "queue test".into(),
            dry_run: true,
        };
        let run = JobRun {
            run_id: job.stable_key("20260910"),
            job_id: job.job_id.clone(),
            trading_day: "20260910".into(),
            attempt: 1,
            status: JobStatus::Running,
            manifest_digest: Some(7),
            error_code: None,
            next_retry_ts: None,
            started_ts: 10,
            deadline_ts: 70,
        };
        (job, run)
    }

    #[test]
    fn file_job_queue_is_idempotent_and_recoverable() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-job-queue-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let queue = FileJobQueue::new(&root);
        let (job, run) = queued_job();
        let run_id = run.run_id;
        let first = queue.enqueue(job.clone(), run.clone(), 10).unwrap();
        assert_eq!(queue.enqueue(job.clone(), run.clone(), 10).unwrap(), first);
        assert_eq!(queue.pending().unwrap().len(), 1);
        assert_eq!(queue.enqueue(job, run.clone(), 11).unwrap(), first);

        let lease = queue.claim(run_id, "worker-a", 10, 10).unwrap();
        assert_eq!(lease.expires_ts, 20);
        assert_eq!(lease.fencing_token, 1);
        assert!(queue.available(11).unwrap().is_empty());
        assert!(matches!(
            queue.ack_at(run_id, "worker-a", lease.fencing_token, 20),
            Err(StorageError::LeaseExpired { .. })
        ));
        assert!(matches!(
            queue.claim(run_id, "worker-b", 11, 10),
            Err(StorageError::LeaseHeld { .. })
        ));
        assert!(queue.recover_expired(19).unwrap().is_empty());
        assert_eq!(queue.recover_expired(20).unwrap(), vec![run_id]);
        assert_eq!(queue.available(20).unwrap().len(), 1);
        let takeover = queue.claim(run_id, "worker-b", 21, 10).unwrap();
        assert_eq!(takeover.fencing_token, 2);
        assert!(matches!(
            queue.ack(run_id, "worker-a"),
            Err(StorageError::Unauthorized(_))
        ));
        assert!(matches!(
            queue.ack_at(run_id, "worker-a", lease.fencing_token, 22),
            Err(StorageError::Unauthorized(_))
        ));
        let done = queue
            .ack_at(run_id, "worker-b", takeover.fencing_token, 21)
            .unwrap();
        assert!(done.exists());
        assert!(queue.pending().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    fn queued_control(command_id: u64) -> ControlCommand {
        ControlCommand {
            command_id,
            request_id: format!("queue-{command_id}"),
            operator_id: "ops".into(),
            reason: "queue integration test".into(),
            kind: CommandKind::SubmitOrder,
            target: format!("{command_id}"),
            payload: std::collections::BTreeMap::new(),
            permission: Permission::Trading,
            dry_run: true,
        }
    }

    #[test]
    fn control_command_queue_is_idempotent_and_fenced() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-control-queue-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let queue = ControlCommandQueue::new(&root);
        let command = queued_control(501);
        let path = queue.enqueue(command.clone(), 10).unwrap();
        assert_eq!(queue.enqueue(command, 10).unwrap(), path);
        assert_eq!(queue.pending().unwrap().len(), 1);
        let lease = queue.claim(501, "execution-a", 10, 10).unwrap();
        assert!(queue.available(11).unwrap().is_empty());
        assert!(matches!(
            queue.claim(501, "execution-b", 11, 10),
            Err(StorageError::LeaseHeld { .. })
        ));
        assert!(matches!(
            queue.ack_at(501, "execution-b", lease.fencing_token, 11),
            Err(StorageError::Unauthorized(_))
        ));
        assert_eq!(queue.available(20).unwrap().len(), 1);
        let takeover = queue.claim(501, "execution-b", 20, 10).unwrap();
        assert_eq!(takeover.fencing_token, 2);
        queue
            .ack_at(501, "execution-b", takeover.fencing_token, 21)
            .unwrap();
        assert!(queue.pending().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn control_state_update_is_atomic_and_restores() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-control-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = JsonStateStore::new(&root);
        let (_, accepted) = store
            .update_control(|plane| {
                plane
                    .submit(queued_control(601), 10)
                    .map_err(|error| format!("{error:?}"))
            })
            .unwrap();
        assert_eq!(accepted.status, qx_control::CommandStatus::Accepted);
        let (_, executed) = store
            .update_control(|plane| {
                plane
                    .execute(601, 11, |_| Ok("DRY_RUN_VALIDATED".into()))
                    .map_err(|error| format!("{error:?}"))
            })
            .unwrap();
        assert_eq!(executed.status, qx_control::CommandStatus::Executed);
        let restored = store.load_control().unwrap();
        assert_eq!(restored.audit().len(), 2);
        assert!(restored.pending().next().is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn audit_file_store_is_append_only_queryable_and_tamper_evident() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-audit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = AuditFileStore::new(&root);
        let mut plane = ControlPlane::default();
        let accepted = plane
            .submit(
                ControlCommand {
                    command_id: 77,
                    request_id: "audit-77".into(),
                    operator_id: "ops".into(),
                    reason: "audit persistence".into(),
                    kind: CommandKind::PauseStrategy,
                    target: "strategy".into(),
                    payload: BTreeMap::new(),
                    permission: Permission::Trading,
                    dry_run: true,
                },
                10,
            )
            .unwrap();
        store.append(accepted.clone()).unwrap();
        store.append(accepted).unwrap();
        plane.execute(77, 11, |_| Ok("DONE".into())).unwrap();
        assert_eq!(store.sync_control(&plane).unwrap(), 1);
        assert_eq!(store.read().unwrap().len(), 2);
        assert_eq!(store.query_command(77).unwrap().len(), 2);
        assert_eq!(store.after(0).unwrap().len(), 1);
        assert_eq!(AuditFileStore::new(&root).read().unwrap().len(), 2);

        let path = root.join("audit.json");
        let mut tampered = store.read().unwrap();
        tampered[0].entry_hash ^= 1;
        std::fs::write(&path, serde_json::to_string(&tampered).unwrap()).unwrap();
        assert!(matches!(
            store.read(),
            Err(StorageError::Conflict(message)) if message.contains("摘要不一致")
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn audit_file_store_serializes_concurrent_appends_without_losing_tail() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-audit-concurrent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = AuditFileStore::new(&root);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let first_store = store.clone();
        let first_barrier = barrier.clone();
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_store.append(AuditRecord {
                command_id: 1,
                request_id: "concurrent-1".into(),
                operator_id: "ops".into(),
                command_digest: 1,
                status: qx_control::CommandStatus::Accepted,
                result_code: "ACCEPTED".into(),
                ts: 1,
            })
        });
        let second_store = store.clone();
        let second_barrier = barrier.clone();
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_store.append(AuditRecord {
                command_id: 2,
                request_id: "concurrent-2".into(),
                operator_id: "ops".into(),
                command_digest: 2,
                status: qx_control::CommandStatus::Accepted,
                result_code: "ACCEPTED".into(),
                ts: 2,
            })
        });
        assert!(first.join().unwrap().is_ok());
        assert!(second.join().unwrap().is_ok());
        let entries = store.read().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].previous_hash, 0);
        assert_eq!(entries[1].previous_hash, entries[0].entry_hash);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn file_job_queue_revalidates_forged_payload_before_claim() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-job-forge-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let queue = FileJobQueue::new(&root);
        let (job, mut run) = queued_job();
        let run_id = run.run_id;
        queue.enqueue(job.clone(), run.clone(), 10).unwrap();
        run.status = JobStatus::Succeeded;
        std::fs::write(
            queue.queue_path(run_id),
            serde_json::to_string(&QueuedJob {
                job,
                run,
                enqueued_ts: 10,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            queue.claim(run_id, "worker", 10, 10),
            Err(StorageError::Conflict(_))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn file_job_queue_serializes_concurrent_claims_on_shared_filesystem() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-job-concurrent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let queue = FileJobQueue::new(&root);
        let (job, run) = queued_job();
        let run_id = run.run_id;
        queue.enqueue(job, run, 10).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let first_queue = queue.clone();
        let first_barrier = barrier.clone();
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_queue.claim(run_id, "worker-a", 10, 10)
        });
        let second_queue = queue.clone();
        let second_barrier = barrier;
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_queue.claim(run_id, "worker-b", 10, 10)
        });
        let outcomes = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(result, Err(StorageError::LeaseHeld { .. })))
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn file_outbox_is_idempotent_fenced_and_retryable() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-outbox-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = FileOutboxStore::new(&root);
        let event = OutboxEvent {
            event_id: "order-1".into(),
            topic: "order.events".into(),
            partition_key: "account-1".into(),
            sequence: 1,
            schema_version: 1,
            trace_id: "trace-1".into(),
            payload: "{\"status\":\"filled\"}".into(),
            created_ts: 10,
            attempts: 0,
        };
        store.append(event.clone()).unwrap();
        store.append(event).unwrap();
        let first = store.claim("order-1", "relay-a", 10, 5).unwrap();
        assert!(matches!(
            store.claim("order-1", "relay-b", 11, 5),
            Err(StorageError::LeaseHeld { .. })
        ));
        assert!(matches!(
            store.ack("order-1", "relay-a", first.fencing_token, 16),
            Err(StorageError::LeaseExpired { .. })
        ));
        let second = store.claim("order-1", "relay-b", 16, 5).unwrap();
        assert_eq!(second.fencing_token, first.fencing_token + 1);
        store
            .retry("order-1", "relay-b", second.fencing_token, 17)
            .unwrap();
        assert_eq!(store.available(17).unwrap()[0].attempts, 1);
        let third = store.claim("order-1", "relay-a", 17, 5).unwrap();
        store
            .ack("order-1", "relay-a", third.fencing_token, 18)
            .unwrap();
        assert!(store.available(18).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn outbox_relay_publishes_then_acknowledges_and_retries_failures() {
        struct Publisher {
            fail: bool,
            published: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        }

        impl OutboxPublisher for Publisher {
            fn publish(&self, event: &OutboxEvent) -> Result<(), String> {
                if self.fail {
                    return Err("test publisher unavailable".into());
                }
                self.published.lock().unwrap().push(event.event_id.clone());
                Ok(())
            }
        }

        let root = std::env::temp_dir().join(format!(
            "qianxing-outbox-relay-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = FileOutboxStore::new(&root);
        store
            .append(OutboxEvent {
                event_id: "relay-event".into(),
                topic: "qx.events".into(),
                partition_key: "account".into(),
                sequence: 1,
                schema_version: 1,
                trace_id: String::new(),
                payload: "{}".into(),
                created_ts: 1,
                attempts: 0,
            })
            .unwrap();
        let published = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let failing = OutboxRelay::new(
            store.clone(),
            Publisher {
                fail: true,
                published: published.clone(),
            },
            "relay",
            10,
        )
        .unwrap();
        assert_eq!(failing.pump_once(1, 10).unwrap().retried, 1);
        assert_eq!(store.available(1).unwrap()[0].attempts, 1);
        let working = OutboxRelay::new(
            store.clone(),
            Publisher {
                fail: false,
                published,
            },
            "relay",
            10,
        )
        .unwrap();
        let report = working.pump_once(2, 10).unwrap();
        assert_eq!(report.published, 1);
        assert!(store.available(2).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn file_consumer_checkpoint_is_idempotent_and_dead_letters_after_retries() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-consumer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = FileConsumerStateStore::new(&root);
        let engine = ConsumerEngine::new(store.clone(), "ledger-reducer", 2).unwrap();
        let event = OutboxEvent {
            event_id: "consumer-event-1".into(),
            topic: "qx.eventlog".into(),
            partition_key: "account-1".into(),
            sequence: 1,
            schema_version: 1,
            trace_id: String::new(),
            payload: "{}".into(),
            created_ts: 10,
            attempts: 0,
        };
        assert_eq!(
            engine.consume(&event, 5, 1, 10, |_| Ok(())).unwrap(),
            ConsumerOutcome::Applied
        );
        assert_eq!(
            engine
                .consume(&event, 5, 1, 11, |_| panic!(
                    "duplicate must not invoke handler"
                ))
                .unwrap(),
            ConsumerOutcome::Duplicate
        );
        let failed = OutboxEvent {
            event_id: "consumer-event-2".into(),
            sequence: 2,
            ..event
        };
        assert!(matches!(
            engine
                .consume(&failed, 6, 1, 12, |_| Err("temporary".into()))
                .unwrap(),
            ConsumerOutcome::Retried { .. }
        ));
        assert_eq!(
            engine
                .consume(&failed, 6, 2, 13, |_| Err("permanent".into()))
                .unwrap(),
            ConsumerOutcome::DeadLettered
        );
        assert_eq!(store.dead_letters("ledger-reducer", 10).unwrap().len(), 1);
        assert_eq!(
            store
                .load_checkpoint("ledger-reducer", "qx.eventlog", "account-1")
                .unwrap()
                .unwrap()
                .offset,
            6
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn file_token_bucket_is_persistent_and_serializes_concurrent_consumers() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-rate-limit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bucket = FileTokenBucket::new(&root, "api", 1, 0).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let first_bucket = bucket.clone();
        let first_barrier = barrier.clone();
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_bucket.try_acquire(10, 1).unwrap()
        });
        let second_bucket = bucket.clone();
        let second_barrier = barrier.clone();
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_bucket.try_acquire(10, 1).unwrap()
        });
        let granted = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(granted.iter().filter(|value| **value).count(), 1);
        assert!(!bucket.try_acquire(10, 1).unwrap());
        let replenished = FileTokenBucket::new(&root, "api", 1, 1).unwrap();
        assert!(replenished.try_acquire(11, 1).unwrap());
        let _ = std::fs::remove_dir_all(root);
    }
}
