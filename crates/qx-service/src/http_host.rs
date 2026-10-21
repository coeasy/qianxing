//! HTTP API and same-origin console service hosts.

use crate::api_host::spawn_api_projection_bridge;
use qx_api::{ApiPolicy, ApiService, ApiState, ConsoleConfig, ConsoleFront};
use qx_runtime::{ApiTransport, RuntimeConfig, RuntimeSupervisor, StorageBackend, WorkerRole};
use qx_storage::FileTokenBucket;
use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Request the API projection bridge to stop and join it exactly once.
pub fn stop_api_projection_bridge(stop: &Arc<AtomicBool>, thread: &mut Option<JoinHandle<()>>) {
    stop.store(true, Ordering::Release);
    if let Some(thread) = thread.take() {
        let _ = thread.join();
    }
}

/// Apply runtime-owned browser, connection, and cross-process rate-limit policies to an API host.
///
/// The facade supplies the already resolved control root; this function performs no runtime-file
/// reads, so boot uses the same immutable config snapshot as the rest of API assembly.
pub fn configure_api_host_policies(
    mut service: ApiService,
    config: &RuntimeConfig,
    control_root: &Path,
) -> Result<ApiService, String> {
    if !config.api.cors_allowed_origins.is_empty() {
        service = service.with_cors_allowed_origins(config.api.cors_allowed_origins.clone())?;
    }
    if let Some(limit) = config.api.max_concurrent_connections {
        service = service.with_max_concurrent_connections(limit)?;
    }
    if config.storage.backend == StorageBackend::Sqlite {
        #[cfg(not(feature = "sqlite"))]
        return Err("SQLite API rate limiting requires the qx-service sqlite feature".into());
        #[cfg(feature = "sqlite")]
        {
            let path = config
                .storage
                .sqlite_path
                .as_deref()
                .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
            let bucket = qx_storage::SqliteTokenBucket::new(
                path,
                "api",
                qx_api::DEFAULT_RATE_LIMIT_CAPACITY,
                qx_api::DEFAULT_RATE_LIMIT_REFILL_PER_SECOND,
            )
            .map_err(|error| format!("初始化 SQLite API 限流失败: {error:?}"))?;
            return Ok(service.with_sqlite_rate_limit(bucket));
        }
    }

    // File-backed deployments also need a shared bucket so concurrent API processes use the
    // same quota. FileTokenBucket provides the atomic lock and replacement semantics.
    let bucket = FileTokenBucket::new(
        control_root.join("api-rate-limit"),
        "api",
        qx_api::DEFAULT_RATE_LIMIT_CAPACITY,
        qx_api::DEFAULT_RATE_LIMIT_REFILL_PER_SECOND,
    )
    .map_err(|error| format!("初始化文件 API 限流失败: {error:?}"))?;
    Ok(service.with_shared_file_rate_limit(bucket))
}

/// Build the operator permission policy from the runtime snapshot.
pub fn api_policy_from_runtime(config: &RuntimeConfig) -> ApiPolicy {
    let mut policy = ApiPolicy::new();
    for (operator_id, operator) in &config.api.operators {
        policy = policy.grant(operator_id.clone(), operator.permission);
    }
    policy
}

/// Create an API service with the operator policy owned by the same runtime snapshot.
pub fn api_service_from_runtime(state: ApiState, config: &RuntimeConfig) -> ApiService {
    if config.api.operators.is_empty() {
        ApiService::new(state)
    } else {
        ApiService::with_policy(state, api_policy_from_runtime(config))
    }
}

/// Resolve the unique enabled API worker from the validated runtime topology.
pub fn configured_api_worker_id(config: &RuntimeConfig) -> Result<String, String> {
    let mut workers = config
        .workers
        .iter()
        .filter(|worker| worker.enabled && worker.role == WorkerRole::Api);
    let worker = workers
        .next()
        .ok_or_else(|| "运行时配置必须且只能启用一个 api worker".to_string())?;
    if workers.next().is_some() {
        return Err("运行时配置必须且只能启用一个 api worker".to_string());
    }
    Ok(worker.id.clone())
}

/// Host the same-origin console as a supervised API worker.
///
/// The CLI resolves files, credentials and user-facing output. This service owns the listener,
/// ConsoleFront, projection bridge and worker shutdown lifecycle for the immutable config.
pub fn serve_console(
    config: RuntimeConfig,
    service: ApiService,
    static_dir: PathBuf,
    bootstrap_token: String,
    runtime_timestamp_ms: fn() -> u64,
    on_bound: impl FnOnce(&str, &str, &Path, u64, &str),
) -> Result<(), String> {
    let console = config.api.console.as_ref().ok_or_else(|| {
        "运行时配置没有 api.console 段：控制台面不跟着 serve 一起开，要开就显式配上".to_string()
    })?;
    qx_api::console_bind_is_loopback(&console.bind)?;
    let ttl_seconds = console
        .session_ttl_seconds
        .unwrap_or(qx_api::DEFAULT_CONSOLE_SESSION_TTL_SECONDS);
    let operator = console.operator.clone();
    let bind = console.bind.clone();
    let supervisor = RuntimeSupervisor::new(config.clone())?;
    let api_worker_id = configured_api_worker_id(&config)?;
    let front = ConsoleFront::new(
        service.clone(),
        ConsoleConfig::new(
            static_dir.clone(),
            operator.clone(),
            bootstrap_token.clone(),
            ttl_seconds,
        )?,
    )?;
    let listener =
        TcpListener::bind(&bind).map_err(|error| format!("绑定控制台地址失败 {bind}: {error}"))?;
    on_bound(&bind, &operator, &static_dir, ttl_seconds, &bootstrap_token);

    let projection_stop = Arc::new(AtomicBool::new(false));
    let mut projection_thread =
        spawn_api_projection_bridge(&config, service, Arc::clone(&projection_stop));
    let worker = match supervisor.spawn_worker(&api_worker_id, move |context| {
        context.heartbeat(runtime_timestamp_ms())?;
        front
            .serve(listener, runtime_timestamp_ms, || context.should_stop())
            .map_err(|error| format!("控制台服务停止: {error}"))
    }) {
        Ok(worker) => worker,
        Err(error) => {
            stop_api_projection_bridge(&projection_stop, &mut projection_thread);
            return Err(error);
        }
    };
    let result =
        qx_runtime::host::join_worker_handle(&supervisor, worker, "Console", &api_worker_id);
    stop_api_projection_bridge(&projection_stop, &mut projection_thread);
    result
}

