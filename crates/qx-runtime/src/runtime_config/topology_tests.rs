//! 存储拓扑与 API 传输面的 fail-closed 用例：从 `schema_tests.rs` 按主题拆出，
//! 因为那个文件越过了 500 行的单文件门槛，而预算登记只降不升。
//!
//! 本文件的每一条都在问同一件事：配置写出来能不能启动 —— 明文 API 的 bind、
//! 后端与消息拓扑的一致性、single_node 画像的排他性。

use super::*;

#[test]
fn runtime_config_round_trips_and_rejects_production_plaintext() {
    let encoded = config().to_json().unwrap();
    assert_eq!(RuntimeConfig::from_json(&encoded).unwrap(), config());
    let mut production = config();
    production.environment = "production".into();
    assert!(production.validate().is_err());

    let mut mtls = config();
    mtls.api.transport = ApiTransport::Mtls;
    assert!(mtls.validate().is_err());
    mtls.api.tls = Some(TlsPaths {
        certificate_chain: "server.pem".into(),
        private_key: "server.key".into(),
        client_ca: "clients.pem".into(),
    });
    assert!(mtls.validate().is_err());
}

/// 明文 API 的边界是**地址**，不是 `environment` 的措辞：明文面拿不到 operator 身份，
/// `qx-api` 在没有装访问策略时直接把请求体里的 `permission` 当授予档位，所以
/// `environment` 写成 paper/sandbox/testnet（仓库模板正在用的值）都不该等于"可以对外"。
#[test]
fn plaintext_api_is_confined_to_loopback_binds() {
    let mut routable = config();
    routable.api.bind = "10.20.30.40:8443".into();
    let error = routable.validate().unwrap_err();
    assert!(
        error.contains("明文 API 只能绑定回环地址"),
        "可路由的明文 bind 必须按地址拒绝，实际报错: {error}"
    );

    let mut unspecified = config();
    unspecified.api.bind = "0.0.0.0:8443".into();
    assert!(unspecified.validate().is_err());

    let mut loopback_v6 = config();
    loopback_v6.api.bind = "[::1]:8443".into();
    assert!(
        loopback_v6.validate().is_ok(),
        "回环 IPv6 与 127.0.0.1 同义，本地调试口径不能误伤"
    );

    // 闸门只管明文：带 tls 与 Operator 证书映射的 mTLS 部署仍必须能绑可路由地址。
    let mut exposed = config();
    exposed.api.bind = "0.0.0.0:8443".into();
    exposed.api.transport = ApiTransport::Mtls;
    exposed.api.tls = Some(TlsPaths {
        certificate_chain: "server.pem".into(),
        private_key: "server.key".into(),
        client_ca: "clients.pem".into(),
    });
    exposed.api.operators.insert(
        "ops".into(),
        OperatorConfig {
            permission: Permission::Admin,
            certificate: "ops.pem".into(),
        },
    );
    assert!(
        exposed.validate().is_ok(),
        "mTLS 部署绑可路由地址不得被这条回环闸门误伤"
    );

    // 生产 mTLS 那份配置该由生产自身的规则（Postgres backend 等）把关，
    // 而不是这条按地址判的明文闸门：报错文案必须不含回环字样。
    exposed.environment = "production".into();
    let error = exposed.validate().unwrap_err();
    assert!(
        !error.contains("明文 API 只能绑定回环地址"),
        "生产 mTLS 的拒绝理由不能来自这条明文闸门，实际报错: {error}"
    );
}

#[test]
fn storage_consistency_matches_backend_and_messaging_topology() {
    let mut files = config();
    files.storage.consistency = StorageConsistency::Transactional;
    assert!(files
        .validate()
        .unwrap_err()
        .contains("Files/SQLite backend"));

    let mut postgres = config();
    postgres.profile = RuntimeProfile::Distributed;
    postgres.storage.backend = StorageBackend::Postgres;
    postgres.storage.postgres_dsn_env = Some("QX_POSTGRES_DSN".into());
    assert!(postgres
        .validate()
        .unwrap_err()
        .contains("不能声明 local_durable"));
    postgres.storage.consistency = StorageConsistency::Transactional;
    assert!(postgres.validate().is_ok());

    let mut messaging = config();
    messaging.profile = RuntimeProfile::Distributed;
    messaging.messaging.enabled = true;
    assert!(messaging
        .validate()
        .unwrap_err()
        .contains("distributed_outbox"));
    messaging.storage.consistency = StorageConsistency::DistributedOutbox;
    assert!(messaging.validate().is_ok());
}

