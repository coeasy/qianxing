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
        topic: &str,
        partition_key: &str,
    ) -> Result<PathBuf, StorageError> {
        let key = self.state_key(group_id, topic, partition_key)?;
        Ok(self.root.join("consumers").join(format!("{key}.lock")))
    }

    fn read_state(&self, path: &Path) -> Result<FileConsumerState, StorageError> {
        read_state_json_or_default(path)
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
        let path = self.path_for(group_id, topic, partition_key)?;
        Ok(self.read_state(&path)?.checkpoint)
    }

    fn is_processed(&self, group_id: &str, event_id: &str) -> Result<bool, StorageError> {
        validate_outbox_name(group_id, "group_id")?;
        validate_outbox_name(event_id, "event_id")?;
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
        checkpoint.validate()?;
        self.transact_state(
            &checkpoint.group_id,
            &checkpoint.topic,
            &checkpoint.partition_key,
            |state| {
                if state.processed_event_ids.contains(&checkpoint.event_id) {
                    return Ok(Commit::Keep(()));
                }
                if let Some(previous) = state.checkpoint.as_ref() {
                    if previous.offset > checkpoint.offset {
                        return Err(StorageError::Conflict("consumer checkpoint 回退".into()));
                    }
                    if previous.offset == checkpoint.offset
                        && previous.event_id != checkpoint.event_id
                    {
                        return Err(StorageError::Conflict(
                            "consumer 相同 offset 对应不同 event_id".into(),
                        ));
                    }
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
        record.validate()?;
        self.transact_state(
            &record.group_id,
            &record.topic,
            &record.partition_key,
            |state| {
                if !state.dead_letters.iter().any(|existing| {
                    existing.event_id == record.event_id && existing.attempts == record.attempts
                }) {
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
        validate_outbox_name(group_id, "group_id")?;
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
        self.transact_state(
            &checkpoint.group_id,
            &checkpoint.topic,
            &checkpoint.partition_key,
            |state| {
                if state.processed_event_ids.contains(&checkpoint.event_id) {
                    return Ok(Commit::Keep(()));
                }
                if let Some(previous) = state.checkpoint.as_ref() {
                    if previous.offset > checkpoint.offset {
                        return Err(StorageError::Conflict("consumer checkpoint 回退".into()));
                    }
                    if previous.offset == checkpoint.offset
                        && previous.event_id != checkpoint.event_id
                    {
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
        record.validate_for(&checkpoint)?;
        self.transact_state(
            &checkpoint.group_id,
            &checkpoint.topic,
            &checkpoint.partition_key,
            |state| {
                if state.processed_event_ids.contains(&checkpoint.event_id) {
                    return Ok(Commit::Keep(()));
                }
                if let Some(previous) = state.checkpoint.as_ref() {
                    if previous.offset > checkpoint.offset {
                        return Err(StorageError::Conflict("consumer checkpoint 回退".into()));
                    }
                    if previous.offset == checkpoint.offset
                        && previous.event_id != checkpoint.event_id
                    {
                        return Err(StorageError::Conflict(
                            "consumer 相同 offset 对应不同 event_id".into(),
                        ));
                    }
                }
                if !state.dead_letters.iter().any(|existing| {
                    existing.event_id == record.event_id && existing.attempts == record.attempts
                }) {
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
        validate_outbox_name(group_id, "group_id")?;
        validate_outbox_name(projection_key, "projection_key")?;
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
