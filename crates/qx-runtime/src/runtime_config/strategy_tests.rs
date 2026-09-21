//! 策略实例配置与策略绑定拓扑用例。

use super::*;

#[test]
fn production_bound_strategy_requires_research_snapshot() {
    let mut config = config();
    config.profile = RuntimeProfile::Distributed;
    config.environment = "production".into();
    config.storage.backend = StorageBackend::Postgres;
    config.storage.consistency = StorageConsistency::Transactional;
    config.storage.postgres_dsn_env = Some("QX_POSTGRES_DSN".into());
    config.api.transport = ApiTransport::Mtls;
    config.api.tls = Some(TlsPaths {
        certificate_chain: "server.pem".into(),
        private_key: "server.key".into(),
        client_ca: "clients.pem".into(),
    });
    config.api.operators.insert(
        "ops".into(),
        OperatorConfig {
            permission: Permission::Admin,
            certificate: "ops.pem".into(),
        },
    );
    config.workers.push(WorkerConfig {
        id: "strategy-main".into(),
        role: WorkerRole::Strategy,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("binance".into()),
        endpoint: None,
        symbols: vec!["BTCUSDT.BINANCE".into()],
        settlement_currency: None,
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    });
    config.strategy.account_id = Some("main".into());
    config.strategy.venue_id = Some("binance".into());
    config.strategy.instrument = Some("BTCUSDT.BINANCE".into());
    assert!(config.validate().is_err());

    config.strategy.research_snapshot_required = true;
    config.strategy.research_snapshot_path = Some("research.json".into());
    config.strategy.research_data_fingerprint = Some("bars-sha256".into());
    config.strategy.dataset_bundle_path = Some("research.bundle.json".into());
    assert!(config.validate().is_ok());

    config.strategy.target_qty = 1;
    assert!(config
        .validate()
        .unwrap_err()
        .contains("禁止使用裸 target_qty"));
}

#[test]
fn research_snapshot_required_rejects_missing_path() {
    let mut config = config();
    config.strategy.research_snapshot_required = true;
    assert!(config.validate().is_err());
}

#[test]
fn c_abi_strategy_requires_digest_and_exclusive_source() {
    let mut config = config();
    config.strategy.c_abi_library = Some("strategy.dll".into());
    assert!(config.validate().is_err());

    config.strategy.c_abi_sha256 = Some("ab".repeat(32));
    assert!(config.validate().is_ok());

    config.strategy.python_module = Some("example_strategy".into());
    assert!(config.validate().is_err());

    config.strategy.python_module = None;
    config.strategy.c_abi_ed25519_public_key = Some("00".repeat(32));
    assert!(config.validate().is_err());
}

#[test]
fn builtin_strategy_requires_valid_name_snapshot_and_exclusive_source() {
    let mut config = config();
    config.strategy.builtin_strategy = Some("macd".into());
    let error = config.validate().unwrap_err();
    assert!(error.contains("bars_snapshot_path"));

    config.strategy.bars_snapshot_path = Some("bars.json".into());
    assert!(config.validate().is_ok());

    config.strategy.builtin_strategy = Some("not-exists".into());
    assert!(config.validate().is_err());

    config.strategy.builtin_strategy = Some("macd".into());
    config.strategy.python_module = Some("demo_strategy".into());
    let error = config.validate().unwrap_err();
    assert!(error.contains("只能配置一个"));
}

#[test]
fn live_builtin_and_pair_arbitrage_require_stream_inputs() {
    let mut config = config();
    config.strategy.live_enabled = true;
    assert!(config
        .validate()
        .unwrap_err()
        .contains("bars_snapshot_path"));
    config.strategy.bars_snapshot_path = Some("primary.json".into());
    config.strategy.builtin_strategy = Some("pairs_arbitrage".into());
    let error = config.validate().unwrap_err();
    assert!(error.contains("builtin_reference_instrument"));
    config.strategy.builtin_reference_instrument = Some("ETHUSDT.BINANCE".into());
    config.strategy.builtin_reference_bars_snapshot_path = Some("reference.json".into());
    assert!(config.validate().is_ok());
}

