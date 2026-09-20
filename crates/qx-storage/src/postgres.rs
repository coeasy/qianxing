//! PostgreSQL 生产存储后端。
//!
//! 该模块把文件/SQLite 后端已经验收的幂等、租约、fencing、审计链和快照校验
//! 语义搬到 PostgreSQL 事务中。所有 u64 身份和时间均以十进制 TEXT 保存，避免
//! 把交易系统的无符号 64 位域静默截断为 PostgreSQL BIGINT。
//!
//! PostgresStorage::connect 保留单连接兼容入口；生产部署可使用
//! connect_with_pool_size 建立固定大小的同步连接池。每个连接仍按事务边界
//! 串行化，连接池只解决并发连接复用，不等同于读写分离或跨节点 HA。

use super::{
    audit_entry_hash, validate_audit_chain, AuditEntry, AuditStore, ConsumerCheckpoint,
    ConsumerProjection, ConsumerStateStore, ControlCommandLease, ControlCommandQueueBackend,
    DeadLetterRecord, EventLogStore, JobLease, JobQueueBackend, OutboxEvent, OutboxLease,
    OutboxStore, QueuedControlCommand, QueuedJob, StorageError, TransactionalConsumerStateStore,
};
use postgres::{Client, GenericClient, Transaction};
use qx_control::{AuditRecord, ControlCommand, ControlPlane};
use qx_core::{EventLog, QxResult};
use qx_protocol::{AccountSnapshot, ProtocolError, SnapshotStore};
use qx_scheduler::{JobRun, JobSpec};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

const QUEUE_LOCK_KEY: i64 = 7_381_922_401_127;
const AUDIT_LOCK_KEY: i64 = 7_381_922_401_129;
const EVENT_LOG_LOCK_KEY: i64 = 7_381_922_401_131;
const OUTBOX_LOCK_KEY: i64 = 7_381_922_401_137;
const CONSUMER_LOCK_KEY: i64 = 7_381_922_401_139;

fn pg_error(error: postgres::Error) -> StorageError {
    StorageError::Io(format!("PostgreSQL: {error}"))
}

fn pg_protocol_error(error: postgres::Error) -> ProtocolError {
    ProtocolError::Io(format!("PostgreSQL: {error}"))
}

fn lock_error() -> StorageError {
    StorageError::Io("PostgreSQL 客户端锁已中毒".into())
}

fn protocol_lock_error() -> ProtocolError {
    ProtocolError::Io("PostgreSQL 客户端锁已中毒".into())
}

fn u64_text(value: u64) -> String {
    value.to_string()
}

fn parse_u64(value: &str, field: &str) -> Result<u64, StorageError> {
    value
        .parse::<u64>()
        .map_err(|error| StorageError::Conflict(format!("PostgreSQL {field} 非法: {error}")))
}

fn marker_path(table: &str, key: u64) -> PathBuf {
    PathBuf::from(format!("postgres://qx/{table}/{key}"))
}

fn marker_name(table: &str, name: &str) -> PathBuf {
    PathBuf::from(format!("postgres://qx/{table}/{name}"))
}

/// PostgreSQL 连接和迁移边界。
pub struct PostgresStorage {
    clients: Arc<Vec<Mutex<Client>>>,
    next_client: Arc<AtomicUsize>,
}

impl Clone for PostgresStorage {
    fn clone(&self) -> Self {
        Self {
            clients: Arc::clone(&self.clients),
            next_client: Arc::clone(&self.next_client),
        }
    }
}

impl std::fmt::Debug for PostgresStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PostgresStorage")
            .finish_non_exhaustive()
    }
}

impl PostgresStorage {
    /// 使用 PostgreSQL DSN 连接并执行幂等迁移。
    ///
    /// 连接使用系统 native-tls；生产 DSN 应显式设置 `sslmode=require` 或更严格
    /// 模式。DSN 不应写入日志；此入口本身不读取交易密钥，也不把连接字符串暴露
    /// 给 `Debug`。
    pub fn connect(dsn: &str) -> Result<Self, StorageError> {
        Self::connect_with_pool_size(dsn, 1)
    }

    pub fn connect_with_pool_size(dsn: &str, pool_size: usize) -> Result<Self, StorageError> {
        if dsn.trim().is_empty() {
            return Err(StorageError::Conflict("PostgreSQL DSN 不能为空".into()));
        }
        if pool_size == 0 || pool_size > 128 {
            return Err(StorageError::Conflict(
                "PostgreSQL pool_size 必须在 1..=128 内".into(),
            ));
        }
        let mut clients = Vec::with_capacity(pool_size);
        for index in 0..pool_size {
            let tls = native_tls::TlsConnector::builder()
                .build()
                .map_err(|error| StorageError::Io(format!("PostgreSQL TLS 初始化失败: {error}")))?;
            let connector = postgres_native_tls::MakeTlsConnector::new(tls);
            let mut client = Client::connect(dsn, connector).map_err(pg_error)?;
            if index == 0 {
                migrate_client(&mut client)?;
            }
            configure_client(&mut client)?;
            clients.push(Mutex::new(client));
        }
        Ok(Self {
            clients: Arc::new(clients),
            next_client: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn migrate(&self) -> Result<(), StorageError> {
        let mut client = self.lock_client()?;
        migrate_client(&mut client)
    }

    pub fn health_check(&self) -> Result<(), StorageError> {
        let mut client = self.lock_client()?;
        client
            .query_one("SELECT 1", &[])
            .map(|_| ())
            .map_err(pg_error)
    }

    fn lock_client(&self) -> Result<MutexGuard<'_, Client>, StorageError> {
        let index = self.next_client.fetch_add(1, Ordering::Relaxed) % self.clients.len();
        self.clients[index].lock().map_err(|_| lock_error())
    }

    fn lock_client_protocol(&self) -> Result<MutexGuard<'_, Client>, ProtocolError> {
        let index = self.next_client.fetch_add(1, Ordering::Relaxed) % self.clients.len();
        self.clients[index]
            .lock()
            .map_err(|_| protocol_lock_error())
    }
}

fn configure_client(client: &mut Client) -> Result<(), StorageError> {
    client
        .batch_execute(
            "SET application_name = 'qianxing';
             SET statement_timeout = '30s';
             SET lock_timeout = '5s';
             SET idle_in_transaction_session_timeout = '60s';",
        )
        .map_err(pg_error)
}

fn migrate_client(client: &mut Client) -> Result<(), StorageError> {
    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS qx_schema_migrations (
                 version TEXT PRIMARY KEY NOT NULL,
                 applied_ts TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS qx_event_logs (
                 name TEXT PRIMARY KEY NOT NULL,
                 content TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS qx_audit_entries (
                 sequence TEXT PRIMARY KEY NOT NULL,
                 record_json TEXT NOT NULL,
                 previous_hash TEXT NOT NULL,
                 entry_hash TEXT NOT NULL UNIQUE
             );
             CREATE TABLE IF NOT EXISTS qx_jobs (
                 run_id TEXT PRIMARY KEY NOT NULL,
                 envelope_json TEXT NOT NULL,
                 done BOOLEAN NOT NULL DEFAULT FALSE
             );
             CREATE TABLE IF NOT EXISTS qx_job_leases (
                 run_id TEXT PRIMARY KEY NOT NULL,
                 owner TEXT NOT NULL,
                 expires_ts TEXT NOT NULL,
                 fencing_token TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS qx_control_state (
                 state_id INTEGER PRIMARY KEY NOT NULL CHECK (state_id = 1),
                 content TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS qx_control_commands (
                 command_id TEXT PRIMARY KEY NOT NULL,
                 command_json TEXT NOT NULL,
                 enqueued_ts TEXT NOT NULL,
                 done BOOLEAN NOT NULL DEFAULT FALSE
             );
             CREATE TABLE IF NOT EXISTS qx_control_command_leases (
                 command_id TEXT PRIMARY KEY NOT NULL,
                 owner TEXT NOT NULL,
                 expires_ts TEXT NOT NULL,
                 fencing_token TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS qx_snapshots (
                 snapshot_id TEXT NOT NULL,
                 state_hash TEXT NOT NULL,
                 content TEXT NOT NULL,
                 PRIMARY KEY(snapshot_id, state_hash)
             );
             CREATE TABLE IF NOT EXISTS qx_outbox_events (
                 event_id TEXT PRIMARY KEY NOT NULL,
                 topic TEXT NOT NULL,
                 partition_key TEXT NOT NULL,
                 sequence TEXT NOT NULL,
                 schema_version INTEGER NOT NULL,
                 trace_id TEXT NOT NULL,
                 payload TEXT NOT NULL,
                 created_ts TEXT NOT NULL,
                 attempts TEXT NOT NULL DEFAULT '0'
             );
             CREATE TABLE IF NOT EXISTS qx_outbox_leases (
                 event_id TEXT PRIMARY KEY NOT NULL,
                 owner TEXT NOT NULL,
                 expires_ts TEXT NOT NULL,
                 fencing_token TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS qx_jobs_pending_idx
                 ON qx_jobs(done, run_id);
             CREATE INDEX IF NOT EXISTS qx_control_commands_pending_idx
                 ON qx_control_commands(done, enqueued_ts, command_id);
             CREATE INDEX IF NOT EXISTS qx_outbox_pending_idx
                 ON qx_outbox_events(created_ts, sequence, event_id);
             CREATE TABLE IF NOT EXISTS qx_consumer_checkpoints (
                 group_id TEXT NOT NULL,
                 topic TEXT NOT NULL,
                 partition_key TEXT NOT NULL,
                 offset TEXT NOT NULL,
                 event_id TEXT NOT NULL,
                 updated_ts TEXT NOT NULL,
                 PRIMARY KEY(group_id, topic, partition_key)
             );
             CREATE TABLE IF NOT EXISTS qx_consumer_processed (
                 group_id TEXT NOT NULL,
                 event_id TEXT NOT NULL,
                 PRIMARY KEY(group_id, event_id)
             );
             CREATE TABLE IF NOT EXISTS qx_consumer_dead_letters (
                 group_id TEXT NOT NULL,
                 event_id TEXT NOT NULL,
                 attempts TEXT NOT NULL,
                 record_json TEXT NOT NULL,
                 PRIMARY KEY(group_id, event_id, attempts)
             );
             CREATE TABLE IF NOT EXISTS qx_consumer_projections (
                 group_id TEXT NOT NULL,
                 projection_key TEXT NOT NULL,
                 topic TEXT NOT NULL,
                 partition_key TEXT NOT NULL,
                 offset TEXT NOT NULL,
                 event_id TEXT NOT NULL,
                 payload TEXT NOT NULL,
                 updated_ts TEXT NOT NULL,
                 PRIMARY KEY(group_id, projection_key)
             );
             INSERT INTO qx_schema_migrations(version, applied_ts)
                 VALUES ('1', '0') ON CONFLICT(version) DO NOTHING;",
        )
        .map_err(pg_error)
}

fn lock_transaction<'a>(transaction: &mut Transaction<'a>, key: i64) -> Result<(), StorageError> {
    transaction
        .query_one("SELECT pg_advisory_xact_lock($1)", &[&key])
        .map(|_| ())
        .map_err(pg_error)
}

// ----------------------------- Consumer state ---------------------------

#[derive(Clone, Debug)]
pub struct PostgresConsumerStateStore {
    storage: PostgresStorage,
}

impl PostgresConsumerStateStore {
    pub fn connect(dsn: &str) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect(dsn)?,
        })
    }

    pub fn connect_with_pool_size(dsn: &str, pool_size: usize) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect_with_pool_size(dsn, pool_size)?,
        })
    }

    pub fn from_storage(storage: PostgresStorage) -> Self {
        Self { storage }
    }
}

