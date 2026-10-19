use super::{AuditEntry, StorageError};
use qx_control::AuditRecord;

/// Return the durable prefix shared by a backend audit chain and the control-plane snapshot.
/// A longer durable chain is valid when the complete common prefix still matches: another
/// process may have committed first, so length alone is never evidence of a fork.
pub(crate) fn audit_sync_prefix_len(
    existing: &[AuditEntry],
    records: &[AuditRecord],
    conflict_message: &str,
) -> Result<usize, StorageError> {
    let common = existing.len().min(records.len());
    if existing
        .iter()
        .take(common)
        .zip(records.iter().take(common))
        .any(|(entry, record)| entry.record != *record)
    {
        return Err(StorageError::Conflict(conflict_message.into()));
    }
    Ok(common)
}
