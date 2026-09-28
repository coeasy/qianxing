//! Kernel 之外的可恢复文件存储。
//!
//! 文件名由调用方提供，但内容必须先经过 EventLog 的序号、时间和 JSON 校验；写入
//! 使用临时文件+rename，避免进程中断留下半个事实日志。

use qx_control::{AuditRecord, ControlCommand, ControlPlane};
use qx_core::retry;
use qx_core::{Event, EventLog, FileLock, Fnv1a, LockError, QxError, QxResult};
use qx_scheduler::{JobRun, JobSpec, JobStatus, Scheduler};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;

mod state_envelope;
pub use state_envelope::JsonStateEnvelope;
use state_envelope::{
    acquire_storage_lock, encode_state_json, read_json_file, read_state_json,
    read_state_json_or_default, read_state_text, read_state_text_required, transact_state_json,
    write_atomic_path, write_json_file, write_state_json, write_state_text, Commit,
};

// 五个文件后端存储的唯一定义点在 `file` 目录模块；crate 根只负责再导出，
// 保持 `qx_storage::{AuditFileStore, FileConsumerStateStore, FileOutboxStore,
// FileJobQueue, JsonStateStore}` 公开路径逐字不变（其它 crate 依赖该路径）。
mod file;
pub use file::{
    AuditFileStore, FileConsumerStateStore, FileJobQueue, FileOutboxStore, JsonStateStore,
};