/// Host the HTTP API as the configured API worker, including TLS reload and query projection.
pub fn serve_api(
    config: RuntimeConfig,
    service: ApiService,
    runtime_timestamp_ms: fn() -> u64,
    on_bound: impl FnOnce(&str, ApiTransport),
) -> Result<(), String> {
    let supervisor = RuntimeSupervisor::new(config.clone())?;
    let api_worker_id = configured_api_worker_id(supervisor.config())?;
    let bind = config.api.bind.clone();
    let transport = config.api.transport;
    let listener =
        TcpListener::bind(&bind).map_err(|error| format!("绑定 API 地址失败 {bind}: {error}"))?;
    on_bound(&bind, transport);

    let result = match transport {
        ApiTransport::Plaintext => {
            let projection_stop = Arc::new(AtomicBool::new(false));
            let mut projection_thread =
                spawn_api_projection_bridge(&config, service.clone(), Arc::clone(&projection_stop));
            let worker = match supervisor.spawn_worker(&api_worker_id, move |context| {
                context.heartbeat(runtime_timestamp_ms())?;
                service
                    .serve(listener, runtime_timestamp_ms, || context.should_stop())
                    .map_err(|error| format!("API 服务停止: {error}"))
            }) {
                Ok(worker) => worker,
                Err(error) => {
                    stop_api_projection_bridge(&projection_stop, &mut projection_thread);
                    return Err(error);
                }
            };
            let result =
                qx_runtime::host::join_worker_handle(&supervisor, worker, "API", &api_worker_id);
            stop_api_projection_bridge(&projection_stop, &mut projection_thread);
            result
        }
        ApiTransport::Mtls => {
            let tls = config
                .api
                .tls
                .as_ref()
                .ok_or_else(|| "mTLS API 缺少 tls 配置".to_string())?;
            let server_config = qx_api::load_mtls_server_config_from_pem(
                &tls.certificate_chain,
                &tls.private_key,
                &tls.client_ca,
            )?;
            let operator_paths = config
                .api
                .operators
                .iter()
                .map(|(operator_id, operator)| {
                    (operator_id.clone(), PathBuf::from(&operator.certificate))
                })
                .collect::<BTreeMap<_, _>>();
            let identity_reloader = qx_api::MtlsIdentityPemReloader::new(operator_paths)?;
            let identity_store = qx_api::MtlsIdentityStore::new(identity_reloader.load()?);
            let reloader = qx_api::TlsPemReloader::new(
                tls.certificate_chain.clone(),
                tls.private_key.clone(),
                tls.client_ca.clone(),
            );
            let store = qx_api::TlsConfigStore::new(server_config);
            let projection_stop = Arc::new(AtomicBool::new(false));
            let mut projection_thread =
                spawn_api_projection_bridge(&config, service.clone(), Arc::clone(&projection_stop));
            let reload_stop = Arc::new(AtomicBool::new(false));
            let reload_stop_thread = Arc::clone(&reload_stop);
            let reload_store = store.clone();
            let reload_identity_store = identity_store.clone();
            let reload_thread = thread::spawn(move || {
                while !reload_stop_thread.load(Ordering::Acquire) {
                    if let Err(error) = reloader.reload_if_changed(&reload_store) {
                        eprintln!("[运行时 · TLS] 证书轮询重载失败，保留当前配置: {error}");
                    }
                    if let Err(error) = identity_reloader.reload_if_changed(&reload_identity_store)
                    {
                        eprintln!(
                            "[运行时 · TLS] Operator 证书轮询重载失败，保留当前映射: {error}"
                        );
                    }
                    thread::sleep(Duration::from_secs(1));
                }
            });
            let worker = match supervisor.spawn_worker(&api_worker_id, move |context| {
                context.heartbeat(runtime_timestamp_ms())?;
                service
                    .serve_tls_mtls_with_stores(
                        listener,
                        &store,
                        &identity_store,
                        runtime_timestamp_ms,
                        || context.should_stop(),
                    )
                    .map_err(|error| format!("mTLS API 服务停止: {error}"))
            }) {
                Ok(worker) => worker,
                Err(error) => {
                    reload_stop.store(true, Ordering::Release);
                    let _ = reload_thread.join();
                    stop_api_projection_bridge(&projection_stop, &mut projection_thread);
                    return Err(error);
                }
            };
            let result = qx_runtime::host::join_worker_handle(
                &supervisor,
                worker,
                "mTLS API",
                &api_worker_id,
            );
            reload_stop.store(true, Ordering::Release);
            let _ = reload_thread.join();
            stop_api_projection_bridge(&projection_stop, &mut projection_thread);
            result
        }
    };
    result
}