impl ConsumerStateStore for PostgresConsumerStateStore {
    fn load_checkpoint(
        &self,
        group_id: &str,
        topic: &str,
        partition_key: &str,
    ) -> Result<Option<ConsumerCheckpoint>, StorageError> {
        let mut client = self.storage.lock_client()?;
        client
            .query_opt(
                "SELECT offset, event_id, updated_ts FROM qx_consumer_checkpoints
                 WHERE group_id = $1 AND topic = $2 AND partition_key = $3",
                &[&group_id, &topic, &partition_key],
            )
            .map_err(pg_error)?
            .map(|row| {
                Ok(ConsumerCheckpoint {
                    group_id: group_id.into(),
                    topic: topic.into(),
                    partition_key: partition_key.into(),
                    offset: parse_u64(row.get::<_, String>(0).as_str(), "consumer.offset")?,
                    event_id: row.get(1),
                    updated_ts: parse_u64(row.get::<_, String>(2).as_str(), "consumer.updated_ts")?,
                })
            })
            .transpose()
    }

    fn is_processed(&self, group_id: &str, event_id: &str) -> Result<bool, StorageError> {
        let mut client = self.storage.lock_client()?;
        client
            .query_opt(
                "SELECT 1 FROM qx_consumer_processed WHERE group_id = $1 AND event_id = $2",
                &[&group_id, &event_id],
            )
            .map(|row| row.is_some())
            .map_err(pg_error)
    }

    fn commit_processed(&self, checkpoint: ConsumerCheckpoint) -> Result<(), StorageError> {
        checkpoint.validate()?;
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, CONSUMER_LOCK_KEY)?;
        let inserted = transaction
            .execute(
                "INSERT INTO qx_consumer_processed(group_id, event_id)
                 VALUES ($1, $2) ON CONFLICT(group_id, event_id) DO NOTHING",
                &[&checkpoint.group_id, &checkpoint.event_id],
            )
            .map_err(pg_error)?;
        if inserted == 0 {
            transaction.commit().map_err(pg_error)?;
            return Ok(());
        }
        let previous = transaction
            .query_opt(
                "SELECT offset, event_id FROM qx_consumer_checkpoints
                 WHERE group_id = $1 AND topic = $2 AND partition_key = $3",
                &[
                    &checkpoint.group_id,
                    &checkpoint.topic,
                    &checkpoint.partition_key,
                ],
            )
            .map_err(pg_error)?;
        if let Some(row) = previous {
            let previous_offset = parse_u64(row.get::<_, String>(0).as_str(), "consumer.offset")?;
            let previous_event_id: String = row.get(1);
            if previous_offset > checkpoint.offset
                || (previous_offset == checkpoint.offset
                    && previous_event_id != checkpoint.event_id)
            {
                return Err(StorageError::Conflict(
                    "consumer checkpoint 顺序或 event_id 非法".into(),
                ));
            }
        }
        transaction
            .execute(
                "INSERT INTO qx_consumer_checkpoints
                 (group_id, topic, partition_key, offset, event_id, updated_ts)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT(group_id, topic, partition_key) DO UPDATE SET
                   offset = EXCLUDED.offset, event_id = EXCLUDED.event_id,
                   updated_ts = EXCLUDED.updated_ts",
                &[
                    &checkpoint.group_id,
                    &checkpoint.topic,
                    &checkpoint.partition_key,
                    &u64_text(checkpoint.offset),
                    &checkpoint.event_id,
                    &u64_text(checkpoint.updated_ts),
                ],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)
    }

    fn append_dead_letter(&self, record: DeadLetterRecord) -> Result<(), StorageError> {
        record.validate()?;
        let content = serde_json::to_string(&record)
            .map_err(|error| StorageError::Io(format!("死信记录序列化失败: {error}")))?;
        let mut client = self.storage.lock_client()?;
        client
            .execute(
                "INSERT INTO qx_consumer_dead_letters
                 (group_id, event_id, attempts, record_json)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT(group_id, event_id, attempts) DO NOTHING",
                &[
                    &record.group_id,
                    &record.event_id,
                    &u64_text(record.attempts as u64),
                    &content,
                ],
            )
            .map_err(pg_error)?;
        Ok(())
    }

    fn dead_letters(
        &self,
        group_id: &str,
        limit: usize,
    ) -> Result<Vec<DeadLetterRecord>, StorageError> {
        let mut client = self.storage.lock_client()?;
        let rows = client
            .query(
                "SELECT record_json FROM qx_consumer_dead_letters
                 WHERE group_id = $1 ORDER BY attempts, event_id LIMIT $2",
                &[&group_id, &(limit as i64)],
            )
            .map_err(pg_error)?;
        rows.into_iter()
            .map(|row| {
                let content: String = row.get(0);
                serde_json::from_str(&content)
                    .map_err(|error| StorageError::Io(format!("死信记录解析失败: {error}")))
            })
            .collect()
    }
}

