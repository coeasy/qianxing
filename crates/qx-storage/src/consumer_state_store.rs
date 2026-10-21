use crate::{
    validate_outbox_name, ConsumerCheckpoint, ConsumerProjection, DeadLetterRecord, StorageError,
};
use std::cmp::Ordering;

#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub(crate) const DEAD_LETTER_ORDER_BY: &str = "attempts, event_id";

pub(crate) fn compare_dead_letter_order(
    left: &DeadLetterRecord,
    right: &DeadLetterRecord,
) -> Ordering {
    (left.attempts, &left.event_id).cmp(&(right.attempts, &right.event_id))
}

/// Shared validation for file, SQLite, and PostgreSQL consumer state stores.
pub trait ConsumerStateStore: Send + Sync {
    fn validate_load_checkpoint_request(
        &self,
        group_id: &str,
        topic: &str,
        partition_key: &str,
    ) -> Result<(), StorageError> {
        validate_outbox_name(group_id, "group_id")?;
        validate_outbox_name(topic, "topic")?;
        validate_outbox_name(partition_key, "partition_key")
    }

    fn validate_is_processed_request(
        &self,
        group_id: &str,
        event_id: &str,
    ) -> Result<(), StorageError> {
        validate_outbox_name(group_id, "group_id")?;
        validate_outbox_name(event_id, "event_id")
    }

    fn validate_commit_processed_request(
        &self,
        checkpoint: &ConsumerCheckpoint,
    ) -> Result<(), StorageError> {
        checkpoint.validate()
    }

    fn validate_append_dead_letter_request(
        &self,
        record: &DeadLetterRecord,
    ) -> Result<(), StorageError> {
        record.validate()
    }

    fn validate_dead_letters_request(&self, group_id: &str) -> Result<(), StorageError> {
        validate_outbox_name(group_id, "group_id")
    }

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

/// Shared validation for atomic projection/checkpoint storage operations.
pub trait TransactionalConsumerStateStore: ConsumerStateStore {
    fn validate_commit_processed_with_projection_request(
        &self,
        checkpoint: &ConsumerCheckpoint,
        projection: &ConsumerProjection,
    ) -> Result<(), StorageError> {
        projection.validate_for(checkpoint)
    }

    fn validate_append_dead_letter_and_commit_request(
        &self,
        record: &DeadLetterRecord,
        checkpoint: &ConsumerCheckpoint,
    ) -> Result<(), StorageError> {
        record.validate_for(checkpoint)
    }

    fn validate_load_projection_request(
        &self,
        group_id: &str,
        projection_key: &str,
    ) -> Result<(), StorageError> {
        validate_outbox_name(group_id, "group_id")?;
        validate_outbox_name(projection_key, "projection_key")
    }

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
