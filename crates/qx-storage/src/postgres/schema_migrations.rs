use super::*;
use qx_core::{EventKind, Priority};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
#[ignore = "requires QX_TEST_POSTGRES_DSN and a running PostgreSQL instance"]
fn postgres_schema_migrations_upgrade_v1_atomically_and_fail_closed() {
    let dsn = std::env::var("QX_TEST_POSTGRES_DSN")
        .expect("QX_TEST_POSTGRES_DSN must point at an isolated test database");
    let storage = PostgresStorage::connect_with_pool_size(&dsn, 1)
        .expect("connect PostgreSQL for schema migration contract");
    let schema = format!(
        "qx_schema_migration_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos()
    );
    let mut client = storage
        .lock_client()
        .expect("lock isolated test connection");
    client
        .batch_execute(&format!(
            "CREATE SCHEMA {schema}; SET search_path TO {schema};"
        ))
        .expect("create isolated schema");

    let result = (|| -> Result<(), StorageError> {
        let mut transaction = client.transaction().map_err(pg_error)?;
        transaction
            .batch_execute(super::super::schema_migrations::CREATE_SCHEMA_MIGRATIONS_SQL)
            .map_err(pg_error)?;
        transaction
            .batch_execute(super::super::schema_migrations::POSTGRES_V1_SCHEMA)
            .map_err(pg_error)?;
        transaction
            .execute(
                "INSERT INTO qx_schema_migrations(version, applied_ts) VALUES ($1, '0')",
                &[&"1"],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)?;

        migrate_client(&mut client)?;
        let versions = client
            .query(
                "SELECT version FROM qx_schema_migrations ORDER BY version",
                &[],
            )
            .map_err(pg_error)?
            .into_iter()
            .map(|row| row.get::<_, String>(0))
            .collect::<Vec<_>>();
        if versions != ["1", "2", "3", "4", "5"] {
            return Err(StorageError::Conflict(format!(
                "PostgreSQL v1 upgrade recorded unexpected versions: {versions:?}"
            )));
        }
        client
            .batch_execute(
                "DELETE FROM qx_schema_migrations WHERE version IN ('4', '5');
                 DROP TABLE qx_deployment_blobs;
                 DROP TABLE qx_event_log_heads;
                 DROP TABLE qx_event_log_entries;",
            )
            .map_err(pg_error)?;
        migrate_client(&mut client)?;
        let blob_table_exists: bool = client
            .query_one("SELECT to_regclass('qx_deployment_blobs') IS NOT NULL", &[])
            .map_err(pg_error)?
            .get(0);
        let version_four_exists: bool = client
            .query_one(
                "SELECT EXISTS (
                     SELECT 1 FROM qx_schema_migrations WHERE version = '4'
                 )",
                &[],
            )
            .map_err(pg_error)?
            .get(0);
        let event_log_index_exists: bool = client
            .query_one(
                "SELECT to_regclass('qx_event_log_entries') IS NOT NULL
                    AND to_regclass('qx_event_log_heads') IS NOT NULL",
                &[],
            )
            .map_err(pg_error)?
            .get(0);
        if !blob_table_exists || !version_four_exists || !event_log_index_exists {
            return Err(StorageError::Conflict(
                "PostgreSQL v3 upgrade did not restore the deployment blob schema".into(),
            ));
        }
        for index in [
            "qx_jobs_pending_idx",
            "qx_control_commands_pending_idx",
            "qx_outbox_pending_idx",
        ] {
            let exists: bool = client
                .query_one(
                    "SELECT EXISTS (
                         SELECT 1 FROM pg_indexes
                         WHERE schemaname = current_schema() AND indexname = $1
                     )",
                    &[&index],
                )
                .map_err(pg_error)?
                .get(0);
            if !exists {
                return Err(StorageError::Conflict(format!(
                    "PostgreSQL v2 migration did not create {index}"
                )));
            }
        }

        client
            .batch_execute(
                "DELETE FROM qx_schema_migrations WHERE version IN ('2', '3', '4', '5');
                 DROP INDEX qx_jobs_pending_idx;
                 DROP INDEX qx_control_commands_pending_idx;
                 DROP INDEX qx_outbox_pending_idx;
                 DROP TABLE qx_deployment_blobs;
                 DROP TABLE qx_event_log_heads;
                 DROP TABLE qx_event_log_entries;
                 DROP TABLE qx_json_state;
                 DROP TABLE qx_control_commands;",
            )
            .map_err(pg_error)?;
        if migrate_client(&mut client).is_ok() {
            return Err(StorageError::Conflict(
                "PostgreSQL v2 migration unexpectedly accepted a missing table".into(),
            ));
        }
        let version_two_exists: bool = client
            .query_one(
                "SELECT EXISTS (
                     SELECT 1 FROM qx_schema_migrations WHERE version = '2'
                 )",
                &[],
            )
            .map_err(pg_error)?
            .get(0);
        let partial_index_exists: bool = client
            .query_one(
                "SELECT EXISTS (
                     SELECT 1 FROM pg_indexes
                     WHERE schemaname = current_schema() AND indexname = 'qx_jobs_pending_idx'
                 )",
                &[],
            )
            .map_err(pg_error)?
            .get(0);
        let version_three_exists: bool = client
            .query_one(
                "SELECT EXISTS (
                     SELECT 1 FROM qx_schema_migrations WHERE version = '3'
                 )",
                &[],
            )
            .map_err(pg_error)?
            .get(0);
        let version_four_exists: bool = client
            .query_one(
                "SELECT EXISTS (
                     SELECT 1 FROM qx_schema_migrations WHERE version = '4'
                 )",
                &[],
            )
            .map_err(pg_error)?
            .get(0);
        let version_five_exists: bool = client
            .query_one(
                "SELECT EXISTS (
                     SELECT 1 FROM qx_schema_migrations WHERE version = '5'
                 )",
                &[],
            )
            .map_err(pg_error)?
            .get(0);
        if version_two_exists
            || version_three_exists
            || version_four_exists
            || version_five_exists
            || partial_index_exists
        {
            return Err(StorageError::Conflict(
                "failed PostgreSQL migration leaked v2 DDL or marker".into(),
            ));
        }

        client
            .batch_execute("DROP TABLE qx_schema_migrations;")
            .map_err(pg_error)?;
        if migrate_client(&mut client).is_ok() {
            return Err(StorageError::Conflict(
                "PostgreSQL migration unexpectedly adopted unversioned tables".into(),
            ));
        }
        let ledger_exists: bool = client
            .query_one(
                "SELECT to_regclass('qx_schema_migrations') IS NOT NULL",
                &[],
            )
            .map_err(pg_error)?
            .get(0);
        if ledger_exists {
            return Err(StorageError::Conflict(
                "failed PostgreSQL bootstrap left a schema ledger behind".into(),
            ));
        }
        Ok(())
    })();

    let cleanup = client.batch_execute(&format!(
        "RESET search_path; DROP SCHEMA IF EXISTS {schema} CASCADE;"
    ));
    cleanup.expect("drop isolated migration schema");
    result.expect("PostgreSQL migration sequence contract");
}

