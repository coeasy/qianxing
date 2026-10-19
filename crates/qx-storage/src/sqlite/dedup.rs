use super::*;

/// SQLite 把日志级 dedup_key 唯一性作为存储层硬约束。
pub(super) fn validate_event_log_dedup_keys(events: &[Event]) -> Result<(), StorageError> {
    let mut seen = BTreeSet::new();
    for event in events {
        let key = event.metadata.dedup_key.as_str();
        if !key.is_empty() && !seen.insert(key) {
            return Err(StorageError::Conflict(format!(
                "事件日志内 dedup_key {key} 重复"
            )));
        }
    }
    Ok(())
}

/// 增量写只检查新尾部；历史前缀由 manifest 摘要验证，唯一索引承担历史 key 查找。
pub(super) fn validate_event_log_dedup_tail(
    connection: &Connection,
    name: &str,
    events: &[Event],
) -> Result<(), StorageError> {
    let mut seen = BTreeSet::new();
    for event in events {
        let key = event.metadata.dedup_key.as_str();
        if key.is_empty() {
            continue;
        }
        if !seen.insert(key) || super::stored_event_by_dedup_key(connection, name, key)?.is_some() {
            return Err(StorageError::Conflict(format!(
                "事件日志内 dedup_key {key} 重复"
            )));
        }
    }
    Ok(())
}
