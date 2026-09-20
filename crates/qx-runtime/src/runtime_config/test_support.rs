//! 运行时配置用例共享的最小可用配置构造器。

use super::*;

pub(crate) fn config() -> RuntimeConfig {
    RuntimeConfig {
        schema_version: RUNTIME_SCHEMA_VERSION,
        environment: "paper".into(),
        profile: RuntimeProfile::SingleNode,
        config_fingerprint: None,
        api: ApiRuntimeConfig {
            bind: "127.0.0.1:19090".into(),
            transport: ApiTransport::Plaintext,
            tls: None,
            operators: BTreeMap::new(),
        },
        storage: StorageRuntimeConfig {
            backend: StorageBackend::Files,
            consistency: StorageConsistency::LocalDurable,
            data_dir: "data".into(),
            sqlite_path: None,
            postgres_dsn_env: None,
            postgres_pool_size: default_postgres_pool_size(),
            event_log_segment_events: None,
        },
        messaging: MessagingRuntimeConfig::default(),
        workers: vec![
            WorkerConfig {
                id: "api".into(),
                role: WorkerRole::Api,
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
            },
            WorkerConfig {
                id: "market".into(),
                role: WorkerRole::MarketData,
                enabled: true,
                account_id: None,
                venue_id: None,
                endpoint: Some("https://example.test".into()),
                symbols: vec!["BTCUSDT.BINANCE".into()],
                settlement_currency: None,
                credential_env: None,
                credential_files: None,
                instrument_spec_path: None,
                paper_initial_cash_raw: None,
                max_order_notional_raw: None,
                max_position_notional_raw: None,
            },
        ],
        shutdown_timeout_ms: 10_000,
        scheduler: SchedulerRuntimeConfig::default(),
        strategy: StrategyRuntimeConfig::default(),
        strategies: Vec::new(),
    }
}