#[cfg(feature = "sqlite")]
mod sqlite;
#[cfg(feature = "sqlite")]
pub use sqlite::{
    SqliteAuditStore, SqliteConsumerStateStore, SqliteControlCommandQueue, SqliteControlStore,
    SqliteEventLogStore, SqliteJobQueue, SqliteOutboxStore, SqliteSnapshotStore, SqliteTokenBucket,
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
        // 崩溃安全的临时文件+rename 一律复用 crate 内唯一的原子替换 helper。
        write_atomic_path(&path, &self.root, &content)?;
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
    /// 当前唯一在写的 Outbox schema 版本。更高的版本意味着“未来的写入者”，
    /// 三个后端（文件 / SQLite / PostgreSQL）必须在同一处拒绝，故校验放在这里，
    /// 文件信封与数据库后端都复用本函数（P1c §4.9）。
    pub const LATEST_SCHEMA_VERSION: u32 = 1;

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
        if self.schema_version > Self::LATEST_SCHEMA_VERSION {
            return Err(StorageError::Conflict(format!(
                "Outbox schema_version 过新: {} > {}",
                self.schema_version,
                Self::LATEST_SCHEMA_VERSION
            )));
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
                schema_version: OutboxEvent::LATEST_SCHEMA_VERSION,
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

/// 一条 Outbox 事件最多被 relay 尝试投递多少次；用尽后它不再被端出，而是留在
/// outbox 里等人工确认（V11 K2）。
///
/// 修前的形状：`retry` 只把 `attempts` 加一、删掉租约，`available` 只看租约定不看
/// `attempts`，于是一条永远发不出去的事件每轮都被重新端出，并按 `created_ts` 排在最前
/// 挤住整条尾巴（`pump_once` 取最旧的 `limit` 条）——头部一个毒事件就让尾部全部事件
/// 无限期停摆，relay 也以固定节拍空转。消费者侧早就有 `max_attempts` + 死信，出站侧缺
/// 同款，这是同族缺陷的另一半。
///
/// 计数口径放在这里而不是三本后端的 SQL 里：判据只有一个出口，文件 / SQLite /
/// PostgreSQL 交回的候选集形状不变，改的是 relay 的取用规则。
pub const OUTBOX_MAX_ATTEMPTS: u32 = 8;

/// 尝试次数是否已经用尽投递预算。用尽的事件不再被 relay 投递，但仍留在 outbox 里可见。
pub const fn outbox_exhausted(attempts: u32) -> bool {
    attempts >= OUTBOX_MAX_ATTEMPTS
}

/// 文件、SQLite、PostgreSQL 和 MQ relay 共用的出站事件语义。
pub trait OutboxStore: Send + Sync {
    fn append_outbox(&self, event: OutboxEvent) -> Result<(), StorageError>;
    /// 端出至多 `limit` 条可投递的事件：已持有有效租约的、以及**已用尽投递预算的**都排到最后，
    /// 后者仍留在候选集里等人工 `claim`/`ack`（K2 的出口），只是不再占住页首。
    /// `limit` 传 [`usize::MAX`] 表示读全量（运维列面），relay 传它自己的投递预算。
    fn available_outbox(&self, now: u64, limit: usize) -> Result<Vec<OutboxEvent>, StorageError>;
    /// 停在投递预算外的事件条数。这是状态量而不是本轮观察值：候选集被 `limit` 截断后，
    /// 逐行数出来的条数会随页数漂移，运维面上「有几条发不出去」必须由这条不问页数的读给出。
    fn count_parked_outbox(&self) -> Result<u64, StorageError>;
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
    /// 库里当前有几条已用尽投递预算、因而不再被尝试（V11 K2 计数、R7-d 改成不问页数的状态量）。
    pub parked: u64,
    pub last_error: Option<String>,
}

/// 通用 Outbox relay。它不关心 NATS、Redpanda 或 HTTP 的具体 SDK，负责保证
/// claim → publish → ack 的生命周期；发布失败只释放租约并递增 attempts，而 attempts 到达
/// [`OUTBOX_MAX_ATTEMPTS`] 的事件由 relay 跳过、由 [`OutboxStore::count_parked_outbox`] 数出来，
/// 既不再阻断后面的事件，也不占据每一轮的读取页数。
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
        // 停摆条数问的是「库里现在有几条发不出去」，不是「这一页里数到几条」：候选集被
        // `limit` 截断后逐行数会随页数漂移，少报等于运维面上那条毒事件消失（V11 R7-d）。
        report.parked = self.store.count_parked_outbox()?;
        // 页数上界只有 store 那一处读：这里再数一遍 `delivered >= limit` 是一份走不到的第二判据
        // ——三本后端的 LIMIT 由跨后端契约用例钉住，relay 只负责「端上来的这一页逐条投递」。
        for event in self.store.available_outbox(now, limit)? {
            if outbox_exhausted(event.attempts) {
                continue;
            }
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
    /// 点查某个事件的死信，返回 `attempts` 最大（最后一次入账）的那一行。
    ///
    /// 刻意不是「取一页再筛」：死信重放是点名操作，而点名的事件不在那一页时，
    /// 「取一页」会把「存在」念成「不存在」。
    fn dead_letter(
        &self,
        group_id: &str,
        event_id: &str,
    ) -> Result<Option<DeadLetterRecord>, StorageError>;
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
    /// P1c（§4.9）：重试判定收敛到 `qx-core` 统一策略（仅计次，无延时）。
    retry_policy: retry::RetryPolicy,
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
            retry_policy: retry::RetryPolicy::attempts_only(max_attempts),
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
            if self.retry_policy.should_retry(delivery_attempt) {
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
            // failed event remains available through `dead_letter`.
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
                if self.retry_policy.should_retry(delivery_attempt) {
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

// --- P1c（§4.9）：crate 根负载类型的信封声明 ------------------------------
//
// 序列化、版本拒绝、损坏拒绝与原子替换的唯一实现都在 `state_envelope`；这里只声明
// 留在 crate 根的载荷类型（OutboxEvent/OutboxLease/QueuedJob/JobLease）的文案主体与
// （如有）内嵌版本字段。`FileConsumerState` 的声明随其存储一并迁入 `file::consumers`。
// 磁盘格式与迁移前逐字节一致：首代没有版本 key 的负载保持
// `embedded_schema_version() == None`，不就地补 key。

impl JsonStateEnvelope for OutboxEvent {
    const LABEL: &'static str = "Outbox 事件";
    const SUPPORTED_SCHEMA_VERSION: u32 = Self::LATEST_SCHEMA_VERSION;

    fn embedded_schema_version(&self) -> Option<u32> {
        Some(self.schema_version)
    }

    fn validate_loaded(&self) -> Result<(), StorageError> {
        self.validate()
    }
}

impl JsonStateEnvelope for OutboxLease {
    const LABEL: &'static str = "Outbox 租约";
}

impl JsonStateEnvelope for QueuedJob {
    const LABEL: &'static str = "任务";

    fn validate_loaded(&self) -> Result<(), StorageError> {
        self.validate()
    }
}

impl JsonStateEnvelope for JobLease {
    const LABEL: &'static str = "任务租约";
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
    /// 尚未确认（未 ack）的全部命令，忽略活跃租约。默认实现把 `available`
    /// 的时间推到上限，使文件/SQLite/PostgreSQL 后端共享同一“待清理”语义。
    fn pending_commands(&self) -> Result<Vec<QueuedControlCommand>, StorageError> {
        self.available_commands(u64::MAX)
    }
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

    fn acquire_claim_lock(&self, command_id: u64) -> Result<FileLock, StorageError> {
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
/// 每条记录都携带前一条记录摘要和自身摘要。冷读侧（`read_entries`）重算整条链，
/// 文件被截断、重排或篡改时会显式失败，而不是返回看似完整的审计结果；追加侧只
/// 复算尾部窗口内的链段，因为每写一条都重验整条链会把写入频率乘上链长（V11 R5-2）。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    pub sequence: u64,
    pub record: AuditRecord,
    pub previous_hash: u64,
    pub entry_hash: u64,
}

/// 审计后端的冷读契约：把整条链读回来并逐环校验，断链、篡改与重排都要失败而不是
/// 返回一段看似完整的流水。
///
/// 它不是控制面的写入通道：控制面流水只由 `transact_control` 在后端自己的
/// `AuditChainWriter` 里续链（V11 R5-2），尾部读与截残尾也都归那个写入器。一份链有两个
/// 都能写的地方，正是这条链要防的那件事。
///
/// 生产读者是 doctor 的 `audit_chain` 检查（`qx-cli`，覆盖 Files 与 Sqlite 两个落点；
/// Postgres 要连库才能读，doctor 不触网，只报未扫描）。按命令号或序号的游标查询在
/// R5-2 里删掉了：它们一个读者都没有，而每次调用都得把整条链读回来再过滤。
pub trait AuditStore {
    fn read_entries(&self) -> Result<Vec<AuditEntry>, StorageError>;
}

fn audit_entry_hash(sequence: u64, previous_hash: u64, record: &AuditRecord) -> u64 {
    // 下面那行 `{:?}` 是本仓 `event.rs` 口径里唯一一处例外：状态名字本身就是已落盘链的
    // 摘要输入，换成稳定编码会作废每一条款存链。因此两半都有钉子——词表由
    // `audit_chain_status_vocabulary_is_pinned_by_literal_words` 逐颗钉住，输入的摆法由
    // `audit_chain_digest_inputs_are_pinned_by_a_golden_record` 那颗字面量钉住（V11 R7-7）。
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

/// 一条审计链的可信末尾：条数与链尾摘要。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuditChainTip {
    pub entries: u64,
    pub head_hash: u64,
}

/// 审计链的读侧核对，也是整条链唯一的公开校验入口（V11 R5-2）。
///
/// 判的是两件事，缺一件都不算通过：链自身逐环重算要连续（`read_entries`/`read` 已经
/// 关不掉的那半），以及控制面状态里的那两格检查点正好指在链尾。只查前者会放过"链被
/// 整段替换过"，只查后者会放过"链自身断环但末尾对得上"。
///
/// 这里刻意不做 IO、也不按后端分叉：三个后端都把整条链读成同一个 `Vec<AuditEntry>`，
/// 判据因此只有一份写法。
pub fn verify_audit_chain(
    plane: &ControlPlane,
    chain: &[AuditEntry],
) -> Result<AuditChainTip, StorageError> {
    validate_audit_chain(chain)?;
    let tip = AuditChainTip {
        entries: chain.len() as u64,
        head_hash: chain.last().map(|entry| entry.entry_hash).unwrap_or(0),
    };
    let state = plane.audit_chain();
    if state.seq != tip.entries || state.head_hash != tip.head_hash {
        return Err(StorageError::Conflict(format!(
            "控制面检查点 ({}, {:016x}) 不指向审计链尾 ({}, {:016x})：要么有流水写出时没进链，\
             要么链被截短或整段替换过",
            state.seq, state.head_hash, tip.entries, tip.head_hash
        )));
    }
    Ok(tip)
}

/// 审计链写入侧的后端动作：读链尾、截残尾、追加已算好摘要的条目。
///
/// 只有 `transact_control` 会构造它，所以三个动作都跑在该事务已经持有的锁/事务里：
/// 文件后端握着 `audit.append.lock`，数据库后端握着同一笔还没有提交的事务。
/// 独立实现这三个方法的后端不需要导出这个 trait——链的写入者只有控制面。
pub(crate) trait AuditChainWriter {
    /// 链尾那一条（尾部读，不把整条链拉回来）。
    fn tail(&mut self) -> Result<Option<AuditEntry>, StorageError>;
    /// 丢掉 `sequence` 及其之后的行。
    fn drop_from(&mut self, sequence: u64) -> Result<(), StorageError>;
    /// 追加已经算好序号与摘要的条目，落盘顺序必须早于控制面状态。
    fn append_entries(&mut self, entries: &[AuditEntry]) -> Result<(), StorageError>;
}

/// 把本轮事务新产出的审计流水接进哈希链，并把检查点回填回控制面。
///
/// 形状是"链先落、状态后落"：崩在两者中间只会留下链上没人引用的残尾，下一笔事务
/// 先把它截掉；反过来就会留下"状态引用了一条没落盘的记录"，那种洞只能靠人补。
/// 因此检查点比对失败时这里直接失败关闭——写不进链的命令也不会写进状态。
pub(crate) fn chain_audit(
    plane: &mut ControlPlane,
    writer: &mut dyn AuditChainWriter,
) -> Result<(), StorageError> {
    let (checkpoint, head_hash, records) = {
        let state = plane.audit_chain();
        (state.seq, state.head_hash, state.unchained.to_vec())
    };
    let mut tail = writer.tail()?;
    if tail
        .as_ref()
        .is_some_and(|entry| entry.sequence >= checkpoint)
    {
        writer.drop_from(checkpoint)?;
        tail = writer.tail()?;
    }
    match &tail {
        None if checkpoint == 0 => {}
        None => {
            return Err(StorageError::Conflict(format!(
                "控制面检查点引用了 {checkpoint} 条审计记录，审计链却是空的"
            )))
        }
        Some(entry) if entry.sequence.saturating_add(1) == checkpoint
            && entry.entry_hash == head_hash => {}
        Some(entry) => {
            return Err(StorageError::Conflict(format!(
                "审计链尾与控制面检查点不一致: 链尾 {} 摘要不接检查点 {checkpoint}，拒绝在不连续的链上追加",
                entry.sequence
            )))
        }
    }
    if records.is_empty() {
        return Ok(());
    }
    let mut entries = Vec::with_capacity(records.len());
    let mut sequence = checkpoint;
    let mut previous_hash = head_hash;
    for record in &records {
        let entry_hash = audit_entry_hash(sequence, previous_hash, record);
        entries.push(AuditEntry {
            sequence,
            record: record.clone(),
            previous_hash,
            entry_hash,
        });
        previous_hash = entry_hash;
        sequence = sequence.saturating_add(1);
    }
    writer.append_entries(&entries)?;
    plane.note_audit_chained(entries.len(), previous_hash);
    Ok(())
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
    fn ack_job_at(
        &self,
        run_id: u64,
        worker: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<PathBuf, StorageError>;
    fn recover_expired_leases(&self, now: u64) -> Result<Vec<u64>, StorageError>;
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

#[cfg(test)]
mod tests;
