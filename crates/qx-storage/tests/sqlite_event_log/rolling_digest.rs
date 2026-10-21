use qx_core::{Event, EventKind, EventLog, EventMetadata, Priority};
use qx_storage::{SqliteEventLogStore, StorageError};
use rusqlite::Connection;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_db() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!(
            "sqlite-rolling-digest-{}-{nonce}.sqlite",
            std::process::id()
        ))
}

fn event(seq: u64, key: &str) -> Event {
    Event::new(seq, seq + 1, Priority::POST, EventKind::Settle).with_metadata(EventMetadata {
        dedup_key: key.into(),
        ..EventMetadata::default()
    })
}

fn sample_log(count: u64) -> EventLog {
    let mut log = EventLog::new();
    for seq in 0..count {
        log.append(event(seq, &format!("fact-{seq}")));
    }
    log
}

#[test]
fn sqlite_event_log_rolling_manifest_digest_upgrades_legacy_and_detects_tampering() {
    let path = temp_db();
    let store = SqliteEventLogStore::new(&path).unwrap();
    let legacy = sample_log(5);
    store.write("run", &legacy).unwrap();

    // Simulate a database created before rolling manifest digests: unprefixed decimal is legacy.
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE qx_event_log_state SET digest = ?1 WHERE name = ?2",
            (&legacy.digest().to_string(), "run"),
        )
        .unwrap();
    drop(connection);
    assert_eq!(store.read("run").unwrap().digest(), legacy.digest());

    // The first incremental append verifies the old prefix, then upgrades the manifest in place.
    let extended = sample_log(7);
    store.append_batch("run", &extended, &[]).unwrap();
    assert_eq!(store.digest("run").unwrap(), Some(extended.digest()));
    assert_eq!(store.read("run").unwrap().digest(), extended.digest());
    let connection = Connection::open(&path).unwrap();
    let manifest_digest: String = connection
        .query_row(
            "SELECT digest FROM qx_event_log_state WHERE name = ?1",
            ["run"],
            |row| row.get(0),
        )
        .unwrap();
    assert!(manifest_digest.starts_with("v2:"));

    // V2 rolling digests are checked on full reads just like the legacy digest was.
    let corrupted = serde_json::to_string(&event(1, "tampered")).unwrap();
    connection
        .execute(
            "UPDATE qx_event_log_entries SET event_json = ?1 WHERE name = ?2 AND position = 1",
            (&corrupted, "run"),
        )
        .unwrap();
    drop(connection);
    assert!(matches!(store.read("run"), Err(StorageError::Conflict(_))));
    let _ = std::fs::remove_file(path);
}
