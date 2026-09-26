//! worker 角色分派表与进程监督器。
//!
//! `ccxt-worker` 与 `binance-worker` 两条入口在此共用同一张 `WorkerRole` 分派表，
//! 新增角色只需要在本处登记一次，避免出现两套角色语义。

use super::*;

/// 角色的稳定名称，用于错误信息与运维输出。
const fn worker_role_label(role: WorkerRole) -> &'static str {
    match role {
        WorkerRole::Api => "api",
        WorkerRole::MarketData => "market-data",
        WorkerRole::UserStream => "user-stream",
        WorkerRole::Execution => "execution",
        WorkerRole::SpreadRecovery => "spread-recovery",
        WorkerRole::Scheduler => "scheduler",
        WorkerRole::Reconciler => "reconciler",
        WorkerRole::Strategy => "strategy",
        WorkerRole::OutboxRelay => "outbox-relay",
        WorkerRole::EventConsumer => "event-consumer",
    }
}

/// 一个 `*-worker` 命令入口的登记表。
pub(crate) struct VenueEntry {
    /// 命令入口展示名。
    name: &'static str,
    /// Venue 绑定判定。
    is_bound: fn(&WorkerConfig) -> bool,
    /// 绑定失败时的说明。
    venue_hint: &'static str,
}

impl VenueEntry {
    pub(crate) const CCXT: VenueEntry = VenueEntry {
        name: "CCXT",
        is_bound: |worker| {
            worker
                .venue_id
                .as_deref()
                .is_some_and(|venue| !venue.eq_ignore_ascii_case("paper"))
        },
        venue_hint: "必须绑定非 paper 的 CCXT venue",
    };
    pub(crate) const BINANCE: VenueEntry = VenueEntry {
        name: "Binance",
        is_bound: |worker| {
            worker
                .venue_id
                .as_deref()
                .is_some_and(|venue| venue.to_ascii_lowercase().contains("binance"))
        },
        venue_hint: "必须绑定 Binance Venue",
    };
}

/// 按登记表加载并校验 worker：存在性 → 启用 → 角色白名单 → Venue 绑定。
fn venue_worker(
    config: &RuntimeConfig,
    worker_id: &str,
    entry: &VenueEntry,
) -> Result<WorkerConfig, String> {
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    if !worker.enabled {
        return Err(format!("worker {worker_id} 未启用"));
    }
    if !worker.role.is_venue_role() {
        let roles = qx_runtime::ALL_WORKER_ROLES
            .iter()
            .filter(|role| role.is_venue_role())
            .map(|role| worker_role_label(*role))
            .collect::<Vec<_>>()
            .join("/");
        return Err(format!(
            "worker {worker_id} 不是 {} 支持的 {roles} 角色",
            entry.name
        ));
    }
    if !(entry.is_bound)(&worker) {
        return Err(format!("worker {worker_id} {}", entry.venue_hint));
    }
    Ok(worker)
}

/// `reconcile` 省略 worker-id 时该起哪一个 reconciler：按 **role + Venue 绑定**解析
/// （V11 S12，与 S1 同一个判据）。字面量 `reconciler-main` 在两个方向上都会撒谎——
/// 名字合法改动的拓扑上报"找不到 worker: reconciler-main"，把"没解析"说成"不存在"；
/// 而恰好有个叫这名字、角色却是 execution 的 worker 时会真的跑一次下单执行。
/// 零个或多个候选都必须报错并让人点名，不能挑第一个。
pub(crate) fn configured_reconcile_worker_id(
    config: &RuntimeConfig,
    entry: &VenueEntry,
) -> Result<String, String> {
    let candidates = config
        .workers
        .iter()
        .filter(|worker| {
            worker.enabled && worker.role == WorkerRole::Reconciler && (entry.is_bound)(worker)
        })
        .map(|worker| worker.id.clone())
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(format!(
            "运行时配置没有可解析的 reconciler worker（要求：启用且{}），请用 \
             reconcile <runtime.json> <worker-id> 显式点名",
            entry.venue_hint
        )),
        many => Err(format!(
            "运行时配置有 {} 个候选 reconciler worker（{}），请用 \
             reconcile <runtime.json> <worker-id> 显式点名",
            many.len(),
            many.join(", ")
        )),
    }
}

