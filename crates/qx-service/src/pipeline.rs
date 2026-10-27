//! Runtime EventLog backend selection and pipeline opening shared by service hosts.

use qx_core::QxError;
use qx_runtime::{LiveEventPipeline, OutboxRecovery, RuntimeConfig, StorageBackend};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct PipelineStorage {
    /// Root used by the file backend and by the database-backend switch guard.
    pub root: PathBuf,
    segment_events: Option<usize>,
    postgres_dsn: Option<String>,
    sqlite_db: Option<PathBuf>,
    #[cfg(feature = "postgres")]
    postgres_pool_size: usize,
}

impl PipelineStorage {
    pub fn from_config(config: &RuntimeConfig) -> Result<Self, String> {
        Ok(Self {
            root: Path::new(&config.storage.data_dir).to_path_buf(),
            segment_events: config.storage.event_log_segment_events,
            postgres_dsn: configured_postgres_dsn(config)?,
            sqlite_db: configured_sqlite_event_log(config)?,
            #[cfg(feature = "postgres")]
            postgres_pool_size: config.storage.postgres_pool_size,
        })
    }

    pub fn open(
        &self,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> Result<LiveEventPipeline, String> {
        self.open_with(log_name, currency, OutboxRecovery::ReprojectOnOpen)
    }

    /// Opens the configured backend without replaying facts into the write-side Outbox.
    pub fn open_read_only(
        &self,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> Result<LiveEventPipeline, String> {
        self.open_with(log_name, currency, OutboxRecovery::ReadOnly)
    }

    pub fn open_with(
        &self,
        log_name: impl Into<String>,
        currency: impl Into<String>,
        recovery: OutboxRecovery,
    ) -> Result<LiveEventPipeline, String> {
        let log_name = log_name.into();
        if self.postgres_dsn.is_some() || self.sqlite_db.is_some() {
            LiveEventPipeline::assert_no_abandoned_file_log(self.root.clone(), log_name.clone())
                .map_err(|error| qx_context("EventLog 后端切换被拒绝", &error))?;
        }
        if let Some(dsn) = self.postgres_dsn.as_deref() {
            #[cfg(not(feature = "postgres"))]
            {
                let _ = dsn;
                return Err(
                    "当前服务宿主未启用 postgres feature，无法打开 PostgreSQL EventLog".into(),
                );
            }
            #[cfg(feature = "postgres")]
            {
                let opened = match recovery {
                    OutboxRecovery::ReprojectOnOpen => {
                        LiveEventPipeline::open_postgres_with_pool_size(
                            dsn,
                            self.postgres_pool_size,
                            log_name,
                            currency,
                        )
                    }
                    OutboxRecovery::ReadOnly => LiveEventPipeline::open_postgres_read_only(
                        dsn,
                        self.postgres_pool_size,
                        log_name,
                        currency,
                    ),
                };
                return opened.map_err(|error| qx_context("打开 PostgreSQL EventLog 失败", &error));
            }
        }
        if let Some(db) = self.sqlite_db.as_deref() {
            #[cfg(not(feature = "sqlite"))]
            {
                let _ = db;
                return Err("当前服务宿主未启用 sqlite feature，无法打开 SQLite EventLog".into());
            }
            #[cfg(feature = "sqlite")]
            {
                let opened = match recovery {
                    OutboxRecovery::ReprojectOnOpen => {
                        LiveEventPipeline::open_sqlite(db, log_name, currency)
                    }
                    OutboxRecovery::ReadOnly => {
                        LiveEventPipeline::open_sqlite_read_only(db, log_name, currency)
                    }
                };
                return opened.map_err(|error| qx_context("打开 SQLite EventLog 失败", &error));
            }
        }
        let opened = match recovery {
            OutboxRecovery::ReprojectOnOpen => LiveEventPipeline::open_configured(
                self.root.clone(),
                log_name,
                currency,
                self.segment_events,
            ),
            OutboxRecovery::ReadOnly => LiveEventPipeline::open_read_only(
                self.root.clone(),
                log_name,
                currency,
                self.segment_events,
            ),
        };
        opened.map_err(|error| qx_context("打开运行时 EventLog 失败", &error))
    }
}

/// Open a runtime pipeline from the requested data root and explicit currency.
pub fn open_runtime_pipeline(
    config: &RuntimeConfig,
    root: &Path,
    log_name: impl Into<String>,
    currency: impl Into<String>,
    recovery: OutboxRecovery,
) -> Result<LiveEventPipeline, String> {
    let mut storage = PipelineStorage::from_config(config)?;
    storage.root = root.to_path_buf();
    storage.open_with(log_name, currency, recovery)
}

/// Open an account-scoped pipeline using the canonical configured settlement currency.
pub fn open_account_pipeline(
    config: &RuntimeConfig,
    root: &Path,
    log_name: &str,
    recovery: OutboxRecovery,
) -> Result<LiveEventPipeline, String> {
    let currency = qx_runtime::settlement_currency_for_log(config, log_name)?;
    open_runtime_pipeline(config, root, log_name, currency, recovery)
}

/// Check whether one EventLog exists in the configured backend without opening a writable pipe.
pub fn event_log_exists(
    config: &RuntimeConfig,
    root: &Path,
    log_name: &str,
) -> Result<bool, String> {
    if config.storage.backend == StorageBackend::Postgres {
        #[cfg(not(feature = "postgres"))]
        return Err("当前服务宿主未启用 postgres feature；无法查询 PostgreSQL EventLog".into());
        #[cfg(feature = "postgres")]
        {
            let dsn = postgres_dsn(config)?;
            let store = qx_storage::PostgresEventLogStore::connect_with_pool_size(
                &dsn,
                config.storage.postgres_pool_size,
            )
            .map_err(|error| format!("连接 PostgreSQL EventLog 失败: {error:?}"))?;
            return store
                .read_if_exists(log_name)
                .map(|value| value.is_some())
                .map_err(|error| format!("查询 PostgreSQL EventLog 失败: {error:?}"));
        }
    }
    if config.storage.backend == StorageBackend::Sqlite {
        #[cfg(not(feature = "sqlite"))]
        return Err("当前服务宿主未启用 sqlite feature；无法查询 SQLite EventLog".into());
        #[cfg(feature = "sqlite")]
        {
            let path = config
                .storage
                .sqlite_path
                .as_deref()
                .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
            let store = qx_storage::SqliteEventLogStore::new(path)
                .map_err(|error| format!("连接 SQLite EventLog 失败: {error:?}"))?;
            return store
                .read_if_exists(log_name)
                .map(|value| value.is_some())
                .map_err(|error| format!("查询 SQLite EventLog 失败: {error:?}"));
        }
    }
    Ok(root.join(format!("{log_name}.json")).exists()
        || (config.storage.event_log_segment_events.is_some()
            && root.join(format!("{log_name}.manifest.json")).exists()))
}

fn configured_sqlite_event_log(config: &RuntimeConfig) -> Result<Option<PathBuf>, String> {
    if config.storage.backend != StorageBackend::Sqlite {
        return Ok(None);
    }
    let Some(path) = config.storage.sqlite_path.as_deref() else {
        return Err("SQLite backend 缺少 sqlite_path".to_string());
    };
    Ok(Some(PathBuf::from(path)))
}

fn configured_postgres_dsn(config: &RuntimeConfig) -> Result<Option<String>, String> {
    if config.storage.backend != StorageBackend::Postgres {
        return Ok(None);
    }
    #[cfg(not(feature = "postgres"))]
    {
        let _ = config;
        Err("当前服务宿主未启用 postgres feature，无法初始化 PostgreSQL EventLog".into())
    }
    #[cfg(feature = "postgres")]
    {
        postgres_dsn(config).map(Some)
    }
}

#[cfg(feature = "postgres")]
fn postgres_dsn(config: &RuntimeConfig) -> Result<String, String> {
    let env_name = config
        .storage
        .postgres_dsn_env
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "PostgreSQL backend 缺少 postgres_dsn_env".to_string())?;
    std::env::var(env_name).map_err(|error| {
        format!(
            "PostgreSQL DSN 环境变量 {} 不可用；凭证不能写入运行时配置: {}",
            env_name, error
        )
    })
}

pub fn qx_context(context: &str, error: &QxError) -> String {
    let contract = error.contract();
    let hint = if contract.reconcile_required {
        "（结果未知，需先对账）"
    } else if contract.retryability.allows_retry() {
        "（可重试）"
    } else if contract.retryability.may_retry_eventually() {
        "（需退避后重试）"
    } else {
        ""
    };
    format!(
        "[{}] {context}: {}{hint}",
        contract.code, contract.user_message
    )
}

#[cfg(test)]
mod tests {
    use super::{event_log_exists, open_account_pipeline, PipelineStorage};
    use qx_core::{InstrumentId, Price, Quantity, VenueId};
    use qx_guanxing::QuoteTick;
    use qx_runtime::{OutboxRecovery, RuntimeConfig, RuntimeEventEnvelope};
    use std::path::{Path, PathBuf};