impl TransactionalConsumerStateStore for PostgresConsumerStateStore {
    fn commit_processed_with_projection(
        &self,
        checkpoint: ConsumerCheckpoint,
        projection: ConsumerProjection,
    ) -> Result<(), StorageError> {
        projection.validate_for(&checkpoint)?;
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, CONSUMER_LOCK_KEY)?;
        let inserted = transaction
            .execute(
                "INSERT INTO qx_consumer_processed(group_id, event_id)
                 VALUES ($1, $2) ON CONFLICT(group_id, event_id) DO NOTHING",
                &[&checkpoint.group_id, &checkpoint.event_id],
            )
            .map_err(pg_error)?;
        if inserted == 0 {
            transaction.commit().map_err(pg_error)?;
            return Ok(());
        }
        let previous = transaction
            .query_opt(
                "SELECT offset, event_id FROM qx_consumer_checkpoints
                 WHERE group_id = $1 AND topic = $2 AND partition_key = $3",
                &[
                    &checkpoint.group_id,
                    &checkpoint.topic,
                    &checkpoint.partition_key,
                ],
            )
            .map_err(pg_error)?;
        if let Some(row) = previous {
            let previous_offset = parse_u64(row.get::<_, String>(0).as_str(), "consumer.offset")?;
            let previous_event_id: String = row.get(1);
            if previous_offset > checkpoint.offset
                || (previous_offset == checkpoint.offset
                    && previous_event_id != checkpoint.event_id)
            {
                return Err(StorageError::Conflict(
                    "consumer checkpoint 顺序或 event_id 非法".into(),
                ));
            }
        }
        let previous_projection = transaction
            .query_opt(
                "SELECT offset, event_id FROM qx_consumer_projections
                 WHERE group_id = $1 AND projection_key = $2",
                &[&projection.group_id, &projection.projection_key],
            )
            .map_err(pg_error)?;
        if let Some(row) = previous_projection {
            let previous_offset = parse_u64(
                row.get::<_, String>(0).as_str(),
                "consumer.projection.offset",
            )?;
            let previous_event_id: String = row.get(1);
            if previous_offset > projection.offset
                || (previous_offset == projection.offset
                    && previous_event_id != projection.event_id)
            {
                return Err(StorageError::Conflict(
                    "consumer projection 顺序或 event_id 非法".into(),
                ));
            }
        }
        transaction
            .execute(
                "INSERT INTO qx_consumer_projections
                 (group_id, projection_key, topic, partition_key, offset, event_id, payload, updated_ts)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                 ON CONFLICT(group_id, projection_key) DO UPDATE SET
                   topic = EXCLUDED.topic, partition_key = EXCLUDED.partition_key,
                   offset = EXCLUDED.offset, event_id = EXCLUDED.event_id,
                   payload = EXCLUDED.payload, updated_ts = EXCLUDED.updated_ts",
                &[
                    &projection.group_id,
                    &projection.projection_key,
                    &projection.topic,
                    &projection.partition_key,
                    &u64_text(projection.offset),
                    &projection.event_id,
                    &projection.payload,
                    &u64_text(projection.updated_ts),
                ],
            )
            .map_err(pg_error)?;
        transaction
            .execute(
                "INSERT INTO qx_consumer_checkpoints
                 (group_id, topic, partition_key, offset, event_id, updated_ts)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT(group_id, topic, partition_key) DO UPDATE SET
                   offset = EXCLUDED.offset, event_id = EXCLUDED.event_id,
                   updated_ts = EXCLUDED.updated_ts",
                &[
                    &checkpoint.group_id,
                    &checkpoint.topic,
                    &checkpoint.partition_key,
                    &u64_text(checkpoint.offset),
                    &checkpoint.event_id,
                    &u64_text(checkpoint.updated_ts),
                ],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)
    }

    fn append_dead_letter_and_commit(
        &self,
        record: DeadLetterRecord,
        checkpoint: ConsumerCheckpoint,
    ) -> Result<(), StorageError> {
        record.validate_for(&checkpoint)?;
        let content = serde_json::to_string(&record)
            .map_err(|error| StorageError::Io(format!("死信记录序列化失败: {error}")))?;
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, CONSUMER_LOCK_KEY)?;
        let inserted = transaction
            .execute(
                "INSERT INTO qx_consumer_processed(group_id, event_id)
                 VALUES ($1, $2) ON CONFLICT(group_id, event_id) DO NOTHING",
                &[&checkpoint.group_id, &checkpoint.event_id],
            )
            .map_err(pg_error)?;
        if inserted == 0 {
            transaction.commit().map_err(pg_error)?;
            return Ok(());
        }
        let previous = transaction
            .query_opt(
                "SELECT offset, event_id FROM qx_consumer_checkpoints
                 WHERE group_id = $1 AND topic = $2 AND partition_key = $3",
                &[
                    &checkpoint.group_id,
                    &checkpoint.topic,
                    &checkpoint.partition_key,
                ],
            )
            .map_err(pg_error)?;
        if let Some(row) = previous {
            let previous_offset = parse_u64(row.get::<_, String>(0).as_str(), "consumer.offset")?;
            let previous_event_id: String = row.get(1);
            if previous_offset > checkpoint.offset
                || (previous_offset == checkpoint.offset
                    && previous_event_id != checkpoint.event_id)
            {
                return Err(StorageError::Conflict(
                    "consumer checkpoint 顺序或 event_id 非法".into(),
                ));
            }
        }
        transaction
            .execute(
                "INSERT INTO qx_consumer_dead_letters
                 (group_id, event_id, attempts, record_json)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT(group_id, event_id, attempts) DO NOTHING",
                &[
                    &record.group_id,
                    &record.event_id,
                    &u64_text(record.attempts as u64),
                    &content,
                ],
            )
            .map_err(pg_error)?;
        transaction
            .execute(
                "INSERT INTO qx_consumer_checkpoints
                 (group_id, topic, partition_key, offset, event_id, updated_ts)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT(group_id, topic, partition_key) DO UPDATE SET
                   offset = EXCLUDED.offset, event_id = EXCLUDED.event_id,
                   updated_ts = EXCLUDED.updated_ts",
                &[
                    &checkpoint.group_id,
                    &checkpoint.topic,
                    &checkpoint.partition_key,
                    &u64_text(checkpoint.offset),
                    &checkpoint.event_id,
                    &u64_text(checkpoint.updated_ts),
                ],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)
    }

    fn load_projection(
        &self,
        group_id: &str,
        projection_key: &str,
    ) -> Result<Option<ConsumerProjection>, StorageError> {
        let mut client = self.storage.lock_client()?;
        client
            .query_opt(
                "SELECT topic, partition_key, offset, event_id, payload, updated_ts
                 FROM qx_consumer_projections
                 WHERE group_id = $1 AND projection_key = $2",
                &[&group_id, &projection_key],
            )
            .map_err(pg_error)?
            .map(|row| {
                Ok(ConsumerProjection {
                    group_id: group_id.into(),
                    projection_key: projection_key.into(),
                    topic: row.get(0),
                    partition_key: row.get(1),
                    offset: parse_u64(row.get::<_, String>(2).as_str(), "consumer.offset")?,
                    event_id: row.get(3),
                    payload: row.get(4),
                    updated_ts: parse_u64(row.get::<_, String>(5).as_str(), "consumer.updated_ts")?,
                })
            })
            .transpose()
    }
}

// ----------------------------- Outbox ----------------------------------

#[derive(Clone, Debug)]
pub struct PostgresOutboxStore {
    storage: PostgresStorage,
}

