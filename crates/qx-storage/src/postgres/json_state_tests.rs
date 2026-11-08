use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, PartialEq, Serialize, serde::Deserialize)]
struct JsonStateFixture {
    generation: u64,
}

#[test]
#[ignore = "requires QX_TEST_POSTGRES_DSN and a running PostgreSQL instance"]
fn postgres_json_state_transactions_persist_across_connections_and_roll_back_rejections() {
    let dsn = std::env::var("QX_TEST_POSTGRES_DSN")
        .expect("QX_TEST_POSTGRES_DSN must point at an isolated test database");
    let state_key = format!(
        "contract/json-state/{}/{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos()
    );
    let first = PostgresJsonStateStore::connect(&dsn).expect("connect first state store");
    assert_eq!(
        first
            .load_json_at::<JsonStateFixture>(&state_key)
            .expect("read missing state"),
        None
    );

    let (stored, result) = first
        .transact_json_at(&state_key, JsonStateFixture { generation: 0 }, |state| {
            state.generation += 1;
            Ok::<_, String>(state.generation)
        })
        .expect("commit state transaction");
    assert_eq!(stored.generation, 1);
    assert_eq!(result, Ok(1));

    let (rejected_state, rejected) = first
        .transact_json_at(&state_key, JsonStateFixture { generation: 0 }, |state| {
            state.generation = 99;
            Err::<(), _>("business rejection")
        })
        .expect("business rejection is not a storage error");
    assert_eq!(rejected_state.generation, 99);
    assert_eq!(rejected, Err("business rejection"));

    let second = PostgresJsonStateStore::connect(&dsn).expect("connect second state store");
    let restored = second
        .load_json_at::<JsonStateFixture>(&state_key)
        .expect("read committed state")
        .expect("committed state exists");
    assert_eq!(restored.generation, 1);

    first
        .storage
        .lock_client()
        .expect("lock cleanup connection")
        .execute(
            "DELETE FROM qx_json_state WHERE state_key = $1",
            &[&state_key],
        )
        .expect("remove unique test state");
}

#[test]
#[ignore = "requires QX_TEST_POSTGRES_DSN and a running PostgreSQL instance"]
fn postgres_deployment_blobs_are_immutable_and_content_addressable() {
    let dsn = std::env::var("QX_TEST_POSTGRES_DSN")
        .expect("QX_TEST_POSTGRES_DSN must point at an isolated test database");
    let store = PostgresBlobStore::connect(&dsn).expect("connect PostgreSQL blob store");
    let content = b"strategy-bundle-v1";
    let key = deployment_blob_sha256(content);
    let blob = (key.clone(), content.to_vec());
    store
        .put_blobs_at(std::slice::from_ref(&blob))
        .expect("insert blob");
    store
        .put_blobs_at(std::slice::from_ref(&blob))
        .expect("idempotent immutable insert");
    assert_eq!(
        store.load_blob_at(&key).expect("load blob").as_deref(),
        Some(content.as_slice())
    );
    let conflicting_blob = (key.clone(), b"different content".to_vec());
    assert!(store.put_blobs_at(&[conflicting_blob]).is_err());
    store
        .storage
        .lock_client()
        .expect("lock cleanup connection")
        .execute("DELETE FROM qx_deployment_blobs WHERE sha256 = $1", &[&key])
        .expect("remove unique test blob");
}
