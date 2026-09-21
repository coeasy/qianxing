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
struct VenueEntry {
    /// 命令入口展示名。
    name: &'static str,
    /// Venue 绑定判定。
    is_bound: fn(&WorkerConfig) -> bool,
    /// 绑定失败时的说明。
    venue_hint: &'static str,
}

impl VenueEntry {
    const CCXT: VenueEntry = VenueEntry {
        name: "CCXT",
        is_bound: |worker| {
            worker
                .venue_id
                .as_deref()
                .is_some_and(|venue| !venue.eq_ignore_ascii_case("paper"))
        },
        venue_hint: "必须绑定非 paper 的 CCXT venue",
    };
    const BINANCE: VenueEntry = VenueEntry {
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
pub(crate) fn run_binance_spread_recovery_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    let venue_id = worker.venue_id.clone().unwrap_or_else(|| "BINANCE".into());
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!("binance spread recovery scanning venue={venue_id}"),
        Some(runtime_timestamp_ms()),
    )?;
    while !context.should_stop() {
        let now = runtime_timestamp_ms();
        if has_pending_spread_recovery(&pipeline_storage.root, &venue_id)? {
            let mut pipeline = pipeline_storage
                .open(binance_event_log_name(&worker), "USDT")
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
    resolve_worker_runtime_paths(&mut worker, path);
    if once && matches!(worker.role, WorkerRole::MarketData | WorkerRole::UserStream) {
        return Err("binance-worker --once 只支持 execution 或 reconciler worker".into());
    }
    let pipeline_root = Path::new(&config.storage.data_dir).to_path_buf();
    let pipeline_storage = PipelineStorage::from_config(&config)?;
    let paper_workers = config
        .workers
        .iter()
        .filter(|worker| {
            worker.enabled
                && worker.role == WorkerRole::Execution
                && worker
                    .venue_id
                    .as_deref()
                    .is_some_and(|venue| venue.eq_ignore_ascii_case("paper"))
        })
        .cloned()
        .collect::<Vec<_>>();
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
            paper_workers,
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
            once,
        ),
        _ => Err("unsupported Binance worker role".into()),
    })?;
    handle
        .join()
        .map_err(|_| format!("worker {worker_id} panic"))?
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

pub(crate) fn ccxt_market_to_spec(
    instrument: &InstrumentId,
    market: &serde_json::Value,
) -> Result<TradingInstrumentSpec, String> {
    let market_type = market
        .get("market_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("spot");
    let product = match market_type {
        "spot" => TradingProduct::Spot,
        "margin" => TradingProduct::Margin,
        "swap" | "perpetual" => TradingProduct::Perpetual,
        "future" | "futures" => TradingProduct::Future,
        other => return Err(format!("CCXT market type 不支持: {other}")),
    };
    let base_currency = market
        .get("base")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "CCXT market 缺少 base".to_string())?;
    let quote_currency = market
        .get("quote")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "CCXT market 缺少 quote".to_string())?;
    let settlement_currency = market
        .get("settle")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(quote_currency);
    let contract_size = raw_json_i128(market, "contract_size_raw")?;
    let linear = market
        .get("linear")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(!product.is_derivative());
    let inverse = market
        .get("inverse")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let max_leverage = market
        .get("max_leverage")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(if product.is_derivative() { 100 } else { 1 });
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product,
        base_currency: base_currency.into(),
        quote_currency: quote_currency.into(),
        settlement_currency: settlement_currency.into(),
        contract_size,
        linear,
        inverse,
        // CCXT 市场快照的精度字段由不同 exchange/precisionMode 表达；
        // 若未由上游归一化，使用最小定点单位并要求部署侧覆盖。
        price_tick: raw_json_i128(market, "price_tick_raw").unwrap_or(1),
        qty_step: raw_json_i128(market, "qty_step_raw").unwrap_or(1),
        min_qty: raw_json_i128(market, "min_qty_raw").unwrap_or(1),
        max_leverage,
        maintenance_margin_bps: market
            .get("maintenance_margin_bps")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(500),
        valid_from: 0,
        valid_to: market.get("expiry_ms").and_then(serde_json::Value::as_u64),
    };
    spec.validate()
        .map_err(|error| format!("CCXT market spec 非法: {error:?}"))?;
    Ok(spec)
}

pub(crate) fn ccxt_margin_rule_from_market(market: &serde_json::Value) -> Box<dyn MarginRule> {
    let Some(rows) = market
        .get("leverage_tiers")
        .and_then(serde_json::Value::as_array)
    else {
        return Box::new(NoMargin);
    };
    let tiers = rows
        .iter()
        .filter_map(|row| {
            let max_notional = row
                .get("max_notional_raw")
                .and_then(serde_json::Value::as_i64)
                .map(i128::from)
                .or_else(|| {
                    row.get("max_notional_raw")
                        .and_then(serde_json::Value::as_u64)
                        .map(i128::from)
                })
                .unwrap_or(i128::MAX);
            let initial_bp = row
                .get("initial_margin_bps")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let maintenance_bp = row
                .get("maintenance_margin_bps")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let max_leverage = row
                .get("max_leverage")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok());
            (max_notional > 0 && initial_bp >= 0 && maintenance_bp >= 0).then_some(MarginTier {
                max_notional,
                initial_bp,
                maintenance_bp,
                max_leverage,
            })
        })
        .collect::<Vec<_>>();
    if tiers.is_empty() {
        Box::new(NoMargin)
    } else {
        Box::new(TieredMargin { tiers })
    }
}
pub(crate) fn run_ccxt_worker(
    path: &Path,
    worker_id: &str,
    ccxt_config_path: &Path,
    once: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker = venue_worker(&config, worker_id, &VenueEntry::CCXT)?;
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
    handle
        .join()
        .map_err(|_| format!("CCXT worker {worker_id} panic"))?
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
                    ccxt_event_log_name(&worker),
                    worker_settlement_currency(&worker),
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