/// `reconcile` 入口：点名时与 `binance-worker` 走完全相同的四段校验，省略时按上表解析。
pub(crate) fn run_binance_reconcile_once(
    path: &Path,
    worker_id: Option<&str>,
) -> Result<(), String> {
    let worker_id = match worker_id {
        Some(worker_id) => worker_id.to_string(),
        None => configured_reconcile_worker_id(&read_runtime_config(path)?, &VenueEntry::BINANCE)?,
    };
    run_binance_worker(path, &worker_id, true)
}

pub(crate) fn run_binance_spread_recovery_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    let venue_id = worker.venue_id.clone().unwrap_or_else(|| "BINANCE".into());
    let settlement_currency = account_worker_currency_from_path(&runtime_config_path, &worker)?;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!("binance spread recovery scanning venue={venue_id}"),
        Some(runtime_timestamp_ms()),
    )?;
    while !context.should_stop() {
        let now = runtime_timestamp_ms();
        if has_pending_spread_recovery(&pipeline_storage.root, &venue_id)? {
            let mut pipeline = pipeline_storage
                .open(
                    binance_event_log_name(&worker)?,
                    settlement_currency.clone(),
                )
                .map_err(|error| format!("打开 Binance 多腿恢复 EventLog 失败: {error}"))?;
            let auth = load_binance_worker_auth(&worker)?;
            let mut venue = new_binance_venue(&worker, auth)?;
            venue
                .restore_orders(pipeline.orders())
                .map_err(|error| format!("恢复 Binance 多腿订单状态失败: {error:?}"))?;
            let mut source_seq = pipeline
                .log()
                .events()
                .last()
                .map(|event| event.source_seq)
                .unwrap_or(0);
            let validator =
                recovery_order_validator(&worker, &pipeline, Some(&runtime_config_path));
            let (_, diagnostics) = recover_spread_groups_for_venue(
                SpreadRecoveryContext {
                    root: &pipeline_storage.root,
                    venue_id: &venue_id,
                    accept_any_venue: false,
                    order_validator: Some(&validator),
                    pipeline: &mut pipeline,
                    worker_id: &worker.id,
                    now,
                    source_seq: &mut source_seq,
                },
                venue,
            )?;
            for message in diagnostics {
                eprintln!("[HedgeRecovery] {message}");
            }
        }
        context.heartbeat(now)?;
        if once {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

pub(crate) fn run_binance_worker(path: &Path, worker_id: &str, once: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let mut worker = venue_worker(&config, worker_id, &VenueEntry::BINANCE)?;
    // 提交路径不认 A 股段：配了就当场拒，而不是收下配置再按无交易制度下单（V11 Q65）。
    reject_ashare_rules_on_submit_path(path, Some(&worker), "binance-worker")?;
    resolve_worker_runtime_paths(&mut worker, path);
    if once && matches!(worker.role, WorkerRole::MarketData | WorkerRole::UserStream) {
        return Err("binance-worker --once 只支持 execution 或 reconciler worker".into());
    }
    let pipeline_root = Path::new(&config.storage.data_dir).to_path_buf();
    let pipeline_storage = PipelineStorage::from_config(&config)?;
    // Paper 行情桥要按账户身份判定记账币种，所以这里带全量 worker；桥自身再按角色过滤。
    let all_workers = config.workers.clone();
    let runtime_config_path = path.to_path_buf();
    let control_store = configured_control_store(&config)?;
    let command_queue = configured_command_queue(&config, &pipeline_root)?;
    let dedicated_spread_recovery = dedicated_spread_recovery_configured(&config, &worker);
    let supervisor = RuntimeSupervisor::new(config)?;
    let worker_role = worker.role;
    let registered_id = worker.id.clone();
    let worker_for_run = worker.clone();
    let handle = supervisor.spawn_worker(&registered_id, move |context| match worker_role {
        WorkerRole::MarketData => run_binance_market_worker(
            context,
            worker_for_run.clone(),
            pipeline_storage.clone(),
            all_workers,
        ),
        WorkerRole::UserStream => run_binance_user_worker(
            context,
            worker_for_run.clone(),
            pipeline_storage.clone(),
            runtime_config_path,
        ),
        WorkerRole::Execution => run_binance_execution_worker(
            context,
            worker_for_run,
            pipeline_storage.clone(),
            control_store,
            command_queue,
            runtime_config_path,
            dedicated_spread_recovery,
            once,
        ),
        WorkerRole::SpreadRecovery => run_binance_spread_recovery_worker(
            context,
            worker_for_run,
            pipeline_storage,
            runtime_config_path,
            once,
        ),
        WorkerRole::Reconciler => run_binance_reconcile_worker(
            context,
            worker_for_run,
            &pipeline_root,
            pipeline_storage,
            &runtime_config_path,
            once,
        ),
        _ => Err("unsupported Binance worker role".into()),
    })?;
    join_worker_handle(&supervisor, handle, "Binance", worker_id)
}

#[cfg(test)]
pub(crate) fn managed_worker_args(
    config: &RuntimeConfig,
    config_path: &Path,
    allow_unmanaged_roles: bool,
) -> Result<Vec<(String, Vec<String>)>, String> {
    Ok(
        qx_orchestrator::plan_workers(config, config_path, allow_unmanaged_roles)?
            .into_iter()
            .map(|launch| (launch.worker_id, launch.args))
            .collect(),
    )
}

/// 跨平台进程托管入口。它只做拓扑校验、日志隔离和 fail-fast 生命周期管理，
/// 不替代 worker 的租约、幂等和 EventLog 恢复语义；任一子进程异常退出时会
/// 停止其余进程，避免 API/策略仍运行而执行器已经消失。
pub(crate) fn run_process_supervisor(
    path: &Path,
    allow_unmanaged_roles: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let executable =
        std::env::current_exe().map_err(|error| format!("解析 qx-cli 可执行文件失败: {error}"))?;
    let work_dir = std::env::current_dir().map_err(|error| format!("读取工作目录失败: {error}"))?;
    supervise_workers(&config, path, &executable, &work_dir, allow_unmanaged_roles)
}

pub(crate) fn run_ccxt_worker(
    path: &Path,
    worker_id: &str,
    ccxt_config_path: &Path,
    once: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker = venue_worker(&config, worker_id, &VenueEntry::CCXT)?;
    reject_ashare_rules_on_submit_path(path, Some(&worker), "ccxt-worker")?;
    let dedicated_spread_recovery = dedicated_spread_recovery_configured(&config, &worker);
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let pipeline_storage = PipelineStorage::from_config(&config)?;
    let runtime_config_path = path.to_path_buf();
    let control_store = configured_control_store(&config)?;
    let queue = configured_command_queue(&config, &root)?;
    let ccxt_config_path = resolve_ccxt_config_path(path, &ccxt_config_path.to_string_lossy());
    validate_ccxt_worker_binding(&worker, Path::new(&ccxt_config_path))?;
    let supervisor = RuntimeSupervisor::new(config)?;
    let registered_id = worker.id.clone();
    let worker_role = worker.role;
    let handle = supervisor.spawn_worker(&registered_id, move |context| match worker_role {
        WorkerRole::MarketData => run_ccxt_market_worker(
            context,
            worker,
            pipeline_storage.clone(),
            ccxt_config_path,
            runtime_config_path.clone(),
            once,
        ),
        WorkerRole::UserStream => run_ccxt_user_stream_worker(
            context,
            worker,
            pipeline_storage.clone(),
            ccxt_config_path,
            runtime_config_path.clone(),
            once,
        ),
        WorkerRole::Reconciler => run_ccxt_reconcile_worker(
            context,
            worker,
            &root,
            pipeline_storage.clone(),
            ccxt_config_path,
            runtime_config_path.clone(),
            once,
        ),
        WorkerRole::SpreadRecovery => run_ccxt_spread_recovery_worker(
            context,
            worker,
            pipeline_storage,
            ccxt_config_path,
            runtime_config_path,
            once,
        ),
        WorkerRole::Execution => run_ccxt_execution_worker(
            context,
            worker,
            pipeline_storage,
            control_store,
            queue,
            ccxt_config_path,
            runtime_config_path,
            dedicated_spread_recovery,
            once,
        ),
        _ => Err("unsupported CCXT worker role".into()),
    })?;
    join_worker_handle(&supervisor, handle, "CCXT", worker_id)
}

pub(crate) fn run_ccxt_spread_recovery_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    ccxt_config_path: String,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    let python = python_interpreter();
    let venue_id = worker.venue_id.clone().unwrap_or_else(|| "ccxt".into());
    let settlement_currency = account_worker_currency_from_path(&runtime_config_path, &worker)?;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!("ccxt spread recovery scanning venue={venue_id}"),
        Some(runtime_timestamp_ms()),
    )?;
    while !context.should_stop() {
        let now = runtime_timestamp_ms();
        if has_pending_spread_recovery(&pipeline_storage.root, &venue_id)? {
            let mut pipeline = pipeline_storage
                .open(
                    required_account_event_log(&worker)?,
                    settlement_currency.clone(),
                )
                .map_err(|error| format!("打开 CCXT 多腿恢复 EventLog 失败: {error}"))?;
            let client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
                .map_err(|error| format!("启动公共 CCXT 恢复 Worker 失败: {error}"))?;
            let venue = CcxtProcessVenue::new(venue_id.clone(), Box::new(client));
            let mut source_seq = pipeline
                .log()
                .events()
                .last()
                .map(|event| event.source_seq)
                .unwrap_or(0);
            let validator =
                recovery_order_validator(&worker, &pipeline, Some(&runtime_config_path));
            let (_, diagnostics) = recover_spread_groups_for_venue(
                SpreadRecoveryContext {
                    root: &pipeline_storage.root,
                    venue_id: &venue_id,
                    accept_any_venue: false,
                    order_validator: Some(&validator),
                    pipeline: &mut pipeline,
                    worker_id: &worker.id,
                    now,
                    source_seq: &mut source_seq,
                },
                venue,
            )?;
            for message in diagnostics {
                eprintln!("[HedgeRecovery] {message}");
            }
        }
        context.heartbeat(now)?;
        if once {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// 下载公共 CCXT OHLCV 快照，输出为 qianxing_bridge.BarFrame JSON，作为回测
/// 的不可变输入；回测运行期间不再访问交易所。
pub(crate) fn run_ccxt_fetch_ohlcv(
    ccxt_config_path: &Path,
    instrument: &str,
    timeframe: &str,
    start_ms: u64,
    end_ms: u64,
    output_path: &Path,
) -> Result<(), String> {
    let python = python_interpreter();
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path.to_string_lossy(), None)?;
    let result = client
        .call(serde_json::json!({
            "op": "fetch_ohlcv",
            "instrument": instrument,
            "timeframe": timeframe,
            "start_ms": start_ms,
            "end_ms": end_ms,
        }))
        .map_err(|error| format!("CCXT OHLCV 查询失败: {error}"))?;
    let frame = result
        .get("frame")
        .ok_or_else(|| "CCXT OHLCV 响应缺少 frame".to_string())?;
    std::fs::write(
        output_path,
        serde_json::to_string_pretty(frame)
            .map_err(|error| format!("编码 OHLCV 快照失败: {error}"))?,
    )
    .map_err(|error| format!("写入 OHLCV 快照失败 {}: {error}", output_path.display()))?;
    println!(
        "[CCXT · OHLCV] instrument={} timeframe={} output={} ✓",
        instrument,
        timeframe,
        output_path.display()
    );
    Ok(())
}

pub(crate) fn run_ccxt_market_spec(
    ccxt_config_path: &Path,
    instrument: &str,
    output_path: &Path,
) -> Result<(), String> {
    let python = python_interpreter();
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path.to_string_lossy(), None)?;
    let result = client
        .call(serde_json::json!({
            "op": "resolve_market",
            "instrument": instrument,
        }))
        .map_err(|error| format!("CCXT market spec 查询失败: {error}"))?;
    let mut market = result
        .get("market")
        .cloned()
        .ok_or_else(|| "CCXT market spec 响应缺少 market".to_string())?;
    let tiers = match client.call(serde_json::json!({
        "op": "fetch_leverage_tiers",
        "instrument": instrument,
    })) {
        Ok(value) => value
            .get("tiers")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
        Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {
            serde_json::json!([])
        }
        Err(error) => return Err(format!("CCXT leverage tiers 查询失败: {error}")),
    };
    if let Some(object) = market.as_object_mut() {
        object.insert("leverage_tiers".into(), tiers);
    }
    std::fs::write(
        output_path,
        serde_json::to_string_pretty(&market)
            .map_err(|error| format!("编码 market spec 失败: {error}"))?,
    )
    .map_err(|error| format!("写入 market spec 失败 {}: {error}", output_path.display()))?;
    println!(
        "[CCXT · MarketSpec] instrument={} output={} ✓",
        instrument,
        output_path.display()
    );
    Ok(())
}