impl PostgresOutboxStore {
    pub fn connect(dsn: &str) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect(dsn)?,
        })
    }

    pub fn connect_with_pool_size(dsn: &str, pool_size: usize) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect_with_pool_size(dsn, pool_size)?,
        })
    }

    pub fn from_storage(storage: PostgresStorage) -> Self {
        Self { storage }
    }

    pub fn append(&self, event: OutboxEvent) -> Result<(), StorageError> {
        event.validate()?;
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, OUTBOX_LOCK_KEY)?;
        let existing = transaction
            .query_opt(
                "SELECT topic, partition_key, sequence, schema_version, trace_id, payload, created_ts, attempts
                 FROM qx_outbox_events WHERE event_id = $1",
                &[&event.event_id],
            )
            .map_err(pg_error)?;
        let existing = existing
            .map(|row| {
                Ok(OutboxEvent {
                    event_id: event.event_id.clone(),
                    topic: row.get(0),
                    partition_key: row.get(1),
                    sequence: parse_u64(row.get::<_, String>(2).as_str(), "outbox.sequence")?,
                    schema_version: row.get::<_, i32>(3) as u32,
                    trace_id: row.get(4),
                    payload: row.get(5),
                    created_ts: parse_u64(row.get::<_, String>(6).as_str(), "outbox.created_ts")?,
                    attempts: parse_u64(row.get::<_, String>(7).as_str(), "outbox.attempts")?
                        as u32,
                })
            })
            .transpose()?;
        if let Some(existing) = existing {
            if existing.same_fact(&event) {
                transaction.commit().map_err(pg_error)?;
                return Ok(());
            }
            return Err(StorageError::Conflict(format!(
                "event_id {} 已被不同 Outbox 事件占用",
                event.event_id
            )));
        }
        transaction
            .execute(
                "INSERT INTO qx_outbox_events
                 (event_id, topic, partition_key, sequence, schema_version, trace_id, payload, created_ts, attempts)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                &[
                    &event.event_id,
                    &event.topic,
                    &event.partition_key,
                    &u64_text(event.sequence),
                    &(event.schema_version as i32),
                    &event.trace_id,
                    &event.payload,
                    &u64_text(event.created_ts),
                    &u64_text(event.attempts as u64),
                ],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)
    }

    pub fn available(&self, now: u64) -> Result<Vec<OutboxEvent>, StorageError> {
        let mut client = self.storage.lock_client()?;
        let rows = client
            .query(
                "SELECT e.event_id, e.topic, e.partition_key, e.sequence, e.schema_version,
                        e.trace_id, e.payload, e.created_ts, e.attempts
                 FROM qx_outbox_events e
                 LEFT JOIN qx_outbox_leases l ON l.event_id = e.event_id
                 WHERE l.event_id IS NULL OR l.expires_ts <= $1
                 ORDER BY e.created_ts, e.sequence, e.event_id",
                &[&u64_text(now)],
            )
            .map_err(pg_error)?;
        rows.into_iter()
            .map(|row| {
                let event = OutboxEvent {
                    event_id: row.get(0),
                    topic: row.get(1),
                    partition_key: row.get(2),
                    sequence: parse_u64(row.get::<_, String>(3).as_str(), "outbox.sequence")?,
                    schema_version: row.get::<_, i32>(4) as u32,
                    trace_id: row.get(5),
                    payload: row.get(6),
                    created_ts: parse_u64(row.get::<_, String>(7).as_str(), "outbox.created_ts")?,
                    attempts: parse_u64(row.get::<_, String>(8).as_str(), "outbox.attempts")?
                        as u32,
                };
                event.validate()?;
                Ok(event)
            })
            .collect()
    }

    pub fn claim(
        &self,
        event_id: &str,
        owner: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<OutboxLease, StorageError> {
        validate_outbox_name(event_id)?;
        if owner.trim().is_empty() || lease_seconds == 0 {
            return Err(StorageError::Conflict(
                "Outbox worker 和租约时长不能为空".into(),
            ));
        }
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, OUTBOX_LOCK_KEY)?;
        if transaction
            .query_opt(
                "SELECT event_id FROM qx_outbox_events WHERE event_id = $1",
                &[&event_id],
            )
            .map_err(pg_error)?
            .is_none()
        {
            return Err(StorageError::NotFound(format!(
                "Outbox event_id {event_id}"
            )));
        }
        let current = transaction
            .query_opt(
                "SELECT owner, expires_ts, fencing_token FROM qx_outbox_leases WHERE event_id = $1",
                &[&event_id],
            )
            .map_err(pg_error)?;
        let mut fencing_token = 1;
        if let Some(row) = current {
            let current_owner: String = row.get(0);
            let expires_ts = parse_u64(row.get::<_, String>(1).as_str(), "outbox.expires_ts")?;
            let current_token =
                parse_u64(row.get::<_, String>(2).as_str(), "outbox.fencing_token")?;
            if expires_ts > now && current_owner != owner {
                return Err(StorageError::LeaseHeld {
                    run_id: 0,
                    owner: current_owner,
                });
            }
            fencing_token = if expires_ts <= now {
                current_token.saturating_add(1).max(1)
            } else {
                current_token.max(1)
            };
        }
        let lease = OutboxLease {
            event_id: event_id.into(),
            owner: owner.into(),
            expires_ts: now.saturating_add(lease_seconds),
            fencing_token,
        };
        transaction
            .execute(
                "INSERT INTO qx_outbox_leases(event_id, owner, expires_ts, fencing_token)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT(event_id) DO UPDATE SET owner = EXCLUDED.owner,
                   expires_ts = EXCLUDED.expires_ts, fencing_token = EXCLUDED.fencing_token",
                &[
                    &lease.event_id,
                    &lease.owner,
                    &u64_text(lease.expires_ts),
                    &u64_text(lease.fencing_token),
                ],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)?;
        Ok(lease)
    }

    pub fn ack(
        &self,
        event_id: &str,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<(), StorageError> {
        self.finish(event_id, owner, fencing_token, now, false)
    }

    pub fn retry(
        &self,
        event_id: &str,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<(), StorageError> {
        self.finish(event_id, owner, fencing_token, now, true)
    }

    fn finish(
        &self,
        event_id: &str,
        owner: &str,
        fencing_token: u64,
        now: u64,
        retry: bool,
    ) -> Result<(), StorageError> {
        validate_outbox_name(event_id)?;
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, OUTBOX_LOCK_KEY)?;
        let lease = transaction
            .query_opt(
                "SELECT owner, expires_ts, fencing_token FROM qx_outbox_leases WHERE event_id = $1",
                &[&event_id],
            )
            .map_err(pg_error)?
            .ok_or_else(|| StorageError::NotFound(format!("Outbox lease {event_id}")))?;
        let current_owner: String = lease.get(0);
        let expires_ts = parse_u64(lease.get::<_, String>(1).as_str(), "outbox.expires_ts")?;
        let current_token = parse_u64(lease.get::<_, String>(2).as_str(), "outbox.fencing_token")?;
        if current_owner != owner || current_token != fencing_token {
            return Err(StorageError::Unauthorized(format!(
                "Outbox event_id {event_id} 的 worker 或 fencing token 无效"
            )));
        }
        if expires_ts <= now {
            return Err(StorageError::LeaseExpired { run_id: 0 });
        }
        if retry {
            // P1c（§4.9）：与文件 / SQLite 后端的 `qx-core` 统一策略口径一致，
            // 尝试计数按 u32 饱和（4294967295 = u32::MAX），不再无限递增。
            transaction
                .execute(
                    "UPDATE qx_outbox_events
                     SET attempts = LEAST(attempts::numeric + 1, 4294967295)::text
                     WHERE event_id = $1",
                    &[&event_id],
                )
                .map_err(pg_error)?;
        } else {
            transaction
                .execute(
                    "DELETE FROM qx_outbox_events WHERE event_id = $1",
                    &[&event_id],
                )
                .map_err(pg_error)?;
        }
        transaction
            .execute(
                "DELETE FROM qx_outbox_leases WHERE event_id = $1",
                &[&event_id],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)
    }
}