#[test]
fn live_strategy_requires_market_data_and_execution_topology() {
    let mut config = config();
    config.workers.push(WorkerConfig {
        id: "strategy-live".into(),
        role: WorkerRole::Strategy,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("okx".into()),
        endpoint: None,
        symbols: vec!["BTC/USDT.OKX".into()],
        settlement_currency: None,
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    });
    config.strategy.account_id = Some("main".into());
    config.strategy.venue_id = Some("okx".into());
    config.strategy.instrument = Some("BTC/USDT.OKX".into());
    config.strategy.live_enabled = true;
    config.strategy.bars_snapshot_path = Some("bars.json".into());

    let error = config.validate().unwrap_err();
    assert!(error.contains("MarketData worker"));

    config.workers[1].symbols = vec!["BTC/USDT.OKX".into()];
    let error = config.validate().unwrap_err();
    assert!(error.contains("Execution worker"));

    config.workers.push(WorkerConfig {
        id: "execution-live".into(),
        role: WorkerRole::Execution,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("OKX".into()),
        endpoint: None,
        symbols: Vec::new(),
        settlement_currency: Some("USDT".into()),
        credential_env: None,
        credential_files: None,
        instrument_spec_path: Some("spec.json".into()),
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    });
    let result = config.validate();
    assert!(result.is_ok(), "{result:?}");
    config.strategy.live_max_staleness_ms = Some(0);
    assert!(config
        .validate()
        .unwrap_err()
        .contains("live_max_staleness_ms"));
}

#[test]
fn production_c_abi_strategy_requires_detached_signature() {
    let mut config = config();
    config.profile = RuntimeProfile::Distributed;
    config.environment = "production".into();
    config.storage.backend = StorageBackend::Postgres;
    config.storage.consistency = StorageConsistency::Transactional;
    config.storage.postgres_dsn_env = Some("QX_POSTGRES_DSN".into());
    config.api.transport = ApiTransport::Mtls;
    config.api.tls = Some(TlsPaths {
        certificate_chain: "server.pem".into(),
        private_key: "server.key".into(),
        client_ca: "clients.pem".into(),
    });
    config.api.operators.insert(
        "ops".into(),
        OperatorConfig {
            permission: Permission::Admin,
            certificate: "ops.pem".into(),
        },
    );
    config.strategy.c_abi_library = Some("strategy.dll".into());
    config.strategy.c_abi_sha256 = Some("ab".repeat(32));
    assert!(config.validate().is_err());
    config.strategy.c_abi_ed25519_public_key = Some("00".repeat(32));
    config.strategy.c_abi_ed25519_signature = Some("00".repeat(64));
    assert!(config.validate().is_ok());
}

#[test]
fn production_external_strategy_requires_artifact_lock() {
    let mut config = config();
    config.profile = RuntimeProfile::Distributed;
    config.environment = "production".into();
    config.storage.backend = StorageBackend::Postgres;
    config.storage.consistency = StorageConsistency::Transactional;
    config.storage.postgres_dsn_env = Some("QX_POSTGRES_DSN".into());
    config.api.transport = ApiTransport::Mtls;
    config.api.tls = Some(TlsPaths {
        certificate_chain: "server.pem".into(),
        private_key: "server.key".into(),
        client_ca: "clients.pem".into(),
    });
    config.api.operators.insert(
        "ops".into(),
        OperatorConfig {
            permission: Permission::Admin,
            certificate: "ops.pem".into(),
        },
    );
    config.strategy.python_module = Some("strategy.production".into());
    let error = config.validate().unwrap_err();
    assert!(error.contains("strategy_artifact_sha256"));
    config.strategy.strategy_artifact_sha256 = Some("00".repeat(32));
    assert!(config.validate().is_ok());
}

