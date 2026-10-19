//! 配置化 API 服务的具体装配实现。

use super::provider_registry;
use crate::{
    account_event_log_name, collect_doctor_report_for_config, configured_account_event_logs,
    configured_api_readiness, configured_command_queue, configured_control_store,
    default_account_event_log, event_log_exists, lease_clock, load_api_account_snapshots,
    load_api_query_models, open_runtime_pipeline, order_from_submit_command, read_worker_metrics,
    reject_ashare_rules_on_submit_path, runtime_timestamp_ms, worker_metrics_dir,
};
use qx_api::{ApiPolicy, ApiService, ApiState, ControlSubmitError};
use qx_control::CommandKind;
use qx_runtime::{OutboxRecovery, RuntimeConfig, RuntimeSupervisor, StorageBackend};
use qx_storage::FileTokenBucket;
#[cfg(feature = "sqlite")]
use qx_storage::SqliteTokenBucket;
use std::path::Path;
use std::sync::Arc;

pub(super) fn build_configured_api_service_inner(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
) -> Result<ApiService, String> {
    // API 会受理 Trading 权限的 SubmitOrder 并写进与 worker 同一条队列：A 股段配在这里
    // 同样没有落点（订单形状在提交那一刻就已定死），所以排在建目录等副作用之前拒掉（V12 R1 §4.20）。
    reject_ashare_rules_on_submit_path(runtime_config_path, None, "serve")?;
    std::fs::create_dir_all(&config.storage.data_dir)
        .map_err(|error| format!("创建运行时 data_dir 失败: {error}"))?;
    let control_root = Path::new(&config.storage.data_dir).to_path_buf();
    let control_store = configured_control_store(config)?;
    let control = control_store.load()?;
    let command_queue = configured_command_queue(config, &control_root)?;
    let mut state = ApiState::default();
    state.control = control;
    let models = load_api_query_models(config)?;
    state.job_runs = models.job_runs;
    state.ledger_entries = models.ledger_entries;
    state.reconcile_reports = models
        .reconcile_reports
        .into_iter()
        .map(|report| (report.worker_id.clone(), report))
        .collect();
    for (account_id, venue_id, log_name, currency) in configured_account_event_logs(config)? {
        let root = Path::new(&config.storage.data_dir);
        if event_log_exists(config, root, &log_name)? {
            let pipeline =
                open_runtime_pipeline(config, root, log_name, currency, OutboxRecovery::ReadOnly)
                    .map_err(|error| format!("读取 API 事件投影失败: {error}"))?;
            state
                .project_account_event_log(&account_id, &venue_id, pipeline.log())
                .map_err(|error| format!("初始化 API 账户事件投影失败: {error}"))?;
        }
    }
    // 只有默认账户能占用无键端点读到的那格全局兼容快照；其余账户按身份进各自的投影，
    // 带 account_id/venue_id 的查询照旧读得到（V11 R12，与上面的 Ledger 共用同一个判据）。
    let default_log = default_account_event_log(config, Path::new(&config.storage.data_dir))?;
    for snapshot in load_api_account_snapshots(config)? {
        let identity =
            account_event_log_name(&snapshot.header.account_id, &snapshot.header.venue_id);
        let loaded = if identity.is_some() && identity == default_log {
            state.publish_snapshot(snapshot)
        } else {
            state.publish_snapshot_for(
                snapshot.header.account_id.clone(),
                snapshot.header.venue_id.clone(),
                snapshot,
            )
        };
        loaded.map_err(|error| format!("装载 API 账户查询快照失败: {error}"))?;
    }
    let mut policy = ApiPolicy::new();
    for (operator_id, operator) in &config.api.operators {
        policy = policy.grant(operator_id.clone(), operator.permission);
    }
    let metrics_dir = worker_metrics_dir(config);
    let worker_metrics_stale_after_ms = config.messaging.worker_stale_after_ms;
    let readiness_store = control_store.clone();
    let readiness_config = config.clone();
    let readiness_static_report = collect_doctor_report_for_config(
        runtime_config_path,
        config,
        RuntimeSupervisor::new(config.clone()).map(|_| ()),
    )?;
    // 三个只读运维端点的现读出口：与 boot 用同一份配置、同一个读点，差别只在"每次请求都读"。
    let query_models_config = config.clone();
    let algo_execution_config = config.clone();
    let readiness_runtime_config_path = runtime_config_path.to_path_buf();
    let readiness_metrics_dir = metrics_dir.clone();
    let service = if config.api.operators.is_empty() {
        ApiService::new(state)
    } else {
        ApiService::with_policy(state, policy)
    }
    .with_worker_metrics_provider(move || {
        read_worker_metrics(
            &metrics_dir,
            runtime_timestamp_ms(),
            worker_metrics_stale_after_ms,
        )
    })
    .with_readiness_provider(move || {
        configured_api_readiness(
            &readiness_config,
            &readiness_runtime_config_path,
            &readiness_static_report,
            &readiness_store,
            &readiness_metrics_dir,
            runtime_timestamp_ms(),
            worker_metrics_stale_after_ms,
        )
    })
    .with_query_models_provider(move || load_api_query_models(&query_models_config))
    .with_algo_execution_provider(move |operation, body| {
        crate::api_algo_execution::handle_algo_execution_request(
            &algo_execution_config,
            operation,
            body,
        )
    })
    .with_control_plane_provider({
        let store = control_store.clone();
        move || store.load()
    })
    .with_control_submitter({
        let store = control_store.clone();
        move |command, granted, ts| {
            if matches!(&command.kind, CommandKind::SubmitOrder) {
                order_from_submit_command(&command).map_err(|error| {
                    ControlSubmitError::Rejected(qx_control::ControlError::Invalid(format!(
                        "SubmitOrder 载荷非法: {error:?}"
                    )))
                })?;
            }
            let (plane, result) = store
                .transact(|plane| {
                    plane
                        .submit_as(command, granted, ts)
                        .map_err(ControlSubmitError::Rejected)
                })
                .map_err(ControlSubmitError::Unavailable)?;
            result.map(|audit| (plane, audit))
        }
    })
    .with_command_enqueuer({
        let queue = Arc::clone(&command_queue);
        move |command, ts| {
            queue
                .enqueue_command(command, lease_clock(ts))
                .map(|_| ())
                .map_err(|error| format!("写入控制命令队列失败: {error:?}"))
        }
    });
    let service = match provider_registry::from_runtime_config(config, runtime_config_path)? {
        Some(registry) => service.with_provider_registry(registry),
        None => service,
    };
    // 浏览器准入与并发连接预算都由配置驱动，且都是"配错了 `serve` 就起不来"的硬错误，
    // 不是跳过坏的那一条继续跑：一份被静默削减的源 allowlist 在生产上的表现是某个前端
    // 跨源失败，而配置侧看不出是哪一条被丢了（口径与理由写在 `ApiRuntimeConfig` 那两格）。
    let service = if config.api.cors_allowed_origins.is_empty() {
        service
    } else {
        service.with_cors_allowed_origins(config.api.cors_allowed_origins.clone())?
    };
    let service = match config.api.max_concurrent_connections {
        Some(limit) => service.with_max_concurrent_connections(limit)?,
        None => service,
    };
    if config.storage.backend == StorageBackend::Sqlite {
        #[cfg(not(feature = "sqlite"))]
        return Err("当前 qx-cli 未启用 sqlite feature；请使用 cargo run -p qx-cli --features sqlite -- serve ...".into());
        #[cfg(feature = "sqlite")]
        {
            let path = config
                .storage
                .sqlite_path
                .as_deref()
                .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
            let bucket = SqliteTokenBucket::new(
                path,
                "api",
                qx_api::DEFAULT_RATE_LIMIT_CAPACITY,
                qx_api::DEFAULT_RATE_LIMIT_REFILL_PER_SECOND,
            )
            .map_err(|error| format!("初始化 SQLite API 限流失败: {error:?}"))?;
            let service = service.with_sqlite_rate_limit(bucket);
            return Ok(service);
        }
    }
    // 文件后端部署也要跨进程共享限流：此前只有 SQLite 部署拿到共享桶，同一
    // data_dir 下并起的两个 API 进程各算各的额度，而调用方看到同一份配置
    // （V12 §16）。`FileTokenBucket` 的原子锁+临时文件替换语义与 worker 侧同源。
    let bucket = FileTokenBucket::new(
        control_root.join("api-rate-limit"),
        "api",
        qx_api::DEFAULT_RATE_LIMIT_CAPACITY,
        qx_api::DEFAULT_RATE_LIMIT_REFILL_PER_SECOND,
    )
    .map_err(|error| format!("初始化文件 API 限流失败: {error:?}"))?;
    Ok(service.with_shared_file_rate_limit(bucket))
}