fn validate_outbox_name(event_id: &str) -> Result<(), StorageError> {
    if event_id.is_empty()
        || event_id.len() > 240
        || event_id.contains('/')
        || event_id.contains('\\')
        || event_id.contains("..")
        || !event_id.chars().all(|value| {
            value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.' | ':' | '@')
        })
    {
        return Err(StorageError::InvalidName("Outbox event_id 非法".into()));
    }
    Ok(())
}

impl OutboxStore for PostgresOutboxStore {
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

// ----------------------------- EventLog ---------------------------------

#[derive(Clone, Debug)]
pub struct PostgresEventLogStore {
    storage: PostgresStorage,
}

impl PostgresEventLogStore {
    pub fn connect(dsn: &str) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect(dsn)?,
        })
    }

    pub fn connect_with_pool_size(dsn: &str, pool_size: usize) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect_with_pool_size(dsn, pool_size)?,
        })
    }

    pub fn from_storage(storage: PostgresStorage) -> Self {
        Self { storage }
    }

    pub fn storage(&self) -> &PostgresStorage {
        &self.storage
    }

    pub fn save(&self, name: &str, log: &EventLog) -> Result<(), StorageError> {
        self.write(name, log).map(|_| ())
    }

    pub fn write(&self, name: &str, log: &EventLog) -> Result<PathBuf, StorageError> {
        validate_name(name)?;
        log.validate().map_err(StorageError::Core)?;
        let content = log.to_json().map_err(StorageError::Core)?;
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, EVENT_LOG_LOCK_KEY)?;
        let existing: Option<String> = transaction
            .query_opt(
                "SELECT content FROM qx_event_logs WHERE name = $1",
                &[&name],
            )
            .map_err(pg_error)?
            .map(|row| row.get(0));
        if let Some(existing) = existing {
            let old = EventLog::from_json(&existing).map_err(StorageError::Core)?;
            if old.len() > log.len()
                || old
                    .events()
                    .iter()
                    .zip(log.events())
                    .any(|(left, right)| left != right)
            {
                return Err(StorageError::NonAppendOnly(name.into()));
            }
            if old.len() == log.len() {
                transaction.commit().map_err(pg_error)?;
                return Ok(marker_name("event-logs", name));
            }
        }
        transaction
            .execute(
                "INSERT INTO qx_event_logs(name, content) VALUES ($1, $2)
                 ON CONFLICT(name) DO UPDATE SET content = EXCLUDED.content",
                &[&name, &content],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)?;
        Ok(marker_name("event-logs", name))
    }

    /// 在同一个 PostgreSQL 事务中提交 EventLog 和其 Outbox 投影。
    ///
    /// EventLog 已存在且内容未增长时仍会补齐缺失的 Outbox 事件，便于 relay
    /// 或进程崩溃后的恢复扫描；Outbox 的 attempts 不参与事实幂等判断。
    pub fn write_with_outbox(
        &self,
        name: &str,
        log: &EventLog,
        outbox_events: &[OutboxEvent],
    ) -> Result<PathBuf, StorageError> {
        validate_name(name)?;
        log.validate().map_err(StorageError::Core)?;
        for event in outbox_events {
            event.validate()?;
        }
        let content = log.to_json().map_err(StorageError::Core)?;
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, EVENT_LOG_LOCK_KEY)?;
        lock_transaction(&mut transaction, OUTBOX_LOCK_KEY)?;
        let existing: Option<String> = transaction
            .query_opt(
                "SELECT content FROM qx_event_logs WHERE name = $1",
                &[&name],
            )
            .map_err(pg_error)?
            .map(|row| row.get(0));
        if let Some(existing) = existing {
            let old = EventLog::from_json(&existing).map_err(StorageError::Core)?;
            if old.len() > log.len()
                || old
                    .events()
                    .iter()
                    .zip(log.events())
                    .any(|(left, right)| left != right)
            {
                return Err(StorageError::NonAppendOnly(name.into()));
            }
            if old.len() < log.len() {
                transaction
                    .execute(
                        "UPDATE qx_event_logs SET content = $2 WHERE name = $1",
                        &[&name, &content],
                    )
                    .map_err(pg_error)?;
            }
        } else {
            transaction
                .execute(
                    "INSERT INTO qx_event_logs(name, content) VALUES ($1, $2)",
                    &[&name, &content],
                )
                .map_err(pg_error)?;
        }
        for event in outbox_events {
            append_outbox_in_transaction(&mut transaction, event)?;
        }
        transaction.commit().map_err(pg_error)?;
        Ok(marker_name("event-logs", name))
    }

    pub fn read(&self, name: &str) -> Result<EventLog, StorageError> {
        validate_name(name)?;
        let mut client = self.storage.lock_client()?;
        let content: String = client
            .query_opt(
                "SELECT content FROM qx_event_logs WHERE name = $1",
                &[&name],
            )
            .map_err(pg_error)?
            .ok_or_else(|| StorageError::NotFound(name.into()))?
            .get(0);
        EventLog::from_json(&content).map_err(StorageError::Core)
    }

    pub fn read_if_exists(&self, name: &str) -> Result<Option<EventLog>, StorageError> {
        validate_name(name)?;
        let mut client = self.storage.lock_client()?;
        let content: Option<String> = client
            .query_opt(
                "SELECT content FROM qx_event_logs WHERE name = $1",
                &[&name],
            )
            .map_err(pg_error)?
            .map(|row| row.get(0));
        content
            .map(|value| EventLog::from_json(&value).map_err(StorageError::Core))
            .transpose()
    }
}

fn append_outbox_in_transaction(
    transaction: &mut Transaction<'_>,
    event: &OutboxEvent,
) -> Result<(), StorageError> {
    let existing = transaction
        .query_opt(
            "SELECT topic, partition_key, sequence, schema_version, trace_id, payload, created_ts, attempts
             FROM qx_outbox_events WHERE event_id = $1",
            &[&event.event_id],
        )
        .map_err(pg_error)?
        .map(|row| {
            Ok(OutboxEvent {
                event_id: event.event_id.clone(),
                topic: row.get(0),
                partition_key: row.get(1),
                sequence: parse_u64(row.get::<_, String>(2).as_str(), "outbox.sequence")?,
                schema_version: row.get::<_, i32>(3) as u32,
                trace_id: row.get(4),
                payload: row.get(5),
                created_ts: parse_u64(row.get::<_, String>(6).as_str(), "outbox.created_ts")?,
                attempts: parse_u64(row.get::<_, String>(7).as_str(), "outbox.attempts")? as u32,
            })
        })
        .transpose()?;
    if let Some(existing) = existing {
        if existing.same_fact(event) {
            return Ok(());
        }
        return Err(StorageError::Conflict(format!(
            "event_id {} 已被不同 Outbox 事件占用",
            event.event_id
        )));
    }
    transaction
        .execute(
            "INSERT INTO qx_outbox_events
             (event_id, topic, partition_key, sequence, schema_version, trace_id, payload, created_ts, attempts)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            &[
                &event.event_id,
                &event.topic,
                &event.partition_key,
                &u64_text(event.sequence),
                &(event.schema_version as i32),
                &event.trace_id,
                &event.payload,
                &u64_text(event.created_ts),
                &u64_text(event.attempts as u64),
            ],
        )
        .map_err(pg_error)?;
    Ok(())
}

impl EventLogStore for PostgresEventLogStore {
    fn save(&self, name: &str, log: &EventLog) -> QxResult<()> {
        self.write(name, log).map(|_| ()).map_err(Into::into)
    }

    fn load(&self, name: &str) -> QxResult<EventLog> {
        self.read(name).map_err(Into::into)
    }
}

fn validate_name(name: &str) -> Result<(), StorageError> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || !name
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || value == '-' || value == '_')
    {
        return Err(StorageError::InvalidName(name.into()));
    }
    Ok(())
}

// ----------------------------- Audit ------------------------------------

#[derive(Clone, Debug)]
pub struct PostgresAuditStore {
    storage: PostgresStorage,
}

