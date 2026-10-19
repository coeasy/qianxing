//! 文件、分段、SQLite 与 PostgreSQL EventLog 共用的事实边界规则。

use crate::{EventLog, StorageError};

pub(crate) fn validate_event_log_name(name: &str) -> Result<(), StorageError> {
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

pub(crate) fn validate_event_log(log: &EventLog) -> Result<(), StorageError> {
    log.validate().map_err(StorageError::Core)
}
