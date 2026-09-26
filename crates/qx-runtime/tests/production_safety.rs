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
        assert!(
            error.contains(field),
            "unexpected validation error: {error}"
        );
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

/// 真·production 夹具上的钱闸门：环境名的写法不得改变哪条风控生效。
/// 旧写法逐字面量比较，`" production "` 会跳过 `max_*_notional_raw` 两条上限判定，
/// 而配置仍然校验通过（V11 §41 E1）。
#[test]
fn production_money_gates_survive_environment_spelling() {
    for spelling in ["production", "PRODUCTION", " production "] {
        for field in ["max_order_notional_raw", "max_position_notional_raw"] {
            let mut config = production_config();
            config.environment = spelling.into();
            assert!(
                config.is_production(),
                "{spelling:?} 必须按 production 判定"
            );
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
                .expect_err(&format!("{spelling:?} 缺 {field} 必须 fail closed"));
            assert!(error.contains(field), "{spelling:?} => {error}");
        }
    }
}

/// 近义词不是"更严格的环境"：它在词表外，必须当场被拒绝，而不是按非 production 放行。
#[test]
fn production_example_rejects_environments_outside_the_vocabulary() {
    for spelling in ["prod", "PRODUCT", "production-line", " preprod"] {
        let mut config = production_config();
        config.environment = spelling.into();
        assert!(
            !config.is_production(),
            "{spelling:?} 不该被当成 production"
        );
        let error = config
            .validate()
            .expect_err(&format!("{spelling:?} 不是合法 environment"));
        assert!(error.contains("之一，当前为"), "{spelling:?} => {error}");
    }
}