impl PostgresAuditStore {
    pub fn connect(dsn: &str) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect(dsn)?,
        })
    }

    pub fn connect_with_pool_size(dsn: &str, pool_size: usize) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect_with_pool_size(dsn, pool_size)?,
        })
    }

    pub fn from_storage(storage: PostgresStorage) -> Self {
        Self { storage }
    }

    pub fn append(&self, record: AuditRecord) -> Result<PathBuf, StorageError> {
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, AUDIT_LOCK_KEY)?;
        let entries = read_audit(&mut transaction)?;
        if entries.last().is_some_and(|entry| entry.record == record) {
            transaction.commit().map_err(pg_error)?;
            return Ok(PathBuf::from("postgres://qx/audit"));
        }
        let sequence = entries.len() as u64;
        let previous_hash = entries.last().map_or(0, |entry| entry.entry_hash);
        let entry_hash = audit_entry_hash(sequence, previous_hash, &record);
        let record_json = serde_json::to_string(&record)
            .map_err(|error| StorageError::Io(format!("审计序列化失败: {error}")))?;
        let sequence_text = u64_text(sequence);
        let previous_text = u64_text(previous_hash);
        let entry_text = u64_text(entry_hash);
        transaction
            .execute(
                "INSERT INTO qx_audit_entries
                 (sequence, record_json, previous_hash, entry_hash)
                 VALUES ($1, $2, $3, $4)",
                &[&sequence_text, &record_json, &previous_text, &entry_text],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)?;
        Ok(PathBuf::from("postgres://qx/audit"))
    }

    pub fn read(&self) -> Result<Vec<AuditEntry>, StorageError> {
        let mut client = self.storage.lock_client()?;
        read_audit(&mut *client)
    }

    pub fn query_command(&self, command_id: u64) -> Result<Vec<AuditEntry>, StorageError> {
        let command_id = u64_text(command_id);
        let mut client = self.storage.lock_client()?;
        let rows = client
            .query(
                "SELECT sequence, record_json, previous_hash, entry_hash
                 FROM qx_audit_entries WHERE record_json::jsonb ->> 'command_id' = $1
                 ORDER BY sequence::numeric",
                &[&command_id],
            )
            .map_err(pg_error)?;
        parse_audit_rows(rows)
    }

    pub fn after(&self, sequence: u64) -> Result<Vec<AuditEntry>, StorageError> {
        let sequence = u64_text(sequence);
        let mut client = self.storage.lock_client()?;
        let rows = client
            .query(
                "SELECT sequence, record_json, previous_hash, entry_hash
                 FROM qx_audit_entries WHERE sequence::numeric > $1::numeric
                 ORDER BY sequence::numeric",
                &[&sequence],
            )
            .map_err(pg_error)?;
        parse_audit_rows(rows)
    }

    pub fn sync_control(&self, plane: &ControlPlane) -> Result<usize, StorageError> {
        let existing = self.read()?;
        let records = plane.audit();
        if existing.len() > records.len()
            || existing
                .iter()
                .zip(records)
                .any(|(entry, record)| entry.record != *record)
        {
            return Err(StorageError::Conflict(
                "控制面审计与 PostgreSQL 审计前缀不一致".into(),
            ));
        }
        let mut appended = 0;
        for record in records.iter().skip(existing.len()) {
            self.append(record.clone())?;
            appended += 1;
        }
        Ok(appended)
    }
}

impl AuditStore for PostgresAuditStore {
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

fn read_audit<C: GenericClient>(client: &mut C) -> Result<Vec<AuditEntry>, StorageError> {
    let rows = client
        .query(
            "SELECT sequence, record_json, previous_hash, entry_hash
             FROM qx_audit_entries ORDER BY sequence::numeric",
            &[],
        )
        .map_err(pg_error)?;
    parse_audit_rows(rows)
}

fn parse_audit_rows(rows: Vec<postgres::Row>) -> Result<Vec<AuditEntry>, StorageError> {
    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let record: AuditRecord = serde_json::from_str(row.get(1))
            .map_err(|error| StorageError::Io(format!("PostgreSQL 审计 JSON 非法: {error}")))?;
        entries.push(AuditEntry {
            sequence: parse_u64(row.get(0), "sequence")?,
            record,
            previous_hash: parse_u64(row.get(2), "previous_hash")?,
            entry_hash: parse_u64(row.get(3), "entry_hash")?,
        });
    }
    validate_audit_chain(&entries)?;
    Ok(entries)
}

// ----------------------------- Snapshot ---------------------------------

#[derive(Clone, Debug)]
pub struct PostgresSnapshotStore {
    storage: PostgresStorage,
}

impl PostgresSnapshotStore {
    pub fn connect(dsn: &str) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect(dsn)?,
        })
    }

    pub fn connect_with_pool_size(dsn: &str, pool_size: usize) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect_with_pool_size(dsn, pool_size)?,
        })
    }

    pub fn from_storage(storage: PostgresStorage) -> Self {
        Self { storage }
    }
}

impl SnapshotStore for PostgresSnapshotStore {
    fn save(&self, snapshot: &AccountSnapshot) -> Result<PathBuf, ProtocolError> {
        snapshot.validate()?;
        let snapshot_id = u64_text(snapshot.header.snapshot_id);
        let state_hash = u64_text(snapshot.state_hash());
        let content = snapshot.to_json();
        let mut client = self.storage.lock_client_protocol()?;
        let mut transaction = client.transaction().map_err(pg_protocol_error)?;
        let existing: Option<String> = transaction
            .query_opt(
                "SELECT content FROM qx_snapshots
                 WHERE snapshot_id = $1 AND state_hash = $2",
                &[&snapshot_id, &state_hash],
            )
            .map_err(pg_protocol_error)?
            .map(|row| row.get(0));
        if let Some(existing) = existing {
            if existing == content {
                transaction.commit().map_err(pg_protocol_error)?;
                return Ok(marker_path("snapshots", snapshot.header.snapshot_id));
            }
            return Err(ProtocolError::StateHashMismatch);
        }
        transaction
            .execute(
                "INSERT INTO qx_snapshots(snapshot_id, state_hash, content)
                 VALUES ($1, $2, $3)",
                &[&snapshot_id, &state_hash, &content],
            )
            .map_err(pg_protocol_error)?;
        transaction.commit().map_err(pg_protocol_error)?;
        Ok(marker_path("snapshots", snapshot.header.snapshot_id))
    }

    fn load_json(&self, snapshot_id: u64, state_hash: u64) -> Result<String, ProtocolError> {
        let snapshot_id_text = u64_text(snapshot_id);
        let state_hash_text = u64_text(state_hash);
        let mut client = self.storage.lock_client_protocol()?;
        let content: String = client
            .query_opt(
                "SELECT content FROM qx_snapshots
                 WHERE snapshot_id = $1 AND state_hash = $2",
                &[&snapshot_id_text, &state_hash_text],
            )
            .map_err(pg_protocol_error)?
            .ok_or_else(|| ProtocolError::Io("PostgreSQL 快照不存在".into()))?
            .get(0);
        let snapshot = AccountSnapshot::from_json(&content)?;
        if snapshot.header.snapshot_id != snapshot_id || snapshot.state_hash() != state_hash {
            return Err(ProtocolError::StateHashMismatch);
        }
        Ok(content)
    }
}

// ----------------------------- Control state ----------------------------

#[derive(Clone, Debug)]
pub struct PostgresControlStore {
    storage: PostgresStorage,
}

