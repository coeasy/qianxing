use qx_storage::{SqliteOutboxStore, StorageError};
use rusqlite::Connection;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn migration_db_path(label: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/qx-storage-migration-{label}-{}-{}.sqlite",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos()
    ))
}

fn index_exists(connection: &Connection, index: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1)",
            [index],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn failed_v2_migration_rolls_back_ddl_and_version_marker() {
    let path = migration_db_path("rollback");
    drop(SqliteOutboxStore::new(&path).expect("fresh schema should reach v2"));
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("DELETE FROM qx_schema_migrations WHERE version = '2'", [])
        .unwrap();
    connection
        .execute_batch(
            "DROP INDEX qx_jobs_pending_idx;
             DROP INDEX qx_control_commands_pending_idx;
             DROP INDEX qx_outbox_pending_idx;
             DROP TABLE qx_control_commands;",
        )
        .unwrap();
    drop(connection);

    assert!(matches!(
        SqliteOutboxStore::new(&path),
        Err(StorageError::Io(_))
    ));
    let connection = Connection::open(&path).unwrap();
    let version_two_exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM qx_schema_migrations WHERE version = '2')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!version_two_exists, "failed migration recorded v2");
    assert!(
        !index_exists(&connection, "qx_jobs_pending_idx"),
        "partial v2 DDL escaped the failed transaction"
    );
    drop(connection);
    let _ = std::fs::remove_file(path);
}

#[test]
fn unversioned_business_schema_is_rejected_without_claiming_it() {
    let path = migration_db_path("unversioned");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE qx_jobs (run_id TEXT PRIMARY KEY);")
        .unwrap();
    drop(connection);

    assert!(matches!(
        SqliteOutboxStore::new(&path),
        Err(StorageError::Conflict(message)) if message.contains("需先执行显式迁移")
    ));
    let connection = Connection::open(&path).unwrap();
    let ledger_created: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'qx_schema_migrations')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !ledger_created,
        "failed bootstrap committed a migration ledger"
    );
    drop(connection);
    let _ = std::fs::remove_file(path);
}
