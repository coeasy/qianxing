//! 进程拓扑校验：API、存储、消息与 worker 角色组合。

use super::*;
use qx_core::VenueFamily;

impl RuntimeConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != RUNTIME_SCHEMA_VERSION {
            return Err(format!(
                "运行时配置 schema_version 必须为 {RUNTIME_SCHEMA_VERSION}"
            ));
        }
        if self.environment.trim().is_empty() {
            return Err("运行时 environment 不能为空".into());
        }
        if self
            .config_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| fingerprint.trim().is_empty())
        {
            return Err("config_fingerprint 不能为空字符串".into());
        }
        self.api
            .bind
            .parse::<SocketAddr>()
            .map_err(|error| format!("API bind 不是合法 SocketAddr: {error}"))?;
        if self.api.transport == ApiTransport::Mtls && self.api.tls.is_none() {
            return Err("mTLS API 必须配置 tls 证书路径".into());
        }
        if self.api.transport == ApiTransport::Mtls && self.api.operators.is_empty() {
            return Err("mTLS API 必须至少配置一个 Operator 证书映射".into());
        }
        if self.api.transport == ApiTransport::Plaintext
            && self.environment.eq_ignore_ascii_case("production")
        {
            return Err("production 环境禁止使用明文 API".into());
        }
        if self.api.transport == ApiTransport::Plaintext && !self.api.operators.is_empty() {
            return Err("明文 API 不能声明需要 mTLS 证书的 Operator 映射".into());
        }
        for (operator_id, operator) in &self.api.operators {
            if operator_id.trim().is_empty() || operator.certificate.trim().is_empty() {
                return Err("Operator 配置必须包含 id 和 certificate".into());
            }
        }
        if let Some(tls) = &self.api.tls {
            if [
                tls.certificate_chain.as_str(),
                tls.private_key.as_str(),
                tls.client_ca.as_str(),
            ]
            .iter()
            .any(|path| path.trim().is_empty())
            {
                return Err("TLS 证书链、私钥和客户端 CA 路径不能为空".into());
            }
        }
        if self.storage.data_dir.trim().is_empty() {
            return Err("storage.data_dir 不能为空".into());
        }
        if self.profile == RuntimeProfile::SingleNode {
            if self.storage.backend == StorageBackend::Postgres {
                return Err(
                    "single_node profile 不允许 PostgreSQL；请使用 SQLite/Files，或显式切换 distributed profile".into(),
                );
            }
            if self.messaging.enabled {
                return Err(
                    "single_node profile 不允许启用 NATS messaging；请使用本地队列，或显式切换 distributed profile".into(),
                );
            }
            if self.workers.iter().any(|worker| {
                worker.enabled
                    && matches!(
                        worker.role,
                        WorkerRole::OutboxRelay | WorkerRole::EventConsumer
                    )
            }) {
                return Err(
                    "single_node profile 不允许启用 OutboxRelay/EventConsumer；请显式切换 distributed profile".into(),
                );
            }
        }
        if self.environment.eq_ignore_ascii_case("production")
            && self.storage.backend != StorageBackend::Postgres
        {
            return Err(
                "production 环境 EventLog/Outbox 必须使用 PostgreSQL transactional backend".into(),
            );
        }
        match (self.storage.backend, self.storage.consistency) {
            (StorageBackend::Files | StorageBackend::Sqlite, StorageConsistency::Transactional) => {
                return Err(
                    "Files/SQLite backend 不支持 transactional consistency；请使用 local_durable 或 distributed_outbox".into(),
                );
            }
            (StorageBackend::Postgres, StorageConsistency::LocalDurable) => {
                return Err(
                    "PostgreSQL backend 不能声明 local_durable；请使用 transactional 或 distributed_outbox".into(),
                );
            }
            _ => {}
        }
        if self.storage.consistency == StorageConsistency::DistributedOutbox
            && !self.messaging.enabled
        {
            return Err(
                "distributed_outbox consistency 必须同时启用 messaging，由 Outbox Relay 发布"
                    .into(),
            );
        }
        if self.messaging.enabled
            && self.storage.consistency != StorageConsistency::DistributedOutbox
        {
            return Err("启用 messaging 时 storage.consistency 必须为 distributed_outbox".into());
        }
        if self
            .storage
            .event_log_segment_events
            .is_some_and(|events| events == 0)
        {
            return Err("storage.event_log_segment_events 必须大于 0".into());
        }
        if self.storage.backend == StorageBackend::Sqlite
            && self
                .storage
                .sqlite_path
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
        {
            return Err("SQLite backend 必须配置 sqlite_path".into());
        }
        if self.storage.backend == StorageBackend::Postgres
            && self
                .storage
                .postgres_dsn_env
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
        {
            return Err("PostgreSQL backend 必须配置 postgres_dsn_env".into());
        }
        if self.storage.backend == StorageBackend::Postgres
            && (self.storage.postgres_pool_size == 0 || self.storage.postgres_pool_size > 128)
        {
            return Err("PostgreSQL postgres_pool_size 必须在 1..=128 内".into());
        }
        if self.messaging.enabled {
            if self.messaging.nats_url.trim().is_empty()
                || self.messaging.subject_prefix.trim().is_empty()
            {
                return Err("启用 messaging 时 nats_url 和 subject_prefix 不能为空".into());
            }
            if self.messaging.relay_interval_ms == 0 || self.messaging.relay_interval_ms > 300_000 {
                return Err("messaging.relay_interval_ms 必须在 1..=300000 内".into());
            }
            if self.messaging.relay_batch_size == 0 || self.messaging.relay_batch_size > 10_000 {
                return Err("messaging.relay_batch_size 必须在 1..=10000 内".into());
            }
            if self.messaging.lease_seconds == 0 || self.messaging.lease_seconds > 86_400 {
                return Err("messaging.lease_seconds 必须在 1..=86400 内".into());
            }
            if self.messaging.worker_stale_after_ms == 0
                || self.messaging.worker_stale_after_ms > 86_400_000
            {
                return Err("messaging.worker_stale_after_ms 必须在 1..=86400000 内".into());
            }
        }
        if self
            .workers
            .iter()
            .any(|worker| worker.enabled && worker.role == WorkerRole::OutboxRelay)
            && !self.messaging.enabled
        {
            return Err("启用 OutboxRelay worker 时必须启用 messaging".into());
        }
        let has_event_consumer = self
            .workers
            .iter()
            .any(|worker| worker.enabled && worker.role == WorkerRole::EventConsumer);
        if has_event_consumer {
            if !self.messaging.enabled {
                return Err("启用 EventConsumer worker 时必须启用 messaging".into());
            }
            for (value, field) in [
                (&self.messaging.consumer_stream, "consumer_stream"),
                (&self.messaging.consumer_name, "consumer_name"),
                (&self.messaging.consumer_group_id, "consumer_group_id"),
                (
                    &self.messaging.consumer_handler_executable,
                    "consumer_handler_executable",
                ),
            ] {
                if value
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .is_empty()
                {
                    return Err(format!("启用 EventConsumer 时 messaging.{field} 不能为空"));
                }
            }
            if self.messaging.consumer_batch_size == 0
                || self.messaging.consumer_batch_size > 10_000
            {
                return Err("messaging.consumer_batch_size 必须在 1..=10000 内".into());
            }
            if self.messaging.consumer_max_attempts == 0 {
                return Err("messaging.consumer_max_attempts 必须大于 0".into());
            }
            if self.messaging.consumer_handler_timeout_ms == 0
                || self.messaging.consumer_handler_timeout_ms > 300_000
            {
                return Err("messaging.consumer_handler_timeout_ms 必须在 1..=300000 内".into());
            }
            if self
                .messaging
                .consumer_handler_executable
                .as_deref()
                .is_some_and(|value| value.contains('\n') || value.contains('\r'))
                || self
                    .messaging
                    .consumer_handler_args
                    .iter()
                    .any(|value| value.contains('\n') || value.contains('\r'))
            {
                return Err("EventConsumer handler executable/args 不能包含换行".into());
            }
        }
        if self.shutdown_timeout_ms == 0 || self.shutdown_timeout_ms > 300_000 {
            return Err("shutdown_timeout_ms 必须在 1..=300000 内".into());
        }
        if self.scheduler.state_path.trim().is_empty()
            || self.scheduler.jobs_path.trim().is_empty()
            || self.scheduler.job_queue_path.trim().is_empty()
        {
            return Err("Scheduler state/jobs/job_queue 路径不能为空".into());
        }
        if self.scheduler.tick_interval_ms == 0 || self.scheduler.tick_interval_ms > 300_000 {
            return Err("Scheduler tick_interval_ms 必须在 1..=300000 内".into());
        }
        self.validate_strategy_config(&self.strategy, "Strategy", false)?;
        let mut strategy_ids = std::collections::BTreeSet::new();
        for strategy in &self.strategies {
            let id = strategy
                .id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "多策略配置的 id 必须非空且匹配 Strategy worker".to_string())?;
            if !strategy_ids.insert(id.to_string()) {
                return Err(format!("多策略配置 id 重复: {id}"));
            }
            if !self.workers.iter().any(|worker| {
                worker.enabled && worker.role == WorkerRole::Strategy && worker.id == id
            }) {
                return Err(format!("多策略配置 {id} 没有匹配的启用 Strategy worker"));
            }
            self.validate_strategy_config(strategy, &format!("Strategy[{id}]"), true)?;
            let instrument = strategy
                .instrument
                .as_deref()
                .and_then(InstrumentId::parse)
                .ok_or_else(|| format!("Strategy[{id}] instrument 不是合法 InstrumentId"))?;
            let worker_binding_matches = self.workers.iter().any(|worker| {
                worker.enabled
                    && worker.role == WorkerRole::Strategy
                    && worker.id == id
                    && worker.account_id.as_deref() == strategy.account_id.as_deref()
                    && worker.venue_id.as_deref() == strategy.venue_id.as_deref()
                    && (worker.symbols.is_empty()
                        || worker.symbols.iter().any(|symbol| {
                            InstrumentId::parse(symbol).as_ref() == Some(&instrument)
                        }))
            });
            if !worker_binding_matches {
                return Err(format!(
                    "Strategy[{id}] 的 account/venue/instrument 与 worker 绑定不一致"
                ));
            }
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut enabled_api_workers = 0_u8;
        for worker in &self.workers {
            if worker.id.trim().is_empty()
                || !worker
                    .id
                    .chars()
                    .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.'))
                || !ids.insert(worker.id.clone())
            {
                return Err(format!("worker id 为空或重复: {}", worker.id));
            }
            // 角色字段可见性由 `WorkerRole::field_scopes` 单点判定，且与启用开关
            // 无关：把一个角色永不读取的字段绑到该 worker 上就是配置错误。
            match worker.role_field_status() {
                RoleFieldStatus::Ok => {}
                RoleFieldStatus::Forbidden { field } => {
                    return Err(format!(
                        "{} {field} 不能配置在 {:?} 角色；该角色的运行路径不会读取它",
                        worker.id, worker.role
                    ))
                }
                RoleFieldStatus::Missing { field } => {
                    return Err(format!(
                        "{} {:?} 角色必须配置 {field}",
                        worker.id, worker.role
                    ))
                }
                RoleFieldStatus::CredentialSource => {
                    return Err(format!(
                        "{} 必须且只能配置一份有效的 credential_env 或 credential_files",
                        worker.id
                    ))
                }
                RoleFieldStatus::PaperCashOnRealVenue => {
                    return Err(format!(
                        "{} paper_initial_cash_raw 只能配置在 Paper worker",
                        worker.id
                    ))
                }
            }
            if worker
                .instrument_spec_path
                .as_deref()
                .is_some_and(|path| path.trim().is_empty())
            {
                return Err(format!("{} instrument_spec_path 不能为空字符串", worker.id));
            }
            if worker
                .paper_initial_cash_raw
                .is_some_and(|amount| amount <= 0)
            {
                return Err(format!("{} paper_initial_cash_raw 必须为正数", worker.id));
            }
            if worker
                .max_order_notional_raw
                .is_some_and(|limit| limit <= 0)
                || worker
                    .max_position_notional_raw
                    .is_some_and(|limit| limit <= 0)
            {
                return Err(format!("{} 风控名义额上限必须为正数", worker.id));
            }
            if worker
                .settlement_currency
                .as_deref()
                .is_some_and(|currency| currency.trim().is_empty())
            {
                return Err(format!("{} settlement_currency 不能为空字符串", worker.id));
            }
            for symbol in &worker.symbols {
                if InstrumentId::parse(symbol).is_none() {
                    return Err(format!(
                        "{} symbols 必须是合法 InstrumentId: {}",
                        worker.id, symbol
                    ));
                }
            }
            if !worker.enabled {
                continue;
            }
            if worker.role == WorkerRole::Api {
                enabled_api_workers = enabled_api_workers.saturating_add(1);
            }
            if matches!(
                worker.role,
                WorkerRole::Execution | WorkerRole::SpreadRecovery
            ) && worker.instrument_spec_path.is_none()
                && (self.environment.eq_ignore_ascii_case("production")
                    || VenueFamily::parse_option(worker.venue_id.as_deref())
                        != Some(VenueFamily::Paper))
            {
                return Err(format!(
                        "{} Execution/SpreadRecovery worker 必须配置 instrument_spec_path；仅非 production 的 Paper smoke 允许兼容省略",
                    worker.id
                ));
            }
            if matches!(
                worker.role,
                WorkerRole::Execution | WorkerRole::SpreadRecovery
            ) && self.environment.eq_ignore_ascii_case("production")
            {
                if worker.max_order_notional_raw.is_none() {
                    return Err(format!(
                        "{} production Execution/SpreadRecovery worker 必须配置 max_order_notional_raw",
                        worker.id
                    ));
                }
                if worker.max_position_notional_raw.is_none() {
                    return Err(format!(
                        "{} production Execution/SpreadRecovery worker 必须配置 max_position_notional_raw",
                        worker.id
                    ));
                }
            }
            // account_id / venue_id 的必需性与凭据来源的唯一性已经由
            // `role_field_status` 判定；这里只补 Binance 私有接口对凭据的强制要求。
            if worker.role.uses_private_venue()
                && VenueFamily::parse_option(worker.venue_id.as_deref())
                    == Some(VenueFamily::Binance)
                && !worker.has_valid_credential_env()
                && !worker.has_valid_credential_files()
            {
                return Err(format!(
                    "{} Binance worker 必须配置有效 credential_env 或 credential_files",
                    worker.id
                ));
            }
            if worker.role == WorkerRole::MarketData
                && VenueFamily::parse_option(worker.venue_id.as_deref())
                    == Some(VenueFamily::Binance)
                && worker.symbols.is_empty()
            {
                return Err(format!(
                    "{} Binance 行情 worker 至少需要一个 symbol",
                    worker.id
                ));
            }
            if worker.instrument_spec_path.is_none()
                && (worker.max_order_notional_raw.is_some()
                    || worker.max_position_notional_raw.is_some())
            {
                return Err(format!(
                    "{} 配置名义额上限时必须同时配置 instrument_spec_path",
                    worker.id
                ));
            }
            if worker.role == WorkerRole::Reconciler
                && VenueFamily::parse_option(worker.venue_id.as_deref())
                    == Some(VenueFamily::Binance)
            {
                for symbol in &worker.symbols {
                    let valid = InstrumentId::parse(symbol)
                        .is_some_and(|instrument| instrument.venue.is_binance());
                    if !valid {
                        return Err(format!(
                            "{} Binance 对账 symbol 必须是合法的 *.BINANCE InstrumentId: {}",
                            worker.id, symbol
                        ));
                    }
                }
            }
        }
        if enabled_api_workers != 1 {
            return Err("运行时配置必须且只能启用一个 api worker".into());
        }
        self.verify_fingerprint()?;
        Ok(())
    }
}
