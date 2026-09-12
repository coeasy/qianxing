use qx_runtime::{RuntimeConfig, StorageBackend, WorkerRole};

fn production_config() -> RuntimeConfig {
    serde_json::from_str(include_str!(
        "../../../deploy/qianxing.runtime.production.example.json"
    ))
    .expect("production example must parse")
}

#[test]
fn production_execution_requires_frozen_instrument_spec_even_for_paper_venue() {
    let mut config = production_config();

    let execution = config
        .workers
        .iter_mut()
        .find(|worker| worker.role == WorkerRole::Execution)
        .expect("production example must contain an execution worker");
    execution.venue_id = Some("paper".into());
    execution.instrument_spec_path = None;
    execution.credential_env = None;

    let error = config
        .validate()
        .expect_err("production execution without a frozen spec must fail closed");
    assert!(
        error.contains("instrument_spec_path"),
        "unexpected validation error: {error}"
    );
}

#[test]
fn production_execution_requires_explicit_order_and_position_notional_limits() {
    for field in ["max_order_notional_raw", "max_position_notional_raw"] {
        let mut config = production_config();
        let execution = config
            .workers
            .iter_mut()
            .find(|worker| worker.role == WorkerRole::Execution)
            .expect("production example must contain an execution worker");
        match field {
            "max_order_notional_raw" => execution.max_order_notional_raw = None,
            "max_position_notional_raw" => execution.max_position_notional_raw = None,
            _ => unreachable!(),
        }

        let error = config
            .validate()
            .expect_err("production execution without explicit notional limits must fail closed");
        assert!(error.contains(field), "unexpected validation error: {error}");
    }
}

#[test]
fn production_requires_postgres_transactional_event_store() {
    for backend in [StorageBackend::Files, StorageBackend::Sqlite] {
        let mut config = production_config();
        config.storage.backend = backend;
        if backend == StorageBackend::Sqlite {
            config.storage.sqlite_path = Some("runtime.sqlite3".into());
        }
        config.storage.postgres_dsn_env = None;

        let error = config
            .validate()
            .expect_err("production must reject non-transactional EventLog/Outbox backends");
        assert!(
            error.contains("PostgreSQL") || error.contains("postgres"),
            "unexpected validation error: {error}"
        );
    }
}

#[test]
fn production_example_is_fail_closed_and_valid() {
    production_config()
        .validate()
        .expect("the production example itself must satisfy all fail-closed safety contracts");
}