#[test]
fn messaging_worker_requires_valid_runtime_contract() {
    let mut relay_config = config();
    relay_config.profile = RuntimeProfile::Distributed;
    relay_config.storage.consistency = StorageConsistency::DistributedOutbox;
    relay_config.workers.push(WorkerConfig {
        id: "outbox-relay".into(),
        role: WorkerRole::OutboxRelay,
        enabled: true,
        account_id: None,
        venue_id: None,
        endpoint: None,
        symbols: Vec::new(),
        settlement_currency: None,
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    });
    assert!(relay_config.validate().is_err());
    relay_config.messaging.enabled = true;
    assert!(relay_config.validate().is_ok());
    relay_config.messaging.relay_batch_size = 0;
    assert!(relay_config.validate().is_err());
    relay_config.messaging.relay_batch_size = 100;
    relay_config.messaging.worker_stale_after_ms = 0;
    assert!(relay_config.validate().is_err());

    let mut consumer = config();
    consumer.profile = RuntimeProfile::Distributed;
    consumer.storage.consistency = StorageConsistency::DistributedOutbox;
    consumer.messaging.enabled = true;
    consumer.messaging.consumer_stream = Some("QIANXING_EVENTS".into());
    consumer.messaging.consumer_name = Some("ledger-reducer".into());
    consumer.messaging.consumer_group_id = Some("ledger-reducer".into());
    consumer.messaging.consumer_handler_executable = Some("python".into());
    consumer.workers.push(WorkerConfig {
        id: "ledger-reducer".into(),
        role: WorkerRole::EventConsumer,
        enabled: true,
        account_id: None,
        venue_id: None,
        endpoint: None,
        symbols: Vec::new(),
        settlement_currency: None,
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    });
    assert!(consumer.validate().is_ok());
    consumer.messaging.consumer_handler_timeout_ms = 0;
    assert!(consumer.validate().is_err());
}

#[test]
fn postgres_storage_requires_secret_manager_environment_name() {
    let mut config = config();
    config.profile = RuntimeProfile::Distributed;
    config.storage.backend = StorageBackend::Postgres;
    config.storage.consistency = StorageConsistency::Transactional;
    assert!(config.validate().is_err());
    config.storage.postgres_dsn_env = Some("QX_POSTGRES_DSN".into());
    assert!(config.validate().is_ok());
    config.storage.postgres_pool_size = 0;
    assert!(config.validate().is_err());
    config.storage.postgres_pool_size = 8;
    assert!(config.validate().is_ok());
    let json = serde_json::to_string(&config).unwrap();
    assert!(!json.contains("postgresql://"));
}

#[test]
fn single_node_profile_rejects_postgres_and_nats() {
    let mut postgres = config();
    postgres.storage.backend = StorageBackend::Postgres;
    postgres.storage.postgres_dsn_env = Some("QX_POSTGRES_DSN".into());
    let error = postgres
        .validate()
        .expect_err("single_node must reject PostgreSQL");
    assert!(error.contains("single_node") && error.contains("PostgreSQL"));

    let mut messaging = config();
    messaging.messaging.enabled = true;
    let error = messaging
        .validate()
        .expect_err("single_node must reject NATS messaging");
    assert!(error.contains("single_node") && error.contains("NATS"));
}

/// `environment` 不是自由字符串：它决定 9 处配置面的 `production` 专属加固闸门是否生效，
/// 以及实时策略作业的 `dry_run` 走模拟还是真实提交（只有 `paper` 是模拟）。只按"非空"
/// 校验时，一个多打的空格就能让 `paper` 配置落进真实提交臂、让拼错的 `production`
/// 关掉全部生产加固后照常启动。
#[test]
fn environment_outside_the_closed_vocab_is_rejected_not_silently_branched() {
    let vocab = ENVIRONMENT_VOCAB.join("/");
    for spelling in ENVIRONMENT_VOCAB {
        let mut accepted = config();
        accepted.environment = spelling.to_string();
        if let Err(error) = accepted.validate() {
            assert!(
                !error.contains("environment 必须是"),
                "名单内的 {spelling:?} 不该被名单闸门拒掉，实际报错: {error}"
            );
        }
    }
    // 大小写不敏感：混排写法仍是同一个环境，生产加固不能因为写法而掉。
    let mut mixed_case = config();
    mixed_case.environment = "PRODUCTION".into();
    let error = mixed_case.validate().unwrap_err();
    assert!(
        error.contains("production 环境禁止使用明文 API"),
        "PRODUCTION 必须仍算生产环境，实际报错: {error}"
    );

    for spelling in [
        "",
        " ",
        " paper",
        "paper ",
        "Production ",
        "prod",
        "produciton",
        "sim",
        "live",
        "dev",
        "test",
        "paper-v2",
    ] {
        let mut rejected = config();
        rejected.environment = spelling.into();
        let error = rejected.validate().unwrap_err();
        assert!(
            error.contains("environment 必须是") && error.contains(&vocab),
            "名单外的 {spelling:?} 必须被闭合名单拒绝并在报错里给出同一份名单，实际报错: {error}"
        );
    }
}