impl PostgresControlStore {
    pub fn connect(dsn: &str) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect(dsn)?,
        })
    }

    pub fn connect_with_pool_size(dsn: &str, pool_size: usize) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect_with_pool_size(dsn, pool_size)?,
        })
    }

    pub fn from_storage(storage: PostgresStorage) -> Self {
        Self { storage }
    }

    pub fn load_if_exists(&self) -> Result<Option<ControlPlane>, StorageError> {
        let mut client = self.storage.lock_client()?;
        let content: Option<String> = client
            .query_opt(
                "SELECT content FROM qx_control_state WHERE state_id = 1",
                &[],
            )
            .map_err(pg_error)?
            .map(|row| row.get(0));
        content
            .map(|content| ControlPlane::from_json(&content).map_err(StorageError::Io))
            .transpose()
    }

    pub fn transact_control<T, E, F>(
        &self,
        update: F,
    ) -> Result<(ControlPlane, Result<T, E>), StorageError>
    where
        F: FnOnce(&mut ControlPlane) -> Result<T, E>,
    {
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, QUEUE_LOCK_KEY + 1)?;
        let content: Option<String> = transaction
            .query_opt(
                "SELECT content FROM qx_control_state WHERE state_id = 1",
                &[],
            )
            .map_err(pg_error)?
            .map(|row| row.get(0));
        let mut plane = match content {
            Some(content) => ControlPlane::from_json(&content).map_err(StorageError::Io)?,
            None => ControlPlane::default(),
        };
        let result = update(&mut plane);
        if result.is_ok() {
            let content = plane.to_json().map_err(StorageError::Io)?;
            transaction
                .execute(
                    "INSERT INTO qx_control_state(state_id, content) VALUES (1, $1)
                     ON CONFLICT(state_id) DO UPDATE SET content = EXCLUDED.content",
                    &[&content],
                )
                .map_err(pg_error)?;
        }
        transaction.commit().map_err(pg_error)?;
        Ok((plane, result))
    }
}

// ----------------------------- Control queue ----------------------------

#[derive(Clone, Debug)]
pub struct PostgresControlCommandQueue {
    storage: PostgresStorage,
}

impl PostgresControlCommandQueue {
    pub fn connect(dsn: &str) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect(dsn)?,
        })
    }

    pub fn connect_with_pool_size(dsn: &str, pool_size: usize) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect_with_pool_size(dsn, pool_size)?,
        })
    }

    pub fn from_storage(storage: PostgresStorage) -> Self {
        Self { storage }
    }

    pub fn enqueue(
        &self,
        command: ControlCommand,
        enqueued_ts: u64,
    ) -> Result<PathBuf, StorageError> {
        command
            .validate()
            .map_err(|error| StorageError::Conflict(format!("控制命令非法: {error:?}")))?;
        let command_id = u64_text(command.command_id);
        let json = serde_json::to_string(&command)
            .map_err(|error| StorageError::Io(format!("控制命令序列化失败: {error}")))?;
        let enqueued = u64_text(enqueued_ts);
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, QUEUE_LOCK_KEY)?;
        let existing: Option<String> = transaction
            .query_opt(
                "SELECT command_json FROM qx_control_commands WHERE command_id = $1",
                &[&command_id],
            )
            .map_err(pg_error)?
            .map(|row| row.get(0));
        if let Some(existing) = existing {
            let existing_command: ControlCommand = serde_json::from_str(&existing)
                .map_err(|error| StorageError::Io(format!("控制命令 JSON 非法: {error}")))?;
            if existing_command == command {
                transaction.commit().map_err(pg_error)?;
                return Ok(marker_path("control-commands", command.command_id));
            }
            return Err(StorageError::Conflict(format!(
                "command_id {} 已被不同命令占用",
                command.command_id
            )));
        }
        transaction
            .execute(
                "INSERT INTO qx_control_commands(command_id, command_json, enqueued_ts, done)
                 VALUES ($1, $2, $3, FALSE)",
                &[&command_id, &json, &enqueued],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)?;
        Ok(marker_path("control-commands", command.command_id))
    }

    pub fn available(&self, now: u64) -> Result<Vec<QueuedControlCommand>, StorageError> {
        let mut client = self.storage.lock_client()?;
        let rows = client
            .query(
                "SELECT c.command_json, c.enqueued_ts, l.expires_ts
                 FROM qx_control_commands c
                 LEFT JOIN qx_control_command_leases l ON l.command_id = c.command_id
                 WHERE c.done = FALSE
                 ORDER BY c.enqueued_ts::numeric, c.command_id::numeric",
                &[],
            )
            .map_err(pg_error)?;
        let mut commands = Vec::new();
        for row in rows {
            let expires: Option<String> = row.get(2);
            if expires
                .as_deref()
                .map(|value| parse_u64(value, "expires_ts"))
                .transpose()?
                .is_some_and(|value| value > now)
            {
                continue;
            }
            let command: ControlCommand = serde_json::from_str(row.get(0))
                .map_err(|error| StorageError::Io(format!("控制命令 JSON 非法: {error}")))?;
            command
                .validate()
                .map_err(|error| StorageError::Conflict(format!("队列控制命令非法: {error:?}")))?;
            commands.push(QueuedControlCommand {
                command,
                enqueued_ts: parse_u64(row.get(1), "enqueued_ts")?,
            });
        }
        Ok(commands)
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
        let command_id_text = u64_text(command_id);
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, QUEUE_LOCK_KEY)?;
        let exists = transaction
            .query_opt(
                "SELECT command_id FROM qx_control_commands
                 WHERE command_id = $1 AND done = FALSE",
                &[&command_id_text],
            )
            .map_err(pg_error)?
            .is_some();
        if !exists {
            return Err(StorageError::NotFound(format!("command_id {command_id}")));
        }
        let current: Option<(String, String, String)> = transaction
            .query_opt(
                "SELECT owner, expires_ts, fencing_token
                 FROM qx_control_command_leases WHERE command_id = $1 FOR UPDATE",
                &[&command_id_text],
            )
            .map_err(pg_error)?
            .map(|row| (row.get(0), row.get(1), row.get(2)));
        let (fencing_token, expires_ts) = match current {
            Some((current_owner, expires, token)) => {
                let expires = parse_u64(&expires, "expires_ts")?;
                let token = parse_u64(&token, "fencing_token")?;
                if expires > now && current_owner != owner {
                    return Err(StorageError::LeaseHeld {
                        run_id: command_id,
                        owner: current_owner,
                    });
                }
                if expires > now && current_owner == owner {
                    (token.max(1), now.saturating_add(lease_seconds))
                } else {
                    (
                        token.saturating_add(1).max(1),
                        now.saturating_add(lease_seconds),
                    )
                }
            }
            None => (1, now.saturating_add(lease_seconds)),
        };
        let expires_text = u64_text(expires_ts);
        let token_text = u64_text(fencing_token);
        transaction
            .execute(
                "INSERT INTO qx_control_command_leases(command_id, owner, expires_ts, fencing_token)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT(command_id) DO UPDATE SET owner = EXCLUDED.owner,
                     expires_ts = EXCLUDED.expires_ts, fencing_token = EXCLUDED.fencing_token",
                &[&command_id_text, &owner, &expires_text, &token_text],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)?;
        Ok(ControlCommandLease {
            command_id,
            owner: owner.into(),
            expires_ts,
            fencing_token,
        })
    }

    pub fn ack_at(
        &self,
        command_id: u64,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<PathBuf, StorageError> {
        let command_id_text = u64_text(command_id);
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, QUEUE_LOCK_KEY)?;
        let lease_row = transaction
            .query_opt(
                "SELECT owner, expires_ts, fencing_token
                 FROM qx_control_command_leases WHERE command_id = $1 FOR UPDATE",
                &[&command_id_text],
            )
            .map_err(pg_error)?
            .ok_or_else(|| StorageError::NotFound(format!("command_id {command_id} 租约")))?;
        let lease: (String, String, String) =
            (lease_row.get(0), lease_row.get(1), lease_row.get(2));
        let expires = parse_u64(&lease.1, "expires_ts")?;
        let token = parse_u64(&lease.2, "fencing_token")?;
        if lease.0 != owner || token != fencing_token {
            return Err(StorageError::Unauthorized(format!(
                "command_id {command_id} 的租约不属于 worker {owner}"
            )));
        }
        if expires <= now {
            return Err(StorageError::LeaseExpired { run_id: command_id });
        }
        let updated = transaction
            .execute(
                "UPDATE qx_control_commands SET done = TRUE
                 WHERE command_id = $1 AND done = FALSE",
                &[&command_id_text],
            )
            .map_err(pg_error)?;
        if updated != 1 {
            return Err(StorageError::NotFound(format!("command_id {command_id}")));
        }
        transaction
            .execute(
                "DELETE FROM qx_control_command_leases WHERE command_id = $1",
                &[&command_id_text],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)?;
        Ok(marker_path("control-commands", command_id))
    }
}

