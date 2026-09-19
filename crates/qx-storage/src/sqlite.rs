//! SQLite 持久化后端。
//!
//! 该模块把文件后端已经验证过的审计链、幂等键、租约和 fencing token
//! 语义搬到事务数据库中。它不改变上层 trait，也不把 SQLite 类型泄漏到
//! Kernel 或调度器；生产环境可以在同一 trait 上替换为 PostgreSQL/MQ 实现。

use super::{
    audit_entry_hash, validate_audit_chain, AuditEntry, AuditStore, ConsumerCheckpoint,
    ConsumerProjection, ConsumerStateStore, ControlCommandLease, ControlCommandQueueBackend,
    DeadLetterRecord, EventLogStore, JobLease, JobQueueBackend, OutboxEvent, OutboxLease,
    OutboxStore, QueuedControlCommand, QueuedJob, StorageError, TransactionalConsumerStateStore,
};
use qx_control::{AuditRecord, ControlCommand, ControlPlane};
use qx_core::{Event, EventLog, QxResult};
use qx_protocol::{AccountSnapshot, ProtocolError, SnapshotStore};
use qx_scheduler::{JobRun, JobSpec};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn db_string(value: u64) -> String {
    value.to_string()
}

fn parse_db_u64(value: &str, field: &str) -> Result<u64, StorageError> {
    value
        .parse::<u64>()
        .map_err(|error| StorageError::Io(format!("SQLite {field} 非法: {error}")))
}

fn map_sqlite(error: rusqlite::Error) -> StorageError {
    StorageError::Io(format!("SQLite: {error}"))
}

fn parse_sqlite_u64(value: &str) -> Result<u64, rusqlite::Error> {
    value
        .parse::<u64>()
        .map_err(|_| rusqlite::Error::InvalidQuery)
}

fn open(path: &Path) -> Result<Connection, StorageError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| StorageError::Io(error.to_string()))?;
    }
    let connection = Connection::open(path).map_err(map_sqlite)?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS qx_audit_entries (
                 sequence INTEGER PRIMARY KEY NOT NULL,
                 record_json TEXT NOT NULL,
                 previous_hash TEXT NOT NULL,
                 entry_hash TEXT NOT NULL UNIQUE
             );
             CREATE TABLE IF NOT EXISTS qx_jobs (
                 run_id TEXT PRIMARY KEY NOT NULL,
                 envelope_json TEXT NOT NULL,
                 done INTEGER NOT NULL DEFAULT 0
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
                 done INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS qx_control_command_leases (
                 command_id TEXT PRIMARY KEY NOT NULL,
                 owner TEXT NOT NULL,
                 expires_ts TEXT NOT NULL,
                 fencing_token TEXT NOT NULL
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
                 attempts INTEGER NOT NULL,
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
             CREATE TABLE IF NOT EXISTS qx_event_log_entries (
                 name TEXT NOT NULL,
                 position INTEGER NOT NULL,
                 seq INTEGER NOT NULL,
                 event_ts TEXT NOT NULL,
                 prio INTEGER NOT NULL,
                 dedup_key TEXT NOT NULL DEFAULT '',
                 event_json TEXT NOT NULL,
                 PRIMARY KEY (name, position)
             );
             CREATE UNIQUE INDEX IF NOT EXISTS qx_event_log_entries_seq
                 ON qx_event_log_entries(name, seq);
             CREATE UNIQUE INDEX IF NOT EXISTS qx_event_log_entries_dedup
                 ON qx_event_log_entries(name, dedup_key) WHERE dedup_key <> '';
             CREATE TABLE IF NOT EXISTS qx_event_log_state (
                 name TEXT PRIMARY KEY NOT NULL,
                 event_count INTEGER NOT NULL,
                 next_seq TEXT NOT NULL,
                 digest TEXT NOT NULL
             );",
        )
        .map_err(map_sqlite)?;
    Ok(connection)
}

