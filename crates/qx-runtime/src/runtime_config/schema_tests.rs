//! 配置 schema、指纹、worker 角色字段策略与存储拓扑用例。

use super::*;

/// 配置面必须 fail-closed：未知键（含风控字段拼错）在反序列化阶段就失败，
/// 而 `_` 前缀的运维注释键被递归剥离且不进入配置指纹。
#[test]
fn runtime_config_rejects_unknown_keys_and_strips_comment_keys() {
    let base = config();
    let fingerprint = base.fingerprint().unwrap();
    let payload = base.to_json().unwrap();
    let mutate = |edit: &dyn Fn(&mut serde_json::Value)| -> String {
        let mut value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        edit(&mut value);
        serde_json::to_string(&value).unwrap()
    };
    let with_top_comment = mutate(&|value| {
        value["_comment"] = serde_json::json!("生产部署说明");
    });
    assert_eq!(RuntimeConfig::from_json(&with_top_comment).unwrap(), base);
    assert_eq!(
        RuntimeConfig::from_json(&with_top_comment)
            .unwrap()
            .fingerprint()
            .unwrap(),
        fingerprint
    );
    let with_worker_comment = mutate(&|value| {
        value["workers"][0]["_comment"] = serde_json::json!("该 worker 只跑行情");
    });
    assert_eq!(
        RuntimeConfig::from_json(&with_worker_comment).unwrap(),
        base
    );
    let typo_in_worker = mutate(&|value| {
        value["workers"][0]["max_order_notional_raws"] = serde_json::json!(1_000_000);
    });
    let error = RuntimeConfig::from_json(&typo_in_worker).unwrap_err();
    assert!(error.contains("max_order_notional_raws"), "{error}");
    let typo_in_storage = mutate(&|value| {
        value["storage"]["sqlite_pat"] = serde_json::json!("runtime/events.jsonl");
    });
    assert!(RuntimeConfig::from_json(&typo_in_storage)
        .unwrap_err()
        .contains("sqlite_pat"));
    let typo_in_strategy = mutate(&|value| {
        value["strategy"]["alow_short"] = serde_json::json!(true);
    });
    assert!(RuntimeConfig::from_json(&typo_in_strategy)
        .unwrap_err()
        .contains("alow_short"));
}

/// 角色字段可见性必须由启动校验咬住，且与 `enabled` 开关无关。
#[test]
fn worker_field_policy_is_enforced_when_validating_runtime_config() {
    let mut with_api_symbols = config();
    with_api_symbols
        .workers
        .iter_mut()
        .find(|worker| worker.role == WorkerRole::Api)
        .expect("测试配置需要 api worker")
        .symbols = vec!["BTCUSDT.BINANCE".into()];
    let error = with_api_symbols.validate().unwrap_err();
    assert!(
        error.contains("symbols") && error.contains("Api"),
        "{error}"
    );

    // 禁用不是豁免：字段绑错角色的风险与是否启动无关。
    let mut disabled_misbinding = config();
    let api = disabled_misbinding
        .workers
        .iter_mut()
        .find(|worker| worker.role == WorkerRole::Api)
        .expect("测试配置需要 api worker");
    api.enabled = false;
    api.credential_env = Some(CredentialEnv {
        api_key: "QX_KEY".into(),
        secret: "QX_SECRET".into(),
    });
    let error = disabled_misbinding.validate().unwrap_err();
    assert!(error.contains("credential_env"), "{error}");

    // 半空的凭据引用在任何 Venue 下都非法，不能退化成"没有凭据"。
    let mut half_credential = config();
    let market = half_credential
        .workers
        .iter_mut()
        .find(|worker| worker.role == WorkerRole::MarketData)
        .expect("测试配置需要 market data worker");
    market.venue_id = Some("okx".into());
    market.credential_env = Some(CredentialEnv {
        api_key: "QX_OKX_KEY".into(),
        secret: " ".into(),
    });
    assert!(half_credential
        .validate()
        .unwrap_err()
        .contains("credential_env 或 credential_files"));
}

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

#[test]
fn spread_recovery_requires_the_same_account_boundary_as_execution() {
    let mut config = config();
    config.workers.push(WorkerConfig {
        id: "paper-recovery".into(),
        role: WorkerRole::SpreadRecovery,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("paper".into()),
        endpoint: None,
        symbols: Vec::new(),
        settlement_currency: Some("USDT".into()),
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    });
    assert!(config.validate().is_ok());
    config.workers.last_mut().unwrap().account_id = None;
    assert!(config.validate().is_err());
}