#[test]
fn multi_strategy_instances_are_bound_to_workers_and_jobs_can_select_them() {
    let mut config = config();
    config.workers.push(WorkerConfig {
        id: "strategy-alpha".into(),
        role: WorkerRole::Strategy,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("okx".into()),
        endpoint: None,
        symbols: vec!["BTC/USDT.OKX".into()],
        settlement_currency: None,
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    });
    config.strategies.push(StrategyRuntimeConfig {
        id: Some("strategy-alpha".into()),
        version: "alpha-v1".into(),
        max_orders: 10,
        risk_rules: None,
        cost_rules_path: None,
        account_id: Some("main".into()),
        venue_id: Some("okx".into()),
        instrument: Some("BTC/USDT.OKX".into()),
        target_qty: 0,
        target_snapshot_path: None,
        research_snapshot_path: None,
        research_snapshot_required: false,
        research_data_fingerprint: None,
        dataset_bundle_path: None,
        dataset_component_paths: BTreeMap::new(),
        product: None,
        margin_mode: None,
        position_mode: None,
        leverage: None,
        allow_short: None,
        live_enabled: false,
        live_timeframe: default_strategy_live_timeframe(),
        live_history_limit: default_strategy_live_history_limit(),
        live_closed_only: default_strategy_live_closed_only(),
        live_max_staleness_ms: None,
        builtin_strategy: None,
        builtin_quantity: None,
        builtin_fast_window: None,
        builtin_slow_window: None,
        builtin_period: None,
        builtin_threshold_bps: None,
        builtin_reference_instrument: None,
        builtin_reference_bars_snapshot_path: None,
        builtin_reference_margin_mode: None,
        builtin_reference_position_mode: None,
        builtin_reference_leverage: None,
        bars_snapshot_path: None,
        ashare_rules_path: None,
        ashare_actions_path: None,
        ashare_calendar_path: None,
        python_module: None,
        transport: StrategyTransport::Jsonl,
        shared_memory_capacity: default_strategy_shared_memory_capacity(),
        shared_memory_slot_bytes: default_strategy_shared_memory_slot_bytes(),
        python_timeout_ms: default_strategy_python_timeout_ms(),
        external_executable: None,
        strategy_artifact_sha256: None,
        external_args: Vec::new(),
        external_env: BTreeMap::new(),
        c_abi_library: None,
        c_abi_sha256: None,
        c_abi_max_library_bytes: default_strategy_c_abi_max_library_bytes(),
        c_abi_ed25519_public_key: None,
        c_abi_ed25519_signature: None,
    });
    config.validate().unwrap();
    assert_eq!(
        config
            .strategy_for_worker("strategy-alpha")
            .unwrap()
            .version,
        "alpha-v1"
    );
    assert!(config.strategy_for_worker("strategy-missing").is_err());
}

#[test]
fn strategy_process_configuration_is_exclusive_and_validated() {
    let mut config = config();
    config.strategy.python_module = Some("demo_strategy".into());
    config.strategy.external_executable = Some("strategy.exe".into());
    assert!(config.validate().is_err());

    config.strategy.python_module = None;
    config.strategy.external_executable = Some("strategy.exe".into());
    config.strategy.external_args = vec!["--mode".into(), "jsonl".into()];
    config
        .strategy
        .external_env
        .insert("QX_MODE".into(), "paper".into());
    assert!(config.validate().is_ok());
    config
        .strategy
        .external_env
        .insert("EXCHANGE_API_KEY".into(), "must-not-pass".into());
    assert!(config.validate().is_err());
}

#[test]
fn strategy_binding_requires_a_matching_enabled_worker_and_nonnegative_target() {
    let mut config = config();
    config.strategy.account_id = Some("main".into());
    config.strategy.venue_id = Some("paper".into());
    config.strategy.instrument = Some("BTCUSDT.BINANCE".into());
    assert!(config.validate().is_err());

    config.workers.push(WorkerConfig {
        id: "strategy".into(),
        role: WorkerRole::Strategy,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("paper".into()),
        endpoint: None,
        symbols: vec!["BTCUSDT.BINANCE".into()],
        settlement_currency: None,
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    });
    assert!(config.validate().is_ok());
    config.strategy.target_qty = -1;
    assert!(config.validate().is_err());
}

#[test]
fn strategy_target_snapshot_is_bound_to_runtime_version_and_time() {
    let snapshot = StrategyTargetSnapshot {
        schema_version: StrategyTargetSnapshot::SCHEMA_VERSION,
        strategy_version: "strategy-runtime-v1".into(),
        data_fingerprint: "data-1".into(),
        as_of: 10,
        targets: BTreeMap::from([("BTCUSDT.BINANCE".into(), 1)]),
    };
    assert!(snapshot.validate_for("strategy-runtime-v1", 10).is_ok());
    assert!(snapshot.validate_for("strategy-runtime-v2", 10).is_err());
    assert!(snapshot.validate_for("strategy-runtime-v1", 9).is_err());
}