#[derive(Clone, Debug)]
pub struct SqliteAuditStore {
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct SqliteSnapshotStore {
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct SqliteControlStore {
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct SqliteControlCommandQueue {
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct SqliteOutboxStore {
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct SqliteConsumerStateStore {
    path: PathBuf,
}

/// SQLite 事务 EventLog：append-only 行存 + manifest 摘要校验。
#[derive(Clone, Debug)]
pub struct SqliteEventLogStore {
    path: PathBuf,
}

impl SqliteConsumerStateStore {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let store = Self { path: path.into() };
        let _ = open(&store.path)?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl ConsumerStateStore for SqliteConsumerStateStore {
    fn load_checkpoint(
        &self,
        group_id: &str,
        topic: &str,
        partition_key: &str,
    ) -> Result<Option<ConsumerCheckpoint>, StorageError> {
        let connection = open(&self.path)?;
        connection
            .query_row(
                "SELECT offset, event_id, updated_ts FROM qx_consumer_checkpoints
                 WHERE group_id = ?1 AND topic = ?2 AND partition_key = ?3",
                params![group_id, topic, partition_key],
                |row| {
                    Ok(ConsumerCheckpoint {
                        group_id: group_id.into(),
                        topic: topic.into(),
                        partition_key: partition_key.into(),
                        offset: parse_sqlite_u64(&row.get::<_, String>(0)?)?,
                        event_id: row.get(1)?,
                        updated_ts: parse_sqlite_u64(&row.get::<_, String>(2)?)?,
                    })
                },
            )
            .optional()
            .map_err(map_sqlite)
    }

    fn is_processed(&self, group_id: &str, event_id: &str) -> Result<bool, StorageError> {
        let connection = open(&self.path)?;
        connection
            .query_row(
                "SELECT 1 FROM qx_consumer_processed WHERE group_id = ?1 AND event_id = ?2",
                params![group_id, event_id],
                |_| Ok(()),
            )
            .optional()
            .map(|value| value.is_some())
            .map_err(map_sqlite)
    }

    fn commit_processed(&self, checkpoint: ConsumerCheckpoint) -> Result<(), StorageError> {
        checkpoint.validate()?;
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO qx_consumer_processed(group_id, event_id)
                 VALUES (?1, ?2)",
                params![checkpoint.group_id, checkpoint.event_id],
            )
            .map_err(map_sqlite)?;
        if inserted == 0 {
            transaction.commit().map_err(map_sqlite)?;
            return Ok(());
        }
        let previous: Option<(String, String)> = transaction
            .query_row(
                "SELECT offset, event_id FROM qx_consumer_checkpoints
                 WHERE group_id = ?1 AND topic = ?2 AND partition_key = ?3",
                params![
                    checkpoint.group_id,
                    checkpoint.topic,
                    checkpoint.partition_key
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(map_sqlite)?;
        if let Some((offset, event_id)) = previous {
            let previous_offset = parse_db_u64(&offset, "consumer.offset")?;
            if previous_offset > checkpoint.offset
                || (previous_offset == checkpoint.offset && event_id != checkpoint.event_id)
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
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(group_id, topic, partition_key) DO UPDATE SET
                   offset = excluded.offset, event_id = excluded.event_id,
                   updated_ts = excluded.updated_ts",
                params![
                    checkpoint.group_id,
                    checkpoint.topic,
                    checkpoint.partition_key,
                    db_string(checkpoint.offset),
                    checkpoint.event_id,
                    db_string(checkpoint.updated_ts),
                ],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)
    }

    fn append_dead_letter(&self, record: DeadLetterRecord) -> Result<(), StorageError> {
        record.validate()?;
        let connection = open(&self.path)?;
        connection
            .execute(
                "INSERT OR IGNORE INTO qx_consumer_dead_letters
                 (group_id, event_id, attempts, record_json) VALUES (?1, ?2, ?3, ?4)",
                params![
                    record.group_id,
                    record.event_id,
                    record.attempts as i64,
                    serde_json::to_string(&record)
                        .map_err(|error| StorageError::Io(error.to_string()))?,
                ],
            )
            .map_err(map_sqlite)?;
        Ok(())
    }

    fn dead_letters(
        &self,
        group_id: &str,
        limit: usize,
    ) -> Result<Vec<DeadLetterRecord>, StorageError> {
        let connection = open(&self.path)?;
        let mut statement = connection
            .prepare(
                "SELECT record_json FROM qx_consumer_dead_letters
                 WHERE group_id = ?1 ORDER BY attempts, event_id LIMIT ?2",
            )
            .map_err(map_sqlite)?;
        let rows = statement
            .query_map(params![group_id, limit as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(map_sqlite)?;
        rows.map(|row| {
            let content = row.map_err(map_sqlite)?;
            serde_json::from_str(&content)
                .map_err(|error| StorageError::Io(format!("死信记录解析失败: {error}")))
        })
        .collect()
    }
}

impl TransactionalConsumerStateStore for SqliteConsumerStateStore {
    fn commit_processed_with_projection(
        &self,
        checkpoint: ConsumerCheckpoint,
        projection: ConsumerProjection,
    ) -> Result<(), StorageError> {
        projection.validate_for(&checkpoint)?;
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO qx_consumer_processed(group_id, event_id)
                 VALUES (?1, ?2)",
                params![checkpoint.group_id, checkpoint.event_id],
            )
            .map_err(map_sqlite)?;
        if inserted == 0 {
            transaction.commit().map_err(map_sqlite)?;
            return Ok(());
        }
        let previous: Option<(String, String)> = transaction
            .query_row(
                "SELECT offset, event_id FROM qx_consumer_checkpoints
                 WHERE group_id = ?1 AND topic = ?2 AND partition_key = ?3",
                params![
                    checkpoint.group_id,
                    checkpoint.topic,
                    checkpoint.partition_key
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(map_sqlite)?;
        if let Some((offset, event_id)) = previous {
            let previous_offset = parse_db_u64(&offset, "consumer.offset")?;
            if previous_offset > checkpoint.offset
                || (previous_offset == checkpoint.offset && event_id != checkpoint.event_id)
            {
                return Err(StorageError::Conflict(
                    "consumer checkpoint 顺序或 event_id 非法".into(),
                ));
            }
        }
        let previous_projection: Option<(String, String)> = transaction
            .query_row(
                "SELECT offset, event_id FROM qx_consumer_projections
                 WHERE group_id = ?1 AND projection_key = ?2",
                params![projection.group_id, projection.projection_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(map_sqlite)?;
        if let Some((offset, event_id)) = previous_projection {
            let previous_offset = parse_db_u64(&offset, "consumer.projection.offset")?;
            if previous_offset > projection.offset
                || (previous_offset == projection.offset && event_id != projection.event_id)
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
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(group_id, projection_key) DO UPDATE SET
                   topic = excluded.topic, partition_key = excluded.partition_key,
                   offset = excluded.offset, event_id = excluded.event_id,
                   payload = excluded.payload, updated_ts = excluded.updated_ts",
                params![
                    projection.group_id,
                    projection.projection_key,
                    projection.topic,
                    projection.partition_key,
                    db_string(projection.offset),
                    projection.event_id,
                    projection.payload,
                    db_string(projection.updated_ts),
                ],
            )
            .map_err(map_sqlite)?;
        transaction
            .execute(
                "INSERT INTO qx_consumer_checkpoints
                 (group_id, topic, partition_key, offset, event_id, updated_ts)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(group_id, topic, partition_key) DO UPDATE SET
                   offset = excluded.offset, event_id = excluded.event_id,
                   updated_ts = excluded.updated_ts",
                params![
                    checkpoint.group_id,
                    checkpoint.topic,
                    checkpoint.partition_key,
                    db_string(checkpoint.offset),
                    checkpoint.event_id,
                    db_string(checkpoint.updated_ts),
                ],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)
    }

    fn append_dead_letter_and_commit(
        &self,
        record: DeadLetterRecord,
        checkpoint: ConsumerCheckpoint,
    ) -> Result<(), StorageError> {
        record.validate_for(&checkpoint)?;
        let content =
            serde_json::to_string(&record).map_err(|error| StorageError::Io(error.to_string()))?;
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO qx_consumer_processed(group_id, event_id)
                 VALUES (?1, ?2)",
                params![checkpoint.group_id, checkpoint.event_id],
            )
            .map_err(map_sqlite)?;
        if inserted == 0 {
            transaction.commit().map_err(map_sqlite)?;
            return Ok(());
        }
        let previous: Option<(String, String)> = transaction
            .query_row(
                "SELECT offset, event_id FROM qx_consumer_checkpoints
                 WHERE group_id = ?1 AND topic = ?2 AND partition_key = ?3",
                params![
                    checkpoint.group_id,
                    checkpoint.topic,
                    checkpoint.partition_key
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(map_sqlite)?;
        if let Some((offset, event_id)) = previous {
            let previous_offset = parse_db_u64(&offset, "consumer.offset")?;
            if previous_offset > checkpoint.offset
                || (previous_offset == checkpoint.offset && event_id != checkpoint.event_id)
            {
                return Err(StorageError::Conflict(
                    "consumer checkpoint 顺序或 event_id 非法".into(),
                ));
            }
        }
        transaction
            .execute(
                "INSERT OR IGNORE INTO qx_consumer_dead_letters
                 (group_id, event_id, attempts, record_json) VALUES (?1, ?2, ?3, ?4)",
                params![
                    record.group_id,
                    record.event_id,
                    record.attempts as i64,
                    content,
                ],
            )
            .map_err(map_sqlite)?;
        transaction
            .execute(
                "INSERT INTO qx_consumer_checkpoints
                 (group_id, topic, partition_key, offset, event_id, updated_ts)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(group_id, topic, partition_key) DO UPDATE SET
                   offset = excluded.offset, event_id = excluded.event_id,
                   updated_ts = excluded.updated_ts",
                params![
                    checkpoint.group_id,
                    checkpoint.topic,
                    checkpoint.partition_key,
                    db_string(checkpoint.offset),
                    checkpoint.event_id,
                    db_string(checkpoint.updated_ts),
                ],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)
    }

    fn load_projection(
        &self,
        group_id: &str,
        projection_key: &str,
    ) -> Result<Option<ConsumerProjection>, StorageError> {
        let connection = open(&self.path)?;
        connection
            .query_row(
                "SELECT topic, partition_key, offset, event_id, payload, updated_ts
                 FROM qx_consumer_projections
                 WHERE group_id = ?1 AND projection_key = ?2",
                params![group_id, projection_key],
                |row| {
                    Ok(ConsumerProjection {
                        group_id: group_id.into(),
                        projection_key: projection_key.into(),
                        topic: row.get(0)?,
                        partition_key: row.get(1)?,
                        offset: parse_sqlite_u64(&row.get::<_, String>(2)?)?,
                        event_id: row.get(3)?,
                        payload: row.get(4)?,
                        updated_ts: parse_sqlite_u64(&row.get::<_, String>(5)?)?,
                    })
                },
            )
            .optional()
            .map_err(map_sqlite)
    }
}

impl SqliteOutboxStore {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let store = Self { path: path.into() };
        let _ = open(&store.path)?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, event: OutboxEvent) -> Result<(), StorageError> {
        event.validate()?;
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        append_outbox_event(&transaction, &event)?;
        transaction.commit().map_err(map_sqlite)
    }

    pub fn available(&self, now: u64) -> Result<Vec<OutboxEvent>, StorageError> {
        let connection = open(&self.path)?;
        let mut statement = connection
            .prepare(
                "SELECT e.event_id, e.topic, e.partition_key, e.sequence, e.schema_version,
                        e.trace_id, e.payload, e.created_ts, e.attempts
                 FROM qx_outbox_events e
                 LEFT JOIN qx_outbox_leases l ON l.event_id = e.event_id
                 WHERE l.event_id IS NULL OR CAST(l.expires_ts AS INTEGER) <= CAST(?1 AS INTEGER)
                 ORDER BY CAST(e.created_ts AS INTEGER), CAST(e.sequence AS INTEGER), e.event_id",
            )
            .map_err(map_sqlite)?;
        let rows = statement
            .query_map(params![db_string(now)], |row| {
                Ok(OutboxEvent {
                    event_id: row.get(0)?,
                    topic: row.get(1)?,
                    partition_key: row.get(2)?,
                    sequence: parse_sqlite_u64(&row.get::<_, String>(3)?)?,
                    schema_version: row.get::<_, i64>(4)? as u32,
                    trace_id: row.get(5)?,
                    payload: row.get(6)?,
                    created_ts: parse_sqlite_u64(&row.get::<_, String>(7)?)?,
                    attempts: parse_sqlite_u64(&row.get::<_, String>(8)?)? as u32,
                })
            })
            .map_err(map_sqlite)?;
        rows.map(|row| {
            let event = row.map_err(map_sqlite)?;
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
        validate_sqlite_outbox_name(event_id)?;
        if owner.trim().is_empty() || lease_seconds == 0 {
            return Err(StorageError::Conflict(
                "Outbox worker 和租约时长不能为空".into(),
            ));
        }
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let exists: Option<String> = transaction
            .query_row(
                "SELECT event_id FROM qx_outbox_events WHERE event_id = ?1",
                params![event_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sqlite)?;
        if exists.is_none() {
            return Err(StorageError::NotFound(format!(
                "Outbox event_id {event_id}"
            )));
        }
        let current: Option<(String, String, String)> = transaction
            .query_row(
                "SELECT owner, expires_ts, fencing_token FROM qx_outbox_leases WHERE event_id = ?1",
                params![event_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(map_sqlite)?;
        let mut fencing_token = 1;
        if let Some((current_owner, expires, token)) = current {
            let expires_ts = parse_db_u64(&expires, "outbox.expires_ts")?;
            let current_token = parse_db_u64(&token, "outbox.fencing_token")?;
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
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(event_id) DO UPDATE SET owner = excluded.owner,
                   expires_ts = excluded.expires_ts, fencing_token = excluded.fencing_token",
                params![
                    lease.event_id,
                    lease.owner,
                    db_string(lease.expires_ts),
                    db_string(lease.fencing_token),
                ],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)?;
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
        validate_sqlite_outbox_name(event_id)?;
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let lease: (String, String, String) = transaction
            .query_row(
                "SELECT owner, expires_ts, fencing_token FROM qx_outbox_leases WHERE event_id = ?1",
                params![event_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => {
                    StorageError::NotFound(format!("Outbox lease {event_id}"))
                }
                other => map_sqlite(other),
            })?;
        let expires_ts = parse_db_u64(&lease.1, "outbox.expires_ts")?;
        let current_token = parse_db_u64(&lease.2, "outbox.fencing_token")?;
        if lease.0 != owner || current_token != fencing_token {
            return Err(StorageError::Unauthorized(format!(
                "Outbox event_id {event_id} 的 worker 或 fencing token 无效"
            )));
        }
        if expires_ts <= now {
            return Err(StorageError::LeaseExpired { run_id: 0 });
        }
        if retry {
            transaction
                .execute(
                    "UPDATE qx_outbox_events SET attempts = CAST(CAST(attempts AS INTEGER) + 1 AS TEXT)
                     WHERE event_id = ?1",
                    params![event_id],
                )
                .map_err(map_sqlite)?;
        } else {
            transaction
                .execute(
                    "DELETE FROM qx_outbox_events WHERE event_id = ?1",
                    params![event_id],
                )
                .map_err(map_sqlite)?;
        }
        transaction
            .execute(
                "DELETE FROM qx_outbox_leases WHERE event_id = ?1",
                params![event_id],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)
    }
}

/// 在调用方事务中幂等追加一条 Outbox 事实。
///
/// `event_id` 主键就是重试幂等键；同一 `event_id` 携带不同事实时按冲突处理，
/// 与 `SqliteOutboxStore::append` 和 PostgreSQL 的 `append_outbox_in_transaction`
/// 保持同一条语义。调用方必须先 `event.validate()`。
fn append_outbox_event(connection: &Connection, event: &OutboxEvent) -> Result<(), StorageError> {
    let existing: Option<OutboxEvent> = connection
        .query_row(
            "SELECT topic, partition_key, sequence, schema_version, trace_id, payload, created_ts, attempts
             FROM qx_outbox_events WHERE event_id = ?1",
            params![event.event_id],
            |row| {
                Ok(OutboxEvent {
                    event_id: event.event_id.clone(),
                    topic: row.get(0)?,
                    partition_key: row.get(1)?,
                    sequence: parse_sqlite_u64(&row.get::<_, String>(2)?)?,
                    schema_version: row.get::<_, i64>(3)? as u32,
                    trace_id: row.get(4)?,
                    payload: row.get(5)?,
                    created_ts: parse_sqlite_u64(&row.get::<_, String>(6)?)?,
                    attempts: parse_sqlite_u64(&row.get::<_, String>(7)?)? as u32,
                })
            },
        )
        .optional()
        .map_err(map_sqlite)?;
    if let Some(existing) = existing {
        if existing.same_fact(event) {
            return Ok(());
        }
        return Err(StorageError::Conflict(format!(
            "event_id {} 已被不同 Outbox 事件占用",
            event.event_id
        )));
    }
    connection
        .execute(
            "INSERT INTO qx_outbox_events
             (event_id, topic, partition_key, sequence, schema_version, trace_id, payload, created_ts, attempts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                event.event_id,
                event.topic,
                event.partition_key,
                db_string(event.sequence),
                event.schema_version as i64,
                event.trace_id,
                event.payload,
                db_string(event.created_ts),
                db_string(event.attempts as u64),
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn validate_sqlite_outbox_name(event_id: &str) -> Result<(), StorageError> {
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

impl OutboxStore for SqliteOutboxStore {
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

impl SqliteSnapshotStore {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let store = Self { path: path.into() };
        let connection = open(&store.path)?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS qx_snapshots (
                    snapshot_id TEXT NOT NULL,
                    state_hash TEXT NOT NULL,
                    content TEXT NOT NULL,
                    PRIMARY KEY(snapshot_id, state_hash)
                );",
            )
            .map_err(map_sqlite)?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn save(&self, snapshot: &AccountSnapshot) -> Result<PathBuf, ProtocolError> {
        snapshot.validate()?;
        let mut connection = open(&self.path).map_err(storage_protocol_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| ProtocolError::Io(format!("SQLite: {error}")))?;
        let snapshot_id = db_string(snapshot.header.snapshot_id);
        let state_hash = db_string(snapshot.state_hash());
        let content = snapshot.to_json();
        let existing: Option<String> = transaction
            .query_row(
                "SELECT content FROM qx_snapshots
                 WHERE snapshot_id = ?1 AND state_hash = ?2",
                params![snapshot_id, state_hash],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| ProtocolError::Io(format!("SQLite: {error}")))?;
        if let Some(existing) = existing {
            if existing == content {
                transaction
                    .commit()
                    .map_err(|error| ProtocolError::Io(format!("SQLite: {error}")))?;
                return Ok(self.path.clone());
            }
            return Err(ProtocolError::StateHashMismatch);
        }
        transaction
            .execute(
                "INSERT INTO qx_snapshots(snapshot_id, state_hash, content)
                 VALUES (?1, ?2, ?3)",
                params![snapshot_id, state_hash, content],
            )
            .map_err(|error| ProtocolError::Io(format!("SQLite: {error}")))?;
        transaction
            .commit()
            .map_err(|error| ProtocolError::Io(format!("SQLite: {error}")))?;
        Ok(self.path.clone())
    }

    pub fn load_json(&self, snapshot_id: u64, state_hash: u64) -> Result<String, ProtocolError> {
        let connection = open(&self.path).map_err(storage_protocol_error)?;
        let content: String = connection
            .query_row(
                "SELECT content FROM qx_snapshots
                 WHERE snapshot_id = ?1 AND state_hash = ?2",
                params![db_string(snapshot_id), db_string(state_hash)],
                |row| row.get(0),
            )
            .map_err(|error| ProtocolError::Io(format!("SQLite: {error}")))?;
        let snapshot = AccountSnapshot::from_json(&content)?;
        if snapshot.header.snapshot_id != snapshot_id || snapshot.state_hash() != state_hash {
            return Err(ProtocolError::StateHashMismatch);
        }
        Ok(content)
    }
}

fn storage_protocol_error(error: StorageError) -> ProtocolError {
    ProtocolError::Io(format!("存储失败: {error:?}"))
}

impl SqliteControlStore {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let store = Self { path: path.into() };
        let _ = open(&store.path)?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_if_exists(&self) -> Result<Option<ControlPlane>, StorageError> {
        let connection = open(&self.path)?;
        let content: Option<String> = connection
            .query_row(
                "SELECT content FROM qx_control_state WHERE state_id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sqlite)?;
        content
            .map(|content| ControlPlane::from_json(&content).map_err(StorageError::Io))
            .transpose()
    }

    /// SQLite Immediate transaction version of the file control-state update.
    /// Business errors are returned untouched and do not commit a partial state.
    pub fn transact_control<T, E, F>(
        &self,
        update: F,
    ) -> Result<(ControlPlane, Result<T, E>), StorageError>
    where
        F: FnOnce(&mut ControlPlane) -> Result<T, E>,
    {
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let content: Option<String> = transaction
            .query_row(
                "SELECT content FROM qx_control_state WHERE state_id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sqlite)?;
        let mut plane = match content {
            Some(content) => ControlPlane::from_json(&content).map_err(StorageError::Io)?,
            None => ControlPlane::default(),
        };
        let result = update(&mut plane);
        if result.is_ok() {
            let content = plane.to_json().map_err(StorageError::Io)?;
            transaction
                .execute(
                    "INSERT INTO qx_control_state(state_id, content)
                     VALUES (1, ?1)
                     ON CONFLICT(state_id) DO UPDATE SET content = excluded.content",
                    params![content],
                )
                .map_err(map_sqlite)?;
        }
        transaction.commit().map_err(map_sqlite)?;
        Ok((plane, result))
    }
}

impl SqliteControlCommandQueue {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let queue = Self { path: path.into() };
        let _ = open(&queue.path)?;
        Ok(queue)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn enqueue(
        &self,
        command: ControlCommand,
        enqueued_ts: u64,
    ) -> Result<PathBuf, StorageError> {
        command
            .validate()
            .map_err(|error| StorageError::Conflict(format!("控制命令非法: {error:?}")))?;
        let json = serde_json::to_string(&command)
            .map_err(|error| StorageError::Io(format!("控制命令序列化失败: {error}")))?;
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let key = db_string(command.command_id);
        let existing: Option<(String, i64)> = transaction
            .query_row(
                "SELECT command_json, done FROM qx_control_commands WHERE command_id = ?1",
                params![key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(map_sqlite)?;
        if let Some((existing, _done)) = existing {
            let existing_command: ControlCommand = serde_json::from_str(&existing)
                .map_err(|error| StorageError::Io(format!("控制命令 JSON 非法: {error}")))?;
            if existing_command == command {
                transaction.commit().map_err(map_sqlite)?;
                return Ok(self.path.clone());
            }
            return Err(StorageError::Conflict(format!(
                "command_id {} 已被不同命令占用",
                command.command_id
            )));
        }
        transaction
            .execute(
                "INSERT INTO qx_control_commands
                 (command_id, command_json, enqueued_ts, done)
                 VALUES (?1, ?2, ?3, 0)",
                params![key, json, db_string(enqueued_ts)],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)?;
        Ok(self.path.clone())
    }

    pub fn available(&self, now: u64) -> Result<Vec<QueuedControlCommand>, StorageError> {
        let connection = open(&self.path)?;
        let mut statement = connection
            .prepare(
                "SELECT c.command_json, c.enqueued_ts, l.expires_ts
                 FROM qx_control_commands c
                 LEFT JOIN qx_control_command_leases l ON l.command_id = c.command_id
                 WHERE c.done = 0
                 ORDER BY c.enqueued_ts, c.command_id",
            )
            .map_err(map_sqlite)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(map_sqlite)?;
        let mut commands = Vec::new();
        for row in rows {
            let (json, enqueued_ts, expires_ts) = row.map_err(map_sqlite)?;
            if expires_ts
                .as_deref()
                .map(|expires| parse_db_u64(expires, "expires_ts"))
                .transpose()?
                .map(|expires| expires > now)
                .unwrap_or(false)
            {
                continue;
            }
            let command: ControlCommand = serde_json::from_str(&json)
                .map_err(|error| StorageError::Io(format!("控制命令 JSON 非法: {error}")))?;
            command
                .validate()
                .map_err(|error| StorageError::Conflict(format!("控制命令非法: {error:?}")))?;
            commands.push(QueuedControlCommand {
                command,
                enqueued_ts: parse_db_u64(&enqueued_ts, "enqueued_ts")?,
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
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let key = db_string(command_id);
        let exists: Option<i64> = transaction
            .query_row(
                "SELECT done FROM qx_control_commands WHERE command_id = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sqlite)?;
        if exists != Some(0) {
            return Err(StorageError::NotFound(format!("command_id {command_id}")));
        }
        let current: Option<(String, String, String)> = transaction
            .query_row(
                "SELECT owner, expires_ts, fencing_token
                 FROM qx_control_command_leases WHERE command_id = ?1",
                params![key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(map_sqlite)?;
        let (fencing_token, expires_ts) = match current {
            Some((current_owner, expires, token)) => {
                let expires = parse_db_u64(&expires, "expires_ts")?;
                let token = parse_db_u64(&token, "fencing_token")?.max(1);
                if expires > now && current_owner != owner {
                    return Err(StorageError::LeaseHeld {
                        run_id: command_id,
                        owner: current_owner,
                    });
                }
                (
                    if expires <= now {
                        token.saturating_add(1).max(1)
                    } else {
                        token
                    },
                    now.saturating_add(lease_seconds),
                )
            }
            None => (1, now.saturating_add(lease_seconds)),
        };
        transaction
            .execute(
                "INSERT INTO qx_control_command_leases
                 (command_id, owner, expires_ts, fencing_token)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(command_id) DO UPDATE SET
                    owner = excluded.owner,
                    expires_ts = excluded.expires_ts,
                    fencing_token = excluded.fencing_token",
                params![key, owner, db_string(expires_ts), db_string(fencing_token)],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)?;
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
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let key = db_string(command_id);
        let lease: (String, String, String) = transaction
            .query_row(
                "SELECT owner, expires_ts, fencing_token
                 FROM qx_control_command_leases WHERE command_id = ?1",
                params![key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(map_sqlite)?;
        let expires = parse_db_u64(&lease.1, "expires_ts")?;
        let stored_token = parse_db_u64(&lease.2, "fencing_token")?;
        if expires <= now {
            return Err(StorageError::LeaseExpired { run_id: command_id });
        }
        if lease.0 != owner || stored_token != fencing_token {
            return Err(StorageError::Unauthorized(format!(
                "worker {} 的控制命令租约 fencing token 无效",
                owner
            )));
        }
        let changed = transaction
            .execute(
                "UPDATE qx_control_commands SET done = 1 WHERE command_id = ?1 AND done = 0",
                params![key],
            )
            .map_err(map_sqlite)?;
        if changed == 0 {
            return Err(StorageError::NotFound(format!("command_id {command_id}")));
        }
        transaction
            .execute(
                "DELETE FROM qx_control_command_leases WHERE command_id = ?1",
                params![key],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)?;
        Ok(self.path.clone())
    }
}

impl SnapshotStore for SqliteSnapshotStore {
    fn save(&self, snapshot: &AccountSnapshot) -> Result<PathBuf, ProtocolError> {
        Self::save(self, snapshot)
    }

    fn load_json(&self, snapshot_id: u64, state_hash: u64) -> Result<String, ProtocolError> {
        Self::load_json(self, snapshot_id, state_hash)
    }
}

impl SqliteAuditStore {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let store = Self { path: path.into() };
        let _ = open(&store.path)?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, record: AuditRecord) -> Result<PathBuf, StorageError> {
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let last: Option<(i64, String, String)> = transaction
            .query_row(
                "SELECT sequence, previous_hash, entry_hash
                 FROM qx_audit_entries ORDER BY sequence DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(map_sqlite)?;
        let (sequence, previous_hash) = last
            .as_ref()
            .map_or((0, "0".into()), |(sequence, _, hash)| {
                (sequence + 1, hash.clone())
            });
        if let Some((_, record_json, _)) = transaction
            .query_row(
                "SELECT sequence, record_json, entry_hash
                 FROM qx_audit_entries ORDER BY sequence DESC LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(map_sqlite)?
        {
            let current: AuditRecord = serde_json::from_str(&record_json)
                .map_err(|error| StorageError::Io(format!("审计 JSON 非法: {error}")))?;
            if current == record {
                transaction.commit().map_err(map_sqlite)?;
                return Ok(self.path.clone());
            }
        }
        let previous_hash_u64 = parse_db_u64(&previous_hash, "previous_hash")?;
        let entry_hash = audit_entry_hash(sequence as u64, previous_hash_u64, &record);
        let record_json = serde_json::to_string(&record)
            .map_err(|error| StorageError::Io(format!("审计序列化失败: {error}")))?;
        transaction
            .execute(
                "INSERT INTO qx_audit_entries
                 (sequence, record_json, previous_hash, entry_hash)
                 VALUES (?1, ?2, ?3, ?4)",
                params![sequence, record_json, previous_hash, db_string(entry_hash)],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)?;
        Ok(self.path.clone())
    }

    pub fn read(&self) -> Result<Vec<AuditEntry>, StorageError> {
        let connection = open(&self.path)?;
        let mut statement = connection
            .prepare(
                "SELECT sequence, record_json, previous_hash, entry_hash
                 FROM qx_audit_entries ORDER BY sequence ASC",
            )
            .map_err(map_sqlite)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(map_sqlite)?;
        let mut entries = Vec::new();
        for row in rows {
            let (sequence, record_json, previous_hash, entry_hash) = row.map_err(map_sqlite)?;
            let record = serde_json::from_str(&record_json)
                .map_err(|error| StorageError::Io(format!("审计 JSON 非法: {error}")))?;
            entries.push(AuditEntry {
                sequence: u64::try_from(sequence)
                    .map_err(|_| StorageError::Conflict("审计 sequence 为负数".into()))?,
                record,
                previous_hash: parse_db_u64(&previous_hash, "previous_hash")?,
                entry_hash: parse_db_u64(&entry_hash, "entry_hash")?,
            });
        }
        validate_audit_chain(&entries)?;
        Ok(entries)
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
                "控制面审计与 SQLite 审计前缀不一致".into(),
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

impl AuditStore for SqliteAuditStore {
    fn append_record(&self, record: AuditRecord) -> Result<PathBuf, StorageError> {
        self.append(record)
    }

    fn read_entries(&self) -> Result<Vec<AuditEntry>, StorageError> {
        self.read()
    }

    fn query_command_entries(&self, command_id: u64) -> Result<Vec<AuditEntry>, StorageError> {
        Ok(self
            .read()?
            .into_iter()
            .filter(|entry| entry.record.command_id == command_id)
            .collect())
    }

    fn entries_after(&self, sequence: u64) -> Result<Vec<AuditEntry>, StorageError> {
        Ok(self
            .read()?
            .into_iter()
            .filter(|entry| entry.sequence > sequence)
            .collect())
    }
}

#[derive(Clone, Debug)]
pub struct SqliteJobQueue {
    path: PathBuf,
}

/// SQLite 事务令牌桶，适用于多个 API 进程共享的强一致限流状态。
#[derive(Clone, Debug)]
pub struct SqliteTokenBucket {
    path: PathBuf,
    name: String,
    capacity: u64,
    refill_per_second: u64,
}

impl SqliteTokenBucket {
    pub fn new(
        path: impl Into<PathBuf>,
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
        let bucket = Self {
            path: path.into(),
            name,
            capacity,
            refill_per_second,
        };
        let connection = open(&bucket.path)?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS qx_token_buckets (
                    name TEXT PRIMARY KEY NOT NULL,
                    tokens TEXT NOT NULL,
                    last_ts TEXT NOT NULL
                );",
            )
            .map_err(map_sqlite)?;
        Ok(bucket)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn try_acquire(&self, now: u64, weight: u64) -> Result<bool, StorageError> {
        if weight == 0 || weight > self.capacity {
            return Err(StorageError::Conflict(
                "令牌桶请求权重必须在 1..=capacity 内".into(),
            ));
        }
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let current: Option<(String, String)> = transaction
            .query_row(
                "SELECT tokens, last_ts FROM qx_token_buckets WHERE name = ?1",
                params![self.name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(map_sqlite)?;
        let (mut tokens, last_ts) = match current {
            Some((tokens, last_ts)) => (
                parse_db_u64(&tokens, "tokens")?,
                parse_db_u64(&last_ts, "last_ts")?,
            ),
            None => (self.capacity, now),
        };
        if tokens > self.capacity {
            return Err(StorageError::Conflict("令牌桶余额超过容量".into()));
        }
        let elapsed = now.saturating_sub(last_ts);
        tokens = tokens
            .saturating_add(elapsed.saturating_mul(self.refill_per_second))
            .min(self.capacity);
        let granted = tokens >= weight;
        if granted {
            tokens -= weight;
        }
        transaction
            .execute(
                "INSERT INTO qx_token_buckets(name, tokens, last_ts)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(name) DO UPDATE SET
                    tokens = excluded.tokens,
                    last_ts = excluded.last_ts",
                params![self.name, db_string(tokens), db_string(now)],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)?;
        Ok(granted)
    }
}

impl ControlCommandQueueBackend for SqliteControlCommandQueue {
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

impl SqliteJobQueue {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let queue = Self { path: path.into() };
        let _ = open(&queue.path)?;
        Ok(queue)
    }

    pub fn path(&self) -> &Path {
        &self.path
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
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let run_id = db_string(envelope.run.run_id);
        let existing: Option<String> = transaction
            .query_row(
                "SELECT envelope_json FROM qx_jobs WHERE run_id = ?1 AND done = 0",
                params![run_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sqlite)?;
        let json = serde_json::to_string(&envelope)
            .map_err(|error| StorageError::Io(format!("任务序列化失败: {error}")))?;
        if let Some(existing) = existing {
            let existing_envelope: QueuedJob = serde_json::from_str(&existing)
                .map_err(|error| StorageError::Io(format!("任务 JSON 非法: {error}")))?;
            if existing_envelope.job == envelope.job && existing_envelope.run == envelope.run {
                transaction.commit().map_err(map_sqlite)?;
                return Ok(self.path.clone());
            }
            return Err(StorageError::Conflict(format!(
                "run_id {} 已被不同任务占用",
                envelope.run.run_id
            )));
        }
        transaction
            .execute(
                "INSERT INTO qx_jobs(run_id, envelope_json, done) VALUES (?1, ?2, 0)",
                params![run_id, json],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)?;
        Ok(self.path.clone())
    }

    pub fn available(&self, now: u64) -> Result<Vec<QueuedJob>, StorageError> {
        let connection = open(&self.path)?;
        let mut statement = connection
            .prepare(
                "SELECT j.envelope_json, l.expires_ts
                 FROM qx_jobs j LEFT JOIN qx_job_leases l ON l.run_id = j.run_id
                 WHERE j.done = 0 ORDER BY j.run_id",
            )
            .map_err(map_sqlite)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(map_sqlite)?;
        let mut jobs = Vec::new();
        for row in rows {
            let (json, expires_ts) = row.map_err(map_sqlite)?;
            let available = match expires_ts {
                None => true,
                Some(expires) => parse_db_u64(&expires, "expires_ts")? <= now,
            };
            if available {
                let job: QueuedJob = serde_json::from_str(&json)
                    .map_err(|error| StorageError::Io(format!("任务 JSON 非法: {error}")))?;
                job.validate()?;
                jobs.push(job);
            }
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
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let run_key = db_string(run_id);
        let envelope: Option<String> = transaction
            .query_row(
                "SELECT envelope_json FROM qx_jobs WHERE run_id = ?1 AND done = 0",
                params![run_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sqlite)?;
        let envelope =
            envelope.ok_or_else(|| StorageError::NotFound(format!("run_id {run_id}")))?;
        let queued: QueuedJob = serde_json::from_str(&envelope)
            .map_err(|error| StorageError::Io(format!("任务 JSON 非法: {error}")))?;
        queued.validate()?;
        let current: Option<(String, String, String)> = transaction
            .query_row(
                "SELECT owner, expires_ts, fencing_token FROM qx_job_leases WHERE run_id = ?1",
                params![run_key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(map_sqlite)?;
        let (fencing_token, expires_ts) = match current {
            Some((owner, expires, token)) => {
                let expires = parse_db_u64(&expires, "expires_ts")?;
                let token = parse_db_u64(&token, "fencing_token")?.max(1);
                if expires > now && owner != worker {
                    return Err(StorageError::LeaseHeld { run_id, owner });
                }
                let next_token = if expires <= now {
                    token.saturating_add(1).max(1)
                } else {
                    token
                };
                (next_token, now.saturating_add(lease_seconds))
            }
            None => (1, now.saturating_add(lease_seconds)),
        };
        transaction
            .execute(
                "INSERT INTO qx_job_leases(run_id, owner, expires_ts, fencing_token)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(run_id) DO UPDATE SET
                    owner = excluded.owner,
                    expires_ts = excluded.expires_ts,
                    fencing_token = excluded.fencing_token",
                params![
                    run_key,
                    worker,
                    db_string(expires_ts),
                    db_string(fencing_token)
                ],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)?;
        Ok(JobLease {
            run_id,
            owner: worker.into(),
            expires_ts,
            fencing_token,
        })
    }

    pub fn ack(&self, run_id: u64, worker: &str) -> Result<PathBuf, StorageError> {
        let connection = open(&self.path)?;
        let lease: (String, String) = connection
            .query_row(
                "SELECT owner, fencing_token FROM qx_job_leases WHERE run_id = ?1",
                params![db_string(run_id)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(map_sqlite)?;
        if lease.0 != worker {
            return Err(StorageError::Unauthorized(format!(
                "worker {} 不能确认 worker {} 的任务",
                worker, lease.0
            )));
        }
        self.ack_at(
            run_id,
            worker,
            parse_db_u64(&lease.1, "fencing_token")?,
            u64::MAX,
        )
    }

    pub fn ack_at(
        &self,
        run_id: u64,
        worker: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<PathBuf, StorageError> {
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let lease: (String, String, String) = transaction
            .query_row(
                "SELECT owner, expires_ts, fencing_token FROM qx_job_leases WHERE run_id = ?1",
                params![db_string(run_id)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(map_sqlite)?;
        let expires_ts = parse_db_u64(&lease.1, "expires_ts")?;
        let stored_token = parse_db_u64(&lease.2, "fencing_token")?;
        if expires_ts <= now && now != u64::MAX {
            return Err(StorageError::LeaseExpired { run_id });
        }
        if lease.0 != worker || stored_token != fencing_token {
            return Err(StorageError::Unauthorized(format!(
                "worker {} 的租约 fencing token 无效",
                worker
            )));
        }
        let changed = transaction
            .execute(
                "UPDATE qx_jobs SET done = 1 WHERE run_id = ?1 AND done = 0",
                params![db_string(run_id)],
            )
            .map_err(map_sqlite)?;
        if changed == 0 {
            return Err(StorageError::NotFound(format!("run_id {run_id}")));
        }
        transaction
            .execute(
                "DELETE FROM qx_job_leases WHERE run_id = ?1",
                params![db_string(run_id)],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)?;
        Ok(self.path.clone())
    }

    pub fn recover_expired(&self, now: u64) -> Result<Vec<u64>, StorageError> {
        let connection = open(&self.path)?;
        let mut statement = connection
            .prepare("SELECT run_id, expires_ts FROM qx_job_leases ORDER BY run_id")
            .map_err(map_sqlite)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(map_sqlite)?;
        let mut recovered = Vec::new();
        for row in rows {
            let (run_id, expires_ts) = row.map_err(map_sqlite)?;
            if parse_db_u64(&expires_ts, "expires_ts")? <= now {
                recovered.push(parse_db_u64(&run_id, "run_id")?);
            }
        }
        recovered.sort_unstable();
        Ok(recovered)
    }
}

impl JobQueueBackend for SqliteJobQueue {
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

// ----------------------------- Event Log ---------------------------------

/// EventLog 名称沿用文件/分段/PostgreSQL 后端的同一套白名单规则，避免同一
/// 运行时日志名在不同后端之间被降级接受。
fn validate_event_log_name(name: &str) -> Result<(), StorageError> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || !name
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
    {
        return Err(StorageError::InvalidName(name.into()));
    }
    Ok(())
}

/// SQLite INTEGER 是带符号 64 位；超出范围的 seq 必须显式失败，
/// 不能被静默截断成另一个事实身份。
fn event_seq_column(seq: u64) -> Result<i64, StorageError> {
    i64::try_from(seq)
        .map_err(|_| StorageError::Conflict(format!("事件 seq {seq} 超出 SQLite INTEGER 范围")))
}

fn event_row_index(index: usize) -> Result<i64, StorageError> {
    i64::try_from(index).map_err(|_| StorageError::Conflict("事件行号超出范围".into()))
}

fn event_row_count(value: i64) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| StorageError::Conflict("事件日志计数为负数".into()))
}

fn encode_event_json(event: &Event) -> Result<String, StorageError> {
    serde_json::to_string(event)
        .map_err(|error| StorageError::Io(format!("SQLite 事件序列化失败: {error}")))
}

fn decode_event_json(value: &str) -> Result<Event, StorageError> {
    serde_json::from_str(value)
        .map_err(|error| StorageError::Io(format!("SQLite 事件 JSON 非法: {error}")))
}

/// 同一条日志内 dedup_key 不得重复：文件后端由归约器保证这一不变量，
/// SQLite 用部分唯一索引 + 这里的前置校验把它变成存储层硬约束。
fn validate_event_log_dedup_keys(events: &[Event]) -> Result<(), StorageError> {
    let mut seen = BTreeSet::new();
    for event in events {
        if event.metadata.dedup_key.is_empty() {
            continue;
        }
        if !seen.insert(event.metadata.dedup_key.as_str()) {
            return Err(StorageError::Conflict(format!(
                "事件日志内 dedup_key {} 重复",
                event.metadata.dedup_key
            )));
        }
    }
    Ok(())
}

fn insert_event_row(
    connection: &Connection,
    name: &str,
    position: i64,
    event: &Event,
) -> Result<(), StorageError> {
    let seq = event_seq_column(event.seq)?;
    let event_ts = db_string(event.ts);
    let dedup_key = event.metadata.dedup_key.as_str();
    let event_json = encode_event_json(event)?;
    connection.execute(
        "INSERT INTO qx_event_log_entries(name, position, seq, event_ts, prio, dedup_key, event_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            name,
            position,
            seq,
            event_ts,
            i64::from(event.prio),
            dedup_key,
            event_json
        ],
    )
    .map_err(map_sqlite)?;
    Ok(())
}

fn stored_event_at_seq(
    connection: &Connection,
    name: &str,
    seq: u64,
) -> Result<Option<Event>, StorageError> {
    let seq = event_seq_column(seq)?;
    connection
        .query_row(
            "SELECT event_json FROM qx_event_log_entries WHERE name = ?1 AND seq = ?2",
            params![name, seq],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(map_sqlite)?
        .map(|value| decode_event_json(&value))
        .transpose()
}

fn stored_event_by_dedup_key(
    connection: &Connection,
    name: &str,
    dedup_key: &str,
) -> Result<Option<Event>, StorageError> {
    if dedup_key.is_empty() {
        return Ok(None);
    }
    connection
        .query_row(
            "SELECT event_json FROM qx_event_log_entries WHERE name = ?1 AND dedup_key = ?2",
            params![name, dedup_key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(map_sqlite)?
        .map(|value| decode_event_json(&value))
        .transpose()
}

/// 读取 manifest 行：`(event_count, next_seq, digest)`。
fn read_event_log_state(
    connection: &Connection,
    name: &str,
) -> Result<Option<(u64, u64, u64)>, StorageError> {
    let state = connection
        .query_row(
            "SELECT event_count, next_seq, digest FROM qx_event_log_state WHERE name = ?1",
            params![name],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite)?;
    let (event_count, next_seq, digest) = match state {
        Some(state) => state,
        None => return Ok(None),
    };
    Ok(Some((
        event_row_count(event_count)?,
        parse_db_u64(&next_seq, "event_log.next_seq")?,
        parse_db_u64(&digest, "event_log.digest")?,
    )))
}

fn write_event_log_state(
    connection: &Connection,
    name: &str,
    log: &EventLog,
) -> Result<(), StorageError> {
    let event_count = event_row_index(log.len())?;
    let next_seq = db_string(log.next_seq());
    let digest = db_string(log.digest());
    connection
        .execute(
            "INSERT INTO qx_event_log_state(name, event_count, next_seq, digest)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(name) DO UPDATE SET event_count = excluded.event_count,
                                        next_seq = excluded.next_seq,
                                        digest = excluded.digest",
            params![name, event_count, next_seq, digest],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

/// 顺序读整条日志，并复用 Kernel 的 seq/因果排序校验和 manifest 摘要比对；
/// 语义与 `SegmentedEventLogStore::read_if_exists` 一致。
fn load_event_log(connection: &Connection, name: &str) -> Result<Option<EventLog>, StorageError> {
    let (event_count, next_seq, digest) = match read_event_log_state(connection, name)? {
        Some(state) => state,
        None => return Ok(None),
    };
    let mut statement = connection
        .prepare(
            "SELECT position, event_json FROM qx_event_log_entries
             WHERE name = ?1 ORDER BY position ASC",
        )
        .map_err(map_sqlite)?;
    let rows = statement
        .query_map(params![name], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(map_sqlite)?;
    let mut log = EventLog::new();
    for (expected_position, row) in rows.enumerate() {
        let expected_position = expected_position as i64;
        let (position, event_json) = row.map_err(map_sqlite)?;
        if position != expected_position {
            return Err(StorageError::Conflict(format!(
                "事件日志 position 不连续：期望 {expected_position}，实际 {position}"
            )));
        }
        log.append(decode_event_json(&event_json)?);
    }
    log.validate().map_err(StorageError::Core)?;
    if log.len() as u64 != event_count || log.next_seq() != next_seq || log.digest() != digest {
        return Err(StorageError::Conflict(
            "事件日志 manifest 与事件内容摘要不一致".into(),
        ));
    }
    Ok(Some(log))
}

/// 在调用方事务内把整条日志按 append-only 语义落库。
///
/// 已存在的前缀必须逐事件相等；日志未增长时只重写 manifest，使同一内容的
/// 重复保存成为幂等重试而不是错误。
fn write_event_log_in_transaction(
    connection: &Connection,
    name: &str,
    log: &EventLog,
) -> Result<(), StorageError> {
    log.validate().map_err(StorageError::Core)?;
    validate_event_log_dedup_keys(log.events())?;
    let stored = load_event_log(connection, name)?.unwrap_or_default();
    if stored.len() > log.len()
        || stored
            .events()
            .iter()
            .zip(log.events())
            .any(|(old, new)| old != new)
    {
        return Err(StorageError::NonAppendOnly(name.into()));
    }
    let start = stored.len();
    if start == log.len() {
        return write_event_log_state(connection, name, log);
    }
    for (offset, event) in log.events()[start..].iter().enumerate() {
        insert_event_row(connection, name, event_row_index(start + offset)?, event)?;
    }
    write_event_log_state(connection, name, log)
}

/// 在调用方事务内逐条幂等追加事件，并同步刷新 manifest。
fn append_events_in_transaction(
    connection: &Connection,
    name: &str,
    events: &[Event],
    log: &mut EventLog,
) -> Result<usize, StorageError> {
    let mut appended = 0;
    for event in events {
        let dedup_key = event.metadata.dedup_key.as_str();
        if let Some(stored) = stored_event_by_dedup_key(connection, name, dedup_key)? {
            if stored == *event {
                continue;
            }
            return Err(StorageError::Conflict(format!(
                "dedup_key {dedup_key} 已被不同事件占用"
            )));
        }
        if let Some(stored) = stored_event_at_seq(connection, name, event.seq)? {
            if stored == *event {
                continue;
            }
            return Err(StorageError::NonAppendOnly(name.into()));
        }
        log.append_checked(event.clone())
            .map_err(StorageError::Core)?;
        let position = event_row_index(log.len() - 1)?;
        insert_event_row(connection, name, position, event)?;
        appended += 1;
    }
    if appended > 0 {
        write_event_log_state(connection, name, log)?;
    }
    Ok(appended)
}

impl SqliteEventLogStore {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let store = Self { path: path.into() };
        let _ = open(&store.path)?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// append-only 保存整条日志；返回数据库文件路径作为写入标记，
    /// 与文件后端的 `write` 返回路径保持同一调用约定。
    pub fn write(&self, name: &str, log: &EventLog) -> Result<PathBuf, StorageError> {
        validate_event_log_name(name)?;
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        write_event_log_in_transaction(&transaction, name, log)?;
        transaction.commit().map_err(map_sqlite)?;
        Ok(self.path.clone())
    }

    /// 与 PostgreSQL 后端同构的事务入口：EventLog 追加和 Outbox 投影要么
    /// 一起提交，要么一起回滚，禁止“先写事实、后丢出站事件”。
    pub fn write_with_outbox(
        &self,
        name: &str,
        log: &EventLog,
        outbox_events: &[OutboxEvent],
    ) -> Result<PathBuf, StorageError> {
        validate_event_log_name(name)?;
        for event in outbox_events {
            event.validate()?;
        }
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        write_event_log_in_transaction(&transaction, name, log)?;
        for event in outbox_events {
            append_outbox_event(&transaction, event)?;
        }
        transaction.commit().map_err(map_sqlite)?;
        Ok(self.path.clone())
    }

    pub fn read(&self, name: &str) -> Result<EventLog, StorageError> {
        self.read_if_exists(name)?
            .ok_or_else(|| StorageError::NotFound(format!("SQLite event log {name} 不存在")))
    }

    /// 首次启动时日志不存在不是错误；已存在但摘要不一致必须失败。
    pub fn read_if_exists(&self, name: &str) -> Result<Option<EventLog>, StorageError> {
        validate_event_log_name(name)?;
        let connection = open(&self.path)?;
        load_event_log(&connection, name)
    }

    /// 单条事实的幂等追加：返回 `false` 表示该事实已按同一 dedup_key 落库。
    pub fn append(&self, name: &str, event: &Event) -> Result<bool, StorageError> {
        self.append_many(name, std::slice::from_ref(event))
            .map(|appended| appended == 1)
    }

    /// 批量追加，返回真正写入的行数；重复事实与已存在 dedup_key 的相同事件
    /// 会被跳过，同一 dedup_key 承载不同事实则按冲突失败。
    pub fn append_many(&self, name: &str, events: &[Event]) -> Result<usize, StorageError> {
        validate_event_log_name(name)?;
        validate_event_log_dedup_keys(events)?;
        let mut connection = open(&self.path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let mut log = match load_event_log(&transaction, name)? {
            Some(log) => log,
            None => EventLog::new(),
        };
        let appended = append_events_in_transaction(&transaction, name, events, &mut log)?;
        transaction.commit().map_err(map_sqlite)?;
        Ok(appended)
    }

    /// 重放用的范围读：返回 `seq > after_seq` 的事件，按日志写入顺序排列。
    pub fn read_range(
        &self,
        name: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<Vec<Event>, StorageError> {
        validate_event_log_name(name)?;
        let connection = open(&self.path)?;
        if read_event_log_state(&connection, name)?.is_none() {
            return Err(StorageError::NotFound(format!(
                "SQLite 事件日志 {name} 不存在"
            )));
        }
        let after_seq = event_seq_column(after_seq)?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = connection
            .prepare(
                "SELECT event_json FROM qx_event_log_entries
                 WHERE name = ?1 AND seq > ?2 ORDER BY position ASC LIMIT ?3",
            )
            .map_err(map_sqlite)?;
        let rows = statement
            .query_map(params![name, after_seq, limit], |row| {
                row.get::<_, String>(0)
            })
            .map_err(map_sqlite)?;
        rows.map(|row| decode_event_json(&row.map_err(map_sqlite)?))
            .collect()
    }

    /// 最后一条已落库事件的 seq；`None` 表示日志为空或尚不存在。
    pub fn last_seq(&self, name: &str) -> Result<Option<u64>, StorageError> {
        validate_event_log_name(name)?;
        let connection = open(&self.path)?;
        let last: Option<i64> = connection
            .query_row(
                "SELECT seq FROM qx_event_log_entries
                 WHERE name = ?1 ORDER BY position DESC LIMIT 1",
                params![name],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sqlite)?;
        last.map(event_row_count).transpose()
    }

    pub fn event_count(&self, name: &str) -> Result<u64, StorageError> {
        validate_event_log_name(name)?;
        let connection = open(&self.path)?;
        Ok(read_event_log_state(&connection, name)?.map_or(0, |(count, _, _)| count))
    }

    pub fn next_seq(&self, name: &str) -> Result<Option<u64>, StorageError> {
        validate_event_log_name(name)?;
        let connection = open(&self.path)?;
        Ok(read_event_log_state(&connection, name)?.map(|(_, next_seq, _)| next_seq))
    }

    /// manifest 中记录的摘要；需要验证内容一致性时使用 `validate`。
    pub fn digest(&self, name: &str) -> Result<Option<u64>, StorageError> {
        validate_event_log_name(name)?;
        let connection = open(&self.path)?;
        Ok(read_event_log_state(&connection, name)?.map(|(_, _, digest)| digest))
    }

    /// 全量顺序读 + 因果排序校验 + manifest 摘要比对；日志不存在时报错。
    pub fn validate(&self, name: &str) -> Result<(), StorageError> {
        self.read(name).map(|_| ())
    }

    /// 供归约器/重放判断某条外部事实是否已经落库。空 dedup_key 不是去重键。
    pub fn contains_dedup_key(&self, name: &str, dedup_key: &str) -> Result<bool, StorageError> {
        validate_event_log_name(name)?;
        if dedup_key.is_empty() {
            return Ok(false);
        }
        let connection = open(&self.path)?;
        let found: Option<i64> = connection
            .query_row(
                "SELECT seq FROM qx_event_log_entries WHERE name = ?1 AND dedup_key = ?2",
                params![name, dedup_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sqlite)?;
        Ok(found.is_some())
    }

    /// 已存在的日志名，等价于文件后端的 `list`。
    pub fn list(&self) -> Result<Vec<String>, StorageError> {
        let connection = open(&self.path)?;
        let mut statement = connection
            .prepare("SELECT name FROM qx_event_log_state ORDER BY name ASC")
            .map_err(map_sqlite)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(map_sqlite)?;
        rows.collect::<Result<Vec<String>, rusqlite::Error>>()
            .map_err(map_sqlite)
    }
}

impl EventLogStore for SqliteEventLogStore {
    fn save(&self, name: &str, log: &EventLog) -> QxResult<()> {
        self.write(name, log).map(|_| ()).map_err(Into::into)
    }

    fn load(&self, name: &str) -> QxResult<EventLog> {
        self.read(name).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_control::{CommandKind, CommandStatus, Permission};
    use qx_protocol::AccountSnapshot;
    use qx_scheduler::{JobStatus, JobWindow, RetryPolicy, Trigger};
    use std::collections::BTreeMap;

    fn temp_db(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "qianxing-{label}-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn queued_job() -> (JobSpec, JobRun) {
        let job = JobSpec {
            job_id: "sqlite-job".into(),
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
            concurrency_key: "sqlite-job".into(),
            idempotency_key: "sqlite-job-daily".into(),
            permission_scope: "research".into(),
            audit_reason: "sqlite test".into(),
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

    fn queued_command(command_id: u64) -> ControlCommand {
        ControlCommand {
            command_id,
            request_id: format!("sqlite-command-{command_id}"),
            operator_id: "ops".into(),
            reason: "sqlite command queue test".into(),
            kind: CommandKind::SubmitOrder,
            target: command_id.to_string(),
            payload: BTreeMap::new(),
            permission: Permission::Trading,
            dry_run: true,
        }
    }

    #[test]
    fn sqlite_audit_is_idempotent_and_tamper_evident() {
        let path = temp_db("audit");
        let store = SqliteAuditStore::new(&path).unwrap();
        let record = AuditRecord {
            command_id: 1,
            request_id: "sqlite-audit".into(),
            operator_id: "ops".into(),
            command_digest: 7,
            status: CommandStatus::Accepted,
            result_code: "ACCEPTED".into(),
            ts: 10,
        };
        store.append(record.clone()).unwrap();
        store.append(record).unwrap();
        assert_eq!(store.read().unwrap().len(), 1);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE qx_audit_entries SET entry_hash = '1' WHERE sequence = 0",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.read(),
            Err(StorageError::Conflict(message)) if message.contains("摘要不一致")
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sqlite_job_queue_preserves_fencing_and_expiry_semantics() {
        let path = temp_db("jobs");
        let queue = SqliteJobQueue::new(&path).unwrap();
        let (job, run) = queued_job();
        let run_id = run.run_id;
        queue.enqueue(job.clone(), run.clone(), 10).unwrap();
        queue.enqueue(job, run, 10).unwrap();
        assert_eq!(queue.available(10).unwrap().len(), 1);
        let first = queue.claim(run_id, "worker-a", 10, 10).unwrap();
        assert_eq!(first.fencing_token, 1);
        assert!(matches!(
            queue.ack_at(run_id, "worker-a", first.fencing_token, 20),
            Err(StorageError::LeaseExpired { .. })
        ));
        assert_eq!(queue.recover_expired(20).unwrap(), vec![run_id]);
        let takeover = queue.claim(run_id, "worker-b", 21, 10).unwrap();
        assert_eq!(takeover.fencing_token, 2);
        assert!(matches!(
            queue.ack_at(run_id, "worker-a", first.fencing_token, 21),
            Err(StorageError::Unauthorized(_))
        ));
        queue
            .ack_at(run_id, "worker-b", takeover.fencing_token, 21)
            .unwrap();
        assert!(queue.available(22).unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sqlite_token_bucket_is_transactional_and_persistent() {
        let path = temp_db("bucket");
        let first = SqliteTokenBucket::new(&path, "api", 2, 0).unwrap();
        assert!(first.try_acquire(10, 1).unwrap());
        let second = SqliteTokenBucket::new(&path, "api", 2, 0).unwrap();
        assert!(second.try_acquire(10, 1).unwrap());
        assert!(!first.try_acquire(10, 1).unwrap());
        assert!(matches!(
            first.try_acquire(10, 3),
            Err(StorageError::Conflict(_))
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sqlite_control_state_and_command_queue_are_transactional_and_fenced() {
        let path = temp_db("control");
        let store = SqliteControlStore::new(&path).unwrap();
        let command = queued_command(91);
        let (_, accepted) = store
            .transact_control(|plane| plane.submit(command.clone(), 10))
            .unwrap();
        assert_eq!(accepted.unwrap().status, CommandStatus::Accepted);
        let (_, duplicate) = store
            .transact_control(|plane| plane.submit(command.clone(), 11))
            .unwrap();
        assert!(duplicate.is_err());
        assert_eq!(store.load_if_exists().unwrap().unwrap().audit().len(), 1);

        let queue = SqliteControlCommandQueue::new(&path).unwrap();
        queue.enqueue(command.clone(), 10).unwrap();
        queue.enqueue(command, 10).unwrap();
        assert_eq!(queue.available(10).unwrap().len(), 1);
        let lease = queue.claim(91, "execution-a", 10, 10).unwrap();
        assert!(matches!(
            queue.claim(91, "execution-b", 11, 10),
            Err(StorageError::LeaseHeld { .. })
        ));
        let takeover = queue.claim(91, "execution-b", 20, 10).unwrap();
        assert_eq!(takeover.fencing_token, lease.fencing_token + 1);
        assert!(matches!(
            queue.ack_at(91, "execution-a", lease.fencing_token, 21),
            Err(StorageError::Unauthorized(_))
        ));
        queue
            .ack_at(91, "execution-b", takeover.fencing_token, 21)
            .unwrap();
        assert!(queue.available(22).unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sqlite_snapshot_store_seals_and_revalidates_account_state() {
        let path = temp_db("snapshot");
        let store = SqliteSnapshotStore::new(&path).unwrap();
        let mut snapshot = AccountSnapshot::new(1, "main", "default", "BINANCE", 10);
        snapshot.cash_raw.insert("USDT".into(), 1000);
        snapshot.seal();
        store.save(&snapshot).unwrap();
        store.save(&snapshot).unwrap();
        let json = store
            .load_json(snapshot.header.snapshot_id, snapshot.state_hash())
            .unwrap();
        assert_eq!(AccountSnapshot::from_json(&json).unwrap(), snapshot);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE qx_snapshots SET content = '{\"tampered\":true}'",
                [],
            )
            .unwrap();
        assert!(store
            .load_json(snapshot.header.snapshot_id, snapshot.state_hash())
            .is_err());
        let _ = std::fs::remove_file(path);
    }
}