#[test]
#[ignore = "requires QX_TEST_POSTGRES_DSN and a running PostgreSQL instance"]
fn postgres_legacy_schema_adoption_recognizes_v1_v2_and_rejects_drift() {
    let dsn = std::env::var("QX_TEST_POSTGRES_DSN")
        .expect("QX_TEST_POSTGRES_DSN must point at an isolated test database");
    let storage = PostgresStorage::connect(&dsn).expect("connect PostgreSQL for adoption contract");
    let suffix = format!(
        "{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos()
    );
    let schema_v1 = format!("qx_adopt_v1_{suffix}");
    let schema_v2 = format!("qx_adopt_v2_{suffix}");
    let schema_drift = format!("qx_adopt_drift_{suffix}");
    let schema_missing = format!("qx_adopt_missing_{suffix}");
    let mut client = storage.lock_client().expect("lock test connection");
    client
        .batch_execute(&format!(
            "CREATE SCHEMA {schema_v1};
             CREATE SCHEMA {schema_v2};
             CREATE SCHEMA {schema_drift};"
        ))
        .expect("create isolated adoption schemas");

    let result = (|| -> Result<(), StorageError> {
        for schema in [&schema_v1, &schema_v2, &schema_drift] {
            client
                .batch_execute(&format!("SET search_path TO {schema};"))
                .map_err(pg_error)?;
            client
                .batch_execute(super::super::schema_migrations::POSTGRES_V1_SCHEMA)
                .map_err(pg_error)?;
        }

        client
            .batch_execute(&format!("SET search_path TO {schema_v1};"))
            .map_err(pg_error)?;
        client
            .execute(
                "INSERT INTO qx_event_logs(name, content) VALUES ('legacy-adoption', '{}')",
                &[],
            )
            .map_err(pg_error)?;

        client
            .batch_execute(&format!(
                "SET search_path TO {schema_v2}; {}",
                super::super::schema_migrations::POSTGRES_V2_SCHEMA
            ))
            .map_err(pg_error)?;
        client
            .batch_execute(&format!(
                "SET search_path TO {schema_drift};
                 ALTER TABLE qx_event_logs ADD COLUMN unexpected TEXT;"
            ))
            .map_err(pg_error)?;

        let adopted_v1 = adopt_legacy_postgres_schema(&dsn, &schema_v1)?;
        if adopted_v1 != 1 {
            return Err(StorageError::Conflict(format!(
                "PostgreSQL v1 adoption returned version {adopted_v1}"
            )));
        }
        client
            .batch_execute(&format!("SET search_path TO {schema_v1};"))
            .map_err(pg_error)?;
        let retained: bool = client
            .query_one(
                "SELECT EXISTS (
                     SELECT 1 FROM qx_event_logs WHERE name = 'legacy-adoption' AND content = '{}'
                 )",
                &[],
            )
            .map_err(pg_error)?
            .get(0);
        if !retained {
            return Err(StorageError::Conflict(
                "PostgreSQL v1 adoption changed existing event log data".into(),
            ));
        }
        migrate_client(&mut client)?;
        let upgraded: Vec<String> = client
            .query(
                "SELECT version FROM qx_schema_migrations ORDER BY version",
                &[],
            )
            .map_err(pg_error)?
            .into_iter()
            .map(|row| row.get(0))
            .collect();
        if upgraded != ["1", "2", "3", "4", "5"] {
            return Err(StorageError::Conflict(format!(
                "PostgreSQL adopted v1 schema did not upgrade to v2: {upgraded:?}"
            )));
        }

        let adopted_v2 = adopt_legacy_postgres_schema(&dsn, &schema_v2)?;
        if adopted_v2 != 2 {
            return Err(StorageError::Conflict(format!(
                "PostgreSQL v2 adoption returned version {adopted_v2}"
            )));
        }
        client
            .batch_execute(&format!("SET search_path TO {schema_v2};"))
            .map_err(pg_error)?;
        let versions: Vec<String> = client
            .query(
                "SELECT version FROM qx_schema_migrations ORDER BY version",
                &[],
            )
            .map_err(pg_error)?
            .into_iter()
            .map(|row| row.get(0))
            .collect();
        if versions != ["1", "2"] {
            return Err(StorageError::Conflict(format!(
                "PostgreSQL v2 adoption recorded unexpected versions: {versions:?}"
            )));
        }
        if adopt_legacy_postgres_schema(&dsn, &schema_v2).is_ok() {
            return Err(StorageError::Conflict(
                "PostgreSQL adoption unexpectedly re-adopted a versioned schema".into(),
            ));
        }

        if adopt_legacy_postgres_schema(&dsn, &schema_drift).is_ok() {
            return Err(StorageError::Conflict(
                "PostgreSQL adoption unexpectedly accepted a drifted schema".into(),
            ));
        }
        client
            .batch_execute(&format!("SET search_path TO {schema_drift};"))
            .map_err(pg_error)?;
        let drift_ledger_exists: bool = client
            .query_one(
                "SELECT to_regclass('qx_schema_migrations') IS NOT NULL",
                &[],
            )
            .map_err(pg_error)?
            .get(0);
        if drift_ledger_exists {
            return Err(StorageError::Conflict(
                "rejected PostgreSQL schema adoption left a ledger behind".into(),
            ));
        }
        if adopt_legacy_postgres_schema(&dsn, &schema_missing).is_ok() {
            return Err(StorageError::Conflict(
                "PostgreSQL adoption unexpectedly created a missing schema".into(),
            ));
        }
        let missing_schema_exists: bool = client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)",
                &[&schema_missing],
            )
            .map_err(pg_error)?
            .get(0);
        if missing_schema_exists {
            return Err(StorageError::Conflict(
                "rejected PostgreSQL adoption created the missing schema".into(),
            ));
        }
        Ok(())
    })();

    client
        .batch_execute(&format!(
            "RESET search_path;
             DROP SCHEMA IF EXISTS {schema_v1} CASCADE;
             DROP SCHEMA IF EXISTS {schema_v2} CASCADE;
             DROP SCHEMA IF EXISTS {schema_drift} CASCADE;"
        ))
        .expect("drop isolated PostgreSQL adoption schemas");
    result.expect("PostgreSQL legacy schema adoption contract");
}