#[test]
fn runtime_config_fingerprint_locks_published_configuration() {
    let mut locked = config();
    let fingerprint = locked.fingerprint().unwrap();
    assert_eq!(fingerprint.len(), 64);
    locked.config_fingerprint = Some(fingerprint);
    let encoded = locked.to_json().unwrap();
    assert_eq!(RuntimeConfig::from_json(&encoded).unwrap(), locked);

    let mut tampered = locked.clone();
    tampered.storage.data_dir = "data/tampered".into();
    let tampered_payload = serde_json::to_string(&tampered).unwrap();
    let error = RuntimeConfig::from_json(&tampered_payload).unwrap_err();
    assert!(error.contains("配置指纹不匹配"));
}

#[test]
fn segmented_event_log_storage_configuration_is_positive_and_optional() {
    let mut config = config();
    assert_eq!(config.storage.event_log_segment_events, None);
    config.storage.event_log_segment_events = Some(1024);
    assert!(config.validate().is_ok());
    config.storage.event_log_segment_events = Some(0);
    assert!(config.validate().is_err());
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
fn worker_ids_are_safe_for_runtime_artifact_names() {
    let mut invalid = config();
    invalid.profile = RuntimeProfile::Distributed;
    invalid.messaging.enabled = true;
    invalid.workers.push(WorkerConfig {
        id: "relay/primary".into(),
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
    assert!(invalid.validate().is_err());
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

#[test]
fn binance_private_workers_require_one_valid_credential_source() {
    let mut invalid = config();
    invalid.workers.push(WorkerConfig {
        id: "user".into(),
        role: WorkerRole::UserStream,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("binance-testnet".into()),
        endpoint: Some("wss://ws-api.testnet.binance.vision/ws-api/v3".into()),
        symbols: Vec::new(),
        settlement_currency: None,
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    });
    assert!(invalid.validate().is_err());
    invalid.workers.last_mut().unwrap().credential_env = Some(CredentialEnv {
        api_key: "QX_BINANCE_TESTNET_API_KEY".into(),
        secret: "QX_BINANCE_TESTNET_API_SECRET".into(),
    });
    assert!(invalid.validate().is_ok());
    invalid.workers.last_mut().unwrap().credential_files = Some(CredentialFiles {
        api_key: "/run/secrets/api-key".into(),
        secret: "/run/secrets/secret".into(),
    });
    assert!(invalid.validate().is_err());
    invalid.workers.last_mut().unwrap().credential_env = None;
    assert!(invalid.validate().is_ok());
    invalid.workers.last_mut().unwrap().credential_files = None;
    invalid.workers.last_mut().unwrap().credential_env = Some(CredentialEnv {
        api_key: "QX-BAD".into(),
        secret: "QX_SECRET".into(),
    });
    assert!(invalid.validate().is_err());
}

#[test]
fn execution_worker_requires_account_venue_and_credentials() {
    let mut config = config();
    config.workers.push(WorkerConfig {
        id: "execution".into(),
        role: WorkerRole::Execution,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("binance-testnet".into()),
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
    assert!(config.validate().is_err());
    config.workers.last_mut().unwrap().credential_env = Some(CredentialEnv {
        api_key: "QX_BINANCE_TESTNET_API_KEY".into(),
        secret: "QX_BINANCE_TESTNET_API_SECRET".into(),
    });
    config.workers.last_mut().unwrap().instrument_spec_path = Some("market-spec.json".into());
    assert!(config.validate().is_ok());
    config.workers.last_mut().unwrap().credential_env = None;
    config.workers.last_mut().unwrap().credential_files = Some(CredentialFiles {
        api_key: "/run/secrets/api-key".into(),
        secret: "/run/secrets/secret".into(),
    });
    assert!(config.validate().is_ok());
}

#[test]
fn execution_risk_limits_require_a_frozen_instrument_spec() {
    let mut config = config();
    config.workers.push(WorkerConfig {
        id: "paper-execution".into(),
        role: WorkerRole::Execution,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("paper".into()),
        endpoint: None,
        symbols: Vec::new(),
        settlement_currency: Some("USDT".into()),
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: Some(1_000),
        max_position_notional_raw: None,
    });
    assert!(config.validate().is_err());
    config.workers.last_mut().unwrap().instrument_spec_path = Some("market-spec.json".into());
    assert!(config.validate().is_ok());
}