impl ControlCommandQueueBackend for PostgresControlCommandQueue {
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

// ----------------------------- Job queue --------------------------------

#[derive(Clone, Debug)]
pub struct PostgresJobQueue {
    storage: PostgresStorage,
}

impl PostgresJobQueue {
    pub fn connect(dsn: &str) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect(dsn)?,
        })
    }

    pub fn connect_with_pool_size(dsn: &str, pool_size: usize) -> Result<Self, StorageError> {
        Ok(Self {
            storage: PostgresStorage::connect_with_pool_size(dsn, pool_size)?,
        })
    }

    pub fn from_storage(storage: PostgresStorage) -> Self {
        Self { storage }
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
        let run_id = u64_text(envelope.run.run_id);
        let json = serde_json::to_string(&envelope)
            .map_err(|error| StorageError::Io(format!("任务序列化失败: {error}")))?;
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, QUEUE_LOCK_KEY + 2)?;
        let existing: Option<String> = transaction
            .query_opt(
                "SELECT envelope_json FROM qx_jobs WHERE run_id = $1 AND done = FALSE",
                &[&run_id],
            )
            .map_err(pg_error)?
            .map(|row| row.get(0));
        if let Some(existing) = existing {
            let old: QueuedJob = serde_json::from_str(&existing)
                .map_err(|error| StorageError::Io(format!("任务 JSON 非法: {error}")))?;
            if old.job == envelope.job && old.run == envelope.run {
                transaction.commit().map_err(pg_error)?;
                return Ok(marker_path("jobs", envelope.run.run_id));
            }
            return Err(StorageError::Conflict(format!(
                "run_id {} 已被不同任务占用",
                envelope.run.run_id
            )));
        }
        transaction
            .execute(
                "INSERT INTO qx_jobs(run_id, envelope_json, done) VALUES ($1, $2, FALSE)",
                &[&run_id, &json],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)?;
        Ok(marker_path("jobs", envelope.run.run_id))
    }

    pub fn available(&self, now: u64) -> Result<Vec<QueuedJob>, StorageError> {
        let mut client = self.storage.lock_client()?;
        let rows = client
            .query(
                "SELECT j.envelope_json, l.expires_ts
                 FROM qx_jobs j LEFT JOIN qx_job_leases l ON l.run_id = j.run_id
                 WHERE j.done = FALSE ORDER BY j.run_id::numeric",
                &[],
            )
            .map_err(pg_error)?;
        let mut jobs = Vec::new();
        for row in rows {
            let expires: Option<String> = row.get(1);
            if expires
                .as_deref()
                .map(|value| parse_u64(value, "expires_ts"))
                .transpose()?
                .is_some_and(|value| value > now)
            {
                continue;
            }
            let job: QueuedJob = serde_json::from_str(row.get(0))
                .map_err(|error| StorageError::Io(format!("任务 JSON 非法: {error}")))?;
            job.validate()?;
            jobs.push(job);
        }
        jobs.sort_by_key(|job| (job.run.trading_day.clone(), job.run.run_id));
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
        let run_id_text = u64_text(run_id);
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, QUEUE_LOCK_KEY + 2)?;
        let exists = transaction
            .query_opt(
                "SELECT run_id FROM qx_jobs WHERE run_id = $1 AND done = FALSE",
                &[&run_id_text],
            )
            .map_err(pg_error)?
            .is_some();
        if !exists {
            return Err(StorageError::NotFound(format!("run_id {run_id}")));
        }
        let current: Option<(String, String, String)> = transaction
            .query_opt(
                "SELECT owner, expires_ts, fencing_token
                 FROM qx_job_leases WHERE run_id = $1 FOR UPDATE",
                &[&run_id_text],
            )
            .map_err(pg_error)?
            .map(|row| (row.get(0), row.get(1), row.get(2)));
        let (fencing_token, expires_ts) = match current {
            Some((current_owner, expires, token)) => {
                let expires = parse_u64(&expires, "expires_ts")?;
                let token = parse_u64(&token, "fencing_token")?;
                if expires > now && current_owner != worker {
                    return Err(StorageError::LeaseHeld {
                        run_id,
                        owner: current_owner,
                    });
                }
                if expires > now && current_owner == worker {
                    (token.max(1), now.saturating_add(lease_seconds))
                } else {
                    (
                        token.saturating_add(1).max(1),
                        now.saturating_add(lease_seconds),
                    )
                }
            }
            None => (1, now.saturating_add(lease_seconds)),
        };
        let expires_text = u64_text(expires_ts);
        let token_text = u64_text(fencing_token);
        transaction
            .execute(
                "INSERT INTO qx_job_leases(run_id, owner, expires_ts, fencing_token)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT(run_id) DO UPDATE SET owner = EXCLUDED.owner,
                     expires_ts = EXCLUDED.expires_ts, fencing_token = EXCLUDED.fencing_token",
                &[&run_id_text, &worker, &expires_text, &token_text],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)?;
        Ok(JobLease {
            run_id,
            owner: worker.into(),
            expires_ts,
            fencing_token,
        })
    }

    pub fn ack_at(
        &self,
        run_id: u64,
        worker: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<PathBuf, StorageError> {
        let run_id_text = u64_text(run_id);
        let mut client = self.storage.lock_client()?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        lock_transaction(&mut transaction, QUEUE_LOCK_KEY + 2)?;
        let lease_row = transaction
            .query_opt(
                "SELECT owner, expires_ts, fencing_token
                 FROM qx_job_leases WHERE run_id = $1 FOR UPDATE",
                &[&run_id_text],
            )
            .map_err(pg_error)?
            .ok_or_else(|| StorageError::NotFound(format!("run_id {run_id} 租约")))?;
        let lease: (String, String, String) =
            (lease_row.get(0), lease_row.get(1), lease_row.get(2));
        let expires = parse_u64(&lease.1, "expires_ts")?;
        let token = parse_u64(&lease.2, "fencing_token")?;
        if lease.0 != worker || token != fencing_token {
            return Err(StorageError::Unauthorized(format!(
                "worker {} 的任务租约 fencing token 无效",
                worker
            )));
        }
        if expires <= now {
            return Err(StorageError::LeaseExpired { run_id });
        }
        let updated = transaction
            .execute(
                "UPDATE qx_jobs SET done = TRUE WHERE run_id = $1 AND done = FALSE",
                &[&run_id_text],
            )
            .map_err(pg_error)?;
        if updated != 1 {
            return Err(StorageError::NotFound(format!("run_id {run_id}")));
        }
        transaction
            .execute(
                "DELETE FROM qx_job_leases WHERE run_id = $1",
                &[&run_id_text],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)?;
        Ok(marker_path("jobs", run_id))
    }

    pub fn recover_expired(&self, now: u64) -> Result<Vec<u64>, StorageError> {
        let now = u64_text(now);
        let mut client = self.storage.lock_client()?;
        let rows = client
            .query(
                "SELECT run_id FROM qx_job_leases
                 WHERE expires_ts::numeric <= $1::numeric ORDER BY run_id::numeric",
                &[&now],
            )
            .map_err(pg_error)?;
        rows.into_iter()
            .map(|row| parse_u64(row.get(0), "run_id"))
            .collect()
    }
}

impl JobQueueBackend for PostgresJobQueue {
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
        let lease = self.storage.clone();
        let mut client = lease.lock_client()?;
        let row = client
            .query_opt(
                "SELECT fencing_token, expires_ts FROM qx_job_leases WHERE run_id = $1",
                &[&u64_text(run_id)],
            )
            .map_err(pg_error)?
            .ok_or_else(|| StorageError::NotFound(format!("run_id {run_id} 租约")))?;
        let token = parse_u64(row.get(0), "fencing_token")?;
        let now = parse_u64(row.get(1), "expires_ts")?.saturating_sub(1);
        drop(client);
        self.ack_at(run_id, worker, token, now)
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