    const FILES_CONFIG: &str =
        include_str!("../../../deploy/qianxing.runtime.strategy-backtest.example.json");

    fn temp_root(label: &str) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .join("target")
            .join(format!(
                "qx-service-pipeline-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock after epoch")
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).expect("create pipeline fixture root");
        root
    }

    fn config(root: &Path) -> RuntimeConfig {
        let mut config = RuntimeConfig::from_json(FILES_CONFIG).expect("valid file backend config");
        config.storage.data_dir = root.to_string_lossy().into_owned();
        config
    }

    fn append_quote(pipeline: &mut qx_runtime::LiveEventPipeline) {
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                InstrumentId::new("BTCUSDT", VenueId::new("BINANCE")),
                QuoteTick::new(
                    10,
                    Price::from_i64(99),
                    Quantity::from_i64(2),
                    Price::from_i64(100),
                    Quantity::from_i64(3),
                    1,
                ),
                11,
                1,
                "service-pipeline-test",
            ))
            .expect("append quote fixture");
    }

    #[test]
    fn configured_file_pipeline_can_be_reopened_as_a_read_only_projection() {
        let root = temp_root("files");
        let config = config(&root);
        let storage = PipelineStorage::from_config(&config).unwrap();
        let mut writer = storage.open("service-pipeline", "USDT").unwrap();
        append_quote(&mut writer);
        drop(writer);
        assert!(event_log_exists(&config, &root, "service-pipeline").unwrap());
        assert!(!event_log_exists(&config, &root, "missing-service-pipeline").unwrap());

        let reader = storage
            .open_read_only("service-pipeline", "USDT")
            .expect("read model uses the configured file backend");
        assert_eq!(reader.log().events().len(), 1);
        drop(reader);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn account_pipeline_uses_the_account_registry_settlement_currency() {
        let root = temp_root("account-currency");
        let mut config = config(&root);
        config.accounts.push(qx_runtime::AccountRuntimeConfig {
            id: "main".into(),
            settlement_currency: Some("USDT".into()),
            initial_cash_raw: None,
            portfolio_id: None,
        });
        let log_name = qx_runtime::account_event_log_name("main", "paper").unwrap();
        let pipeline = open_account_pipeline(&config, &root, &log_name, OutboxRecovery::ReadOnly)
            .expect("account pipeline resolves registry currency and read-only recovery");
        assert_eq!(pipeline.settlement_currency(), "USDT");
        drop(pipeline);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn configured_sqlite_pipeline_uses_the_service_feature_forwarding() {
        let root = temp_root("sqlite");
        let mut config = config(&root);
        config.storage.backend = qx_runtime::StorageBackend::Sqlite;
        config.storage.sqlite_path =
            Some(root.join("runtime.sqlite3").to_string_lossy().into_owned());
        let storage = PipelineStorage::from_config(&config).unwrap();
        let mut writer = storage.open("service-sqlite", "USDT").unwrap();
        append_quote(&mut writer);
        drop(writer);
        assert!(event_log_exists(&config, &root, "service-sqlite").unwrap());
        assert!(!event_log_exists(&config, &root, "missing-service-sqlite").unwrap());
        let reader = storage
            .open_read_only("service-sqlite", "USDT")
            .expect("read model uses the configured SQLite backend");
        assert_eq!(reader.log().events().len(), 1);
        drop(reader);
        std::fs::remove_dir_all(root).unwrap();
    }
}
