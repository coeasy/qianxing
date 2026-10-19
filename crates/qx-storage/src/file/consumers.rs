//! 文件消费者状态存储（checkpoint、幂等标记、dead-letter 与投影）。
//!
//! 落盘一律经由统一信封（`state_envelope`）与 [`JsonRecordStore`] 的共享
//! load/list/save 形状；目录枚举不再各自抄写 `read_dir` 样板。

use super::*;

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
struct FileConsumerState {
    checkpoint: Option<ConsumerCheckpoint>,
    processed_event_ids: BTreeSet<String>,
    dead_letters: Vec<DeadLetterRecord>,
    #[serde(default)]
    projections: std::collections::BTreeMap<String, ConsumerProjection>,
}

impl JsonStateEnvelope for FileConsumerState {
    const LABEL: &'static str = "consumer 状态";
}

#[derive(Clone, Debug)]
pub struct FileConsumerStateStore {
    root: PathBuf,
}

impl JsonRecordStore for FileConsumerStateStore {
    fn records_dir(&self) -> PathBuf {
        self.root.join("consumers")
    }
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
        _topic: &str,
        _partition_key: &str,
    ) -> Result<PathBuf, StorageError> {
        validate_outbox_name(group_id, "group_id")?;
        let key = outbox_file_key(group_id);
        Ok(self
            .root
            .join("consumers")
            .join(format!("{key}.group.lock")))
    }

    fn read_state(&self, path: &Path) -> Result<FileConsumerState, StorageError> {
        read_state_json_or_default(path)
    }

    /// SQL stores key processed events by `(group_id, event_id)`, independent
    /// of topic/partition. The file backend keeps partition snapshots separate,
    /// so commits must inspect sibling snapshots while holding the group lock.
    fn is_processed_in_group(
        &self,
        state: &FileConsumerState,
        state_path: &Path,
        group_id: &str,
        event_id: &str,
    ) -> Result<bool, StorageError> {
        if state.processed_event_ids.contains(event_id) {
            return Ok(true);
        }
        for path in self.list_records()? {
            if path == state_path {
                continue;
            }
            let other = self.read_state(&path)?;
            if other.processed_event_ids.contains(event_id)
                && other
                    .checkpoint
                    .as_ref()
                    .is_some_and(|checkpoint| checkpoint.group_id == group_id)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// SQL stores dead letters by `(group_id, event_id, attempts)` across every
    /// topic and partition. File snapshots are partition-scoped, so inspect
    /// sibling snapshots while `transact_state` holds the group lock.
    fn has_dead_letter_in_group(
        &self,
        state: &FileConsumerState,
        state_path: &Path,
        record: &DeadLetterRecord,
    ) -> Result<bool, StorageError> {
        let matches = |candidate: &DeadLetterRecord| {
            candidate.group_id == record.group_id
                && candidate.event_id == record.event_id
                && candidate.attempts == record.attempts
        };
        if state.dead_letters.iter().any(matches) {
            return Ok(true);
        }
        for path in self.list_records()? {
            if path == state_path {
                continue;
            }
            if self.read_state(&path)?.dead_letters.iter().any(matches) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// 信封事务：确保 consumers 目录 → 取锁 → 读改写（幂等早退不落盘）。
    fn transact_state(
        &self,
        group_id: &str,
        topic: &str,
        partition_key: &str,
        update: impl FnOnce(&mut FileConsumerState) -> Result<Commit<()>, StorageError>,
    ) -> Result<(), StorageError> {
        let path = self.path_for(group_id, topic, partition_key)?;
        let lock = self.lock_for(group_id, topic, partition_key)?;
        transact_state_json(
            &self.root,
            &self.root.join("consumers"),
            &path,
            lock,
            update,
        )
    }
}

impl ConsumerStateStore for FileConsumerStateStore {
    fn load_checkpoint(
        &self,
        group_id: &str,
        topic: &str,
        partition_key: &str,
    ) -> Result<Option<ConsumerCheckpoint>, StorageError> {
        self.validate_load_checkpoint_request(group_id, topic, partition_key)?;
        let path = self.path_for(group_id, topic, partition_key)?;
        Ok(self.read_state(&path)?.checkpoint)
    }

    fn is_processed(&self, group_id: &str, event_id: &str) -> Result<bool, StorageError> {
        self.validate_is_processed_request(group_id, event_id)?;
        for path in self.list_records()? {
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
        self.validate_commit_processed_request(&checkpoint)?;
        let state_path = self.path_for(
            &checkpoint.group_id,
            &checkpoint.topic,
            &checkpoint.partition_key,
        )?;
        self.transact_state(
            &checkpoint.group_id,
            &checkpoint.topic,
            &checkpoint.partition_key,
            |state| {
                if self.is_processed_in_group(
                    state,
                    &state_path,
                    &checkpoint.group_id,
                    &checkpoint.event_id,
                )? {
                    return Ok(Commit::Keep(()));
                }
                if let Some(previous) = state.checkpoint.as_ref() {
                    super::super::validate_consumer_cursor_advance(
                        previous.offset,
                        &previous.event_id,
                        checkpoint.offset,
                        &checkpoint.event_id,
                        "consumer checkpoint 回退",
                        "consumer 相同 offset 对应不同 event_id",
                    )?;
                }
                state
                    .processed_event_ids
                    .insert(checkpoint.event_id.clone());
                state.checkpoint = Some(checkpoint.clone());
                Ok(Commit::Write(()))
            },
        )
    }

    fn append_dead_letter(&self, record: DeadLetterRecord) -> Result<(), StorageError> {
        self.validate_append_dead_letter_request(&record)?;
        let state_path = self.path_for(&record.group_id, &record.topic, &record.partition_key)?;
        self.transact_state(
            &record.group_id,
            &record.topic,
            &record.partition_key,
            |state| {
                if !self.has_dead_letter_in_group(state, &state_path, &record)? {
                    state.dead_letters.push(record.clone());
                    return Ok(Commit::Write(()));
                }
                Ok(Commit::Keep(()))
            },
        )
    }

    fn dead_letters(
        &self,
        group_id: &str,
        limit: usize,
    ) -> Result<Vec<DeadLetterRecord>, StorageError> {
        self.validate_dead_letters_request(group_id)?;
        let mut result = Vec::new();
        for path in self.list_records()? {
            let state = self.read_state(&path)?;
            result.extend(
                state
                    .dead_letters
                    .into_iter()
                    .filter(|record| record.group_id == group_id),
            );
        }
        result.sort_by(crate::consumer_state_store::compare_dead_letter_order);
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
        self.validate_commit_processed_with_projection_request(&checkpoint, &projection)?;
        let state_path = self.path_for(
            &checkpoint.group_id,
            &checkpoint.topic,
            &checkpoint.partition_key,
        )?;
        self.transact_state(
            &checkpoint.group_id,
            &checkpoint.topic,
            &checkpoint.partition_key,
            |state| {
                if self.is_processed_in_group(
                    state,
                    &state_path,
                    &checkpoint.group_id,
                    &checkpoint.event_id,
                )? {
                    return Ok(Commit::Keep(()));
                }
                if let Some(previous) = state.checkpoint.as_ref() {
                    super::super::validate_consumer_cursor_advance(
                        previous.offset,
                        &previous.event_id,
                        checkpoint.offset,
                        &checkpoint.event_id,
                        "consumer checkpoint 回退",
                        "consumer 相同 offset 对应不同 event_id",
                    )?;
                }
                if let Some(previous) = state.projections.get(&projection.projection_key) {
                    super::super::validate_consumer_cursor_advance(
                        previous.offset,
                        &previous.event_id,
                        projection.offset,
                        &projection.event_id,
                        "consumer projection 顺序或 event_id 非法",
                        "consumer projection 顺序或 event_id 非法",
                    )?;
                }
                state
                    .processed_event_ids
                    .insert(checkpoint.event_id.clone());
                state
                    .projections
                    .insert(projection.projection_key.clone(), projection.clone());
                state.checkpoint = Some(checkpoint.clone());
                Ok(Commit::Write(()))
            },
        )
    }

    fn append_dead_letter_and_commit(
        &self,
        record: DeadLetterRecord,
        checkpoint: ConsumerCheckpoint,
    ) -> Result<(), StorageError> {
        self.validate_append_dead_letter_and_commit_request(&record, &checkpoint)?;
        let state_path = self.path_for(
            &checkpoint.group_id,
            &checkpoint.topic,
            &checkpoint.partition_key,
        )?;
        self.transact_state(
            &checkpoint.group_id,
            &checkpoint.topic,
            &checkpoint.partition_key,
            |state| {
                if self.is_processed_in_group(
                    state,
                    &state_path,
                    &checkpoint.group_id,
                    &checkpoint.event_id,
                )? {
                    return Ok(Commit::Keep(()));
                }
                if let Some(previous) = state.checkpoint.as_ref() {
                    super::super::validate_consumer_cursor_advance(
                        previous.offset,
                        &previous.event_id,
                        checkpoint.offset,
                        &checkpoint.event_id,
                        "consumer checkpoint 回退",
                        "consumer 相同 offset 对应不同 event_id",
                    )?;
                }
                if !self.has_dead_letter_in_group(state, &state_path, &record)? {
                    state.dead_letters.push(record.clone());
                }
                state
                    .processed_event_ids
                    .insert(checkpoint.event_id.clone());
                state.checkpoint = Some(checkpoint.clone());
                Ok(Commit::Write(()))
            },
        )
    }

    fn load_projection(
        &self,
        group_id: &str,
        projection_key: &str,
    ) -> Result<Option<ConsumerProjection>, StorageError> {
        self.validate_load_projection_request(group_id, projection_key)?;
        for path in self.list_records()? {
            if let Some(projection) = self.read_state(&path)?.projections.get(projection_key) {
                if projection.group_id == group_id {
                    return Ok(Some(projection.clone()));
                }
            }
        }
        Ok(None)
    }
}