#[test]
#[ignore = "requires QX_TEST_POSTGRES_DSN and a running PostgreSQL instance"]
fn postgres_event_log_read_only_fallback_and_write_backfill_legacy_index() {
    let dsn = std::env::var("QX_TEST_POSTGRES_DSN")
        .expect("QX_TEST_POSTGRES_DSN must point at an isolated test database");
    let storage = PostgresStorage::connect_with_pool_size(&dsn, 1)
        .expect("connect PostgreSQL for EventLog index backfill contract");
    let suffix = format!(
        "{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos()
    );
    let schema = format!("qx_event_log_v4_{suffix}");
    let name = format!("legacy-{suffix}");
    let mut legacy = EventLog::new();
    for seq in 0..2 {
        legacy.append(Event::new(seq, seq + 1, Priority::POST, EventKind::Settle));
    }

    {
        let mut client = storage.lock_client().expect("lock isolated connection");
        client
            .batch_execute(&format!(
                "CREATE SCHEMA {schema}; SET search_path TO {schema};"
            ))
            .expect("create isolated v4 schema");
        client
            .batch_execute(super::super::schema_migrations::CREATE_SCHEMA_MIGRATIONS_SQL)
            .expect("create schema migration ledger");
        client
            .batch_execute(super::super::schema_migrations::POSTGRES_V1_SCHEMA)
            .expect("create legacy EventLog schema");
        client
            .execute(
                "INSERT INTO qx_schema_migrations(version, applied_ts) VALUES ('1', '0')",
                &[],
            )
            .expect("record legacy schema version");
        let content = legacy.to_json().expect("serialize legacy EventLog");
        client
            .execute(
                "INSERT INTO qx_event_logs(name, content) VALUES ($1, $2)",
                &[&name, &content],
            )
            .expect("insert legacy EventLog content");
        migrate_client(&mut client).expect("upgrade legacy schema through v5");
    }

    let store = PostgresEventLogStore {
        storage: storage.clone(),
    };
    let prefix_tail = legacy.events().last().expect("legacy EventLog has a tail");
    assert_eq!(
        store
            .read_since(
                &name,
                Some(prefix_tail.seq),
                Some((prefix_tail, legacy.len())),
                legacy.event_digest(),
                legacy.next_seq(),
            )
            .expect("unindexed v4 EventLog should fall back"),
        None
    );
    {
        let mut client = storage.lock_client().expect("lock isolated connection");
        let index_exists: bool = client
            .query_one(
                "SELECT EXISTS (
                     SELECT 1 FROM qx_event_log_heads WHERE name = $1
                 )",
                &[&name],
            )
            .expect("check EventLog index after fallback")
            .get(0);
        assert!(!index_exists, "read-only fallback must not write an index");
    }

    let mut extended = legacy.clone();
    extended.append(Event::new(2, 3, Priority::POST, EventKind::Settle));
    store
        .write(&name, &extended)
        .expect("next write backfills legacy history and appends atomically");
    assert_eq!(
        store
            .read_since(
                &name,
                Some(prefix_tail.seq),
                Some((prefix_tail, legacy.len())),
                legacy.event_digest(),
                legacy.next_seq(),
            )
            .expect("read indexed EventLog tail"),
        Some(vec![extended.events()[2].clone()])
    );

    let mut client = storage.lock_client().expect("lock isolated connection");
    client
        .batch_execute(&format!(
            "RESET search_path; DROP SCHEMA IF EXISTS {schema} CASCADE;"
        ))
        .expect("drop isolated EventLog schema");
}
