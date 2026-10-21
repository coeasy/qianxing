use super::*;

/// The unprefixed form is the original full EventLog digest; v2 is the append-maintained rolling
/// event digest. Keeping the format in the existing TEXT column makes the transition lazy and
/// backward-compatible without a schema migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EventLogManifestDigest {
    Legacy(u64),
    RollingV2(u64),
}

fn parse_manifest_digest(value: &str) -> Result<EventLogManifestDigest, StorageError> {
    if let Some(value) = value.strip_prefix("v2:") {
        return parse_db_u64(value, "event_log.event_digest")
            .map(EventLogManifestDigest::RollingV2);
    }
    parse_db_u64(value, "event_log.digest").map(EventLogManifestDigest::Legacy)
}

pub(super) fn read_event_log_state(
    connection: &Connection,
    name: &str,
) -> Result<Option<(u64, u64, EventLogManifestDigest)>, StorageError> {
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
        parse_manifest_digest(&digest)?,
    )))
}

pub(super) fn write_event_log_state(
    connection: &Connection,
    name: &str,
    log: &EventLog,
) -> Result<(), StorageError> {
    let event_count = event_row_index(log.len())?;
    let next_seq = db_string(log.next_seq());
    let digest = format!("v2:{}", log.event_digest());
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
