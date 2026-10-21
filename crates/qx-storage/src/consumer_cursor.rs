use super::StorageError;

/// Enforce the monotonic ordering shared by file, SQLite, and PostgreSQL consumer state.
/// An offset may stay equal only when it still names the same event; it may never move back.
pub(crate) fn validate_consumer_cursor_advance(
    previous_offset: u64,
    previous_event_id: &str,
    next_offset: u64,
    next_event_id: &str,
    regression_message: &str,
    same_offset_message: &str,
) -> Result<(), StorageError> {
    if previous_offset > next_offset {
        return Err(StorageError::Conflict(regression_message.into()));
    }
    if previous_offset == next_offset && previous_event_id != next_event_id {
        return Err(StorageError::Conflict(same_offset_message.into()));
    }
    Ok(())
}

#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub(crate) fn validate_consumer_cursor_advance_with_message(
    previous_offset: u64,
    previous_event_id: &str,
    next_offset: u64,
    next_event_id: &str,
    message: &str,
) -> Result<(), StorageError> {
    validate_consumer_cursor_advance(
        previous_offset,
        previous_event_id,
        next_offset,
        next_event_id,
        message,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::validate_consumer_cursor_advance;
    use crate::StorageError;

    #[test]
    fn consumer_cursor_accepts_forward_and_identical_replay() {
        assert!(
            validate_consumer_cursor_advance(4, "event-4", 5, "event-5", "back", "fork").is_ok()
        );
        assert!(
            validate_consumer_cursor_advance(5, "event-5", 5, "event-5", "back", "fork").is_ok()
        );
    }

    #[test]
    fn consumer_cursor_rejects_regression_and_same_offset_fork() {
        assert!(matches!(
            validate_consumer_cursor_advance(5, "event-5", 4, "event-4", "back", "fork"),
            Err(StorageError::Conflict(message)) if message == "back"
        ));
        assert!(matches!(
            validate_consumer_cursor_advance(5, "event-5a", 5, "event-5b", "back", "fork"),
            Err(StorageError::Conflict(message)) if message == "fork"
        ));
    }
}
