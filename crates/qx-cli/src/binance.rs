//! Binance 连接器：market/user/execution/reconcile worker、探针与 SubmitOrder 入口。

use super::*;

fn is_binance_testnet(worker: &WorkerConfig) -> bool {
    worker
        .venue_id
        .as_deref()
        .map(|venue| venue.to_ascii_lowercase().contains("testnet"))
        .unwrap_or(false)
        || worker
            .endpoint
            .as_deref()
            .map(|endpoint| endpoint.to_ascii_lowercase().contains("testnet"))
            .unwrap_or(false)
}

fn load_binance_worker_auth(worker: &WorkerConfig) -> Result<BinanceSpotAuth, String> {
    if let Some(files) = worker.credential_files.as_ref() {
        return BinanceSpotCredentials::from_files(&files.api_key, &files.secret)?.into_auth();
    }
    let env = worker.credential_env.as_ref().ok_or_else(|| {
        format!(
            "worker {} 缺少 credential_env 或 credential_files",
            worker.id
        )
    })?;
    BinanceSpotCredentials::from_env(&env.api_key, &env.secret)?.into_auth()
}

fn new_binance_venue(
    worker: &WorkerConfig,
    auth: BinanceSpotAuth,
) -> Result<BinanceSpotVenue, String> {
    let transport: Arc<dyn HttpTransport> =
        Arc::new(TlsHttpTransport::new(Duration::from_secs(10))?);
    Ok(if is_binance_testnet(worker) {
        BinanceSpotVenue::testnet(worker.id.clone(), auth, transport)
    } else {
        BinanceSpotVenue::new(worker.id.clone(), auth, transport)
    })
}

fn configured_ws_endpoint(worker: &WorkerConfig) -> Result<(String, u16), String> {
    let endpoint = worker
        .endpoint
        .as_deref()
        .ok_or_else(|| format!("worker {} 缺少 WebSocket endpoint", worker.id))?;
    let remainder = endpoint
        .strip_prefix("wss://")
        .or_else(|| endpoint.strip_prefix("ws://"))
        .ok_or_else(|| format!("worker {} endpoint 必须使用 ws:// 或 wss://", worker.id))?;
    let authority = remainder
        .split('/')
        .next()
        .filter(|authority| !authority.is_empty())
        .ok_or_else(|| format!("worker {} endpoint 缺少主机", worker.id))?;
    let (host, port) = authority
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()))
        .map(|(host, port)| (host, port.parse::<u16>().unwrap_or_default()))
        .unwrap_or((authority, 443));
    if host.is_empty()
        || host.contains(':')
        || host.contains('\r')
        || host.contains('\n')
        || port == 0
    {
        return Err(format!("worker {} endpoint 主机非法", worker.id));
    }
    Ok((host.to_string(), port))
}

pub(crate) fn run_binance_public_probe(testnet: bool, instrument: &str) -> Result<(), String> {
    let instrument = InstrumentId::parse(instrument)
        .ok_or_else(|| format!("Binance public probe instrument 非法: {instrument}"))?;
    if !instrument.venue.as_str().eq_ignore_ascii_case("BINANCE") {
        return Err("Binance public probe 只接受 *.BINANCE InstrumentId".into());
    }
    let transport: Arc<dyn HttpTransport> =
        Arc::new(TlsHttpTransport::new(Duration::from_secs(10))?);
    let mut market = if testnet {
        BinanceSpotMarketData::testnet(transport)
    } else {
        BinanceSpotMarketData::new(transport)
    };
    let quote = market
        .fetch_book_ticker(&instrument, runtime_timestamp_ms())
        .map_err(|error| format!("Binance public bookTicker 失败: {error:?}"))?;
    println!(
        "[Binance · PublicProbe] network={} instrument={} bid_raw={} ask_raw={} bid_qty_raw={} ask_qty_raw={} ts={}",
        if testnet { "testnet" } else { "mainnet" },
        instrument,
        quote.bid.raw(),
        quote.ask.raw(),
        quote.bid_qty.raw(),
        quote.ask_qty.raw(),
        quote.ts
    );
    Ok(())
}

pub(crate) fn run_binance_private_probe(
    runtime_path: &Path,
    worker_id: &str,
) -> Result<(), String> {
    let config = read_runtime_config(runtime_path)?;
    let mut worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 Binance private probe worker: {worker_id}"))?;
    if !worker.enabled
        || !matches!(
            worker.role,
            WorkerRole::UserStream | WorkerRole::Execution | WorkerRole::Reconciler
        )
    {
        return Err(format!("worker {worker_id} 不是启用的 Binance 私有 worker"));
    }
    if !worker
        .venue_id
        .as_deref()
        .is_some_and(|venue| venue.to_ascii_lowercase().contains("binance"))
    {
        return Err(format!("worker {worker_id} 不是 Binance worker"));
    }
    resolve_worker_runtime_paths(&mut worker, runtime_path);
    let auth = load_binance_worker_auth(&worker)?;
    let transport: Arc<dyn HttpTransport> =
        Arc::new(TlsHttpTransport::new(Duration::from_secs(10))?);
    let mut venue = if is_binance_testnet(&worker) {
        BinanceSpotVenue::testnet(worker.id.clone(), auth, transport)
    } else {
        BinanceSpotVenue::new(worker.id.clone(), auth, transport)
    };
    let balances = venue
        .fetch_account_balances(runtime_timestamp_ms())
        .map_err(|error| format!("Binance private account probe 失败: {error:?}"))?;
    println!(
        "[Binance · PrivateProbe] network={} worker={} balances={}",
        if is_binance_testnet(&worker) {
            "testnet"
        } else {
            "mainnet"
        },
        worker.id,
        balances.len()
    );
    Ok(())
}

fn binance_event_log_name(worker: &WorkerConfig) -> String {
    match worker.role {
        WorkerRole::MarketData => format!("{}-events", worker.id),
        WorkerRole::UserStream
        | WorkerRole::Execution
        | WorkerRole::SpreadRecovery
        | WorkerRole::Reconciler => format!(
            "binance-{}-{}-events",
            worker.account_id.as_deref().unwrap_or("unknown"),
            worker.venue_id.as_deref().unwrap_or("unknown")
        ),
        _ => format!("{}-events", worker.id),
    }
}

fn run_binance_market_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    paper_workers: Vec<WorkerConfig>,
) -> Result<(), String> {
    if worker.symbols.len() != 1 {
        return Err(format!(
            "当前 Binance market worker 要求恰好一个 symbol，实际 {}",
            worker.symbols.len()
        ));
    }
    let instrument = InstrumentId::parse(&worker.symbols[0])
        .ok_or_else(|| format!("worker {} instrument 非法", worker.id))?;
    let (host, port) = configured_ws_endpoint(&worker)?;
    let mut pipeline = pipeline_storage
        .open(binance_event_log_name(&worker), "USDT")
        .map_err(|error| format!("创建行情事件管线失败: {error}"))?;
    let mut paper_bridges = open_paper_market_bridges(&pipeline_storage, &paper_workers)?;
    let mut stream = BinanceSpotMarketStream::connect_with_endpoint(
        instrument.clone(),
        host,
        port,
        Duration::from_secs(10),
    )?;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        "market stream connected",
        Some(runtime_timestamp_ms()),
    )?;
    let mut quotes = 0_u64;
    while !context.should_stop() {
        match stream.recv_quote(runtime_timestamp_ms())? {
            Some(quote) => {
                let received_ts = runtime_timestamp_ms();
                pipeline
                    .ingest(RuntimeEventEnvelope::market_quote(
                        instrument.clone(),
                        quote,
                        received_ts,
                        quote.source_seq,
                        format!("{}:quote:{}", worker.id, quote.source_seq),
                    ))
                    .map_err(|error| format!("行情事实归约失败: {error:?}"))?;
                bridge_market_quote_to_paper(
                    &mut paper_bridges,
                    PaperMarketQuote {
                        source_worker_id: &worker.id,
                        instrument: &instrument,
                        bid: quote.bid,
                        bid_qty: quote.bid_qty,
                        ask: quote.ask,
                        ask_qty: quote.ask_qty,
                        event_ts: quote.ts,
                        receive_ts: received_ts,
                        source_seq: quote.source_seq,
                    },
                )?;
                quotes = quotes.saturating_add(1);
                context.heartbeat(received_ts)?;
            }
            None => break,
        }
    }
    let _ = stream.close();
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        format!("market stream stopped quotes={quotes}"),
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}

fn run_binance_user_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
) -> Result<(), String> {
    let policy =
        BinanceStreamRetryPolicy::new(10, Duration::from_secs(1), Duration::from_secs(30))?;
    let initial_auth = load_binance_worker_auth(&worker)?;
    let mut venue = new_binance_venue(&worker, initial_auth)?;
    let mut pipeline = pipeline_storage
        .open(binance_event_log_name(&worker), "USDT")
        .map_err(|error| format!("创建用户流事件管线失败: {error}"))?;
    venue
        .restore_orders(pipeline.orders())
        .map_err(|error| format!("恢复用户流订单状态失败: {error:?}"))?;
    let stop_context = context.clone();
    let sleep_context = context.clone();
    let event_context = context.clone();
    let request_prefix = worker.id.clone();
    let worker_id = worker.id.clone();
    let (host, port) = configured_ws_endpoint(&worker)?;
    let mut source_seq = 0_u64;
    let mut should_stop = move || stop_context.should_stop();
    let mut sleep = move |delay| {
        thread::sleep(delay);
        let _ = sleep_context.heartbeat(runtime_timestamp_ms());
    };
    let mut on_event = move |payload: &str| -> Result<(), String> {
        pipeline
            .refresh()
            .map_err(|error| format!("刷新共享订单 EventLog 失败: {error:?}"))?;
        venue
            .refresh_orders(pipeline.orders())
            .map_err(|error| format!("刷新用户流订单状态失败: {error:?}"))?;
        let events = match venue.ingest_user_event(payload) {
            Ok(events) => events,
            Err(error) => {
                // listenKey 过期后必须关闭当前会话并重新订阅；重连后的
                // reconciler 会先用 REST 快照确认状态，再允许新的提交。
                if payload.contains("listenKeyExpired") {
                    venue.reconnect();
                }
                return Err(format!("Binance 用户事件处理失败: {error:?}"));
            }
        };
        let received_ts = runtime_timestamp_ms();
        let event_count = ingest_venue_events(
            &mut pipeline,
            events,
            &worker_id,
            received_ts,
            &mut source_seq,
        )?;
        event_context.heartbeat(received_ts)?;
        if event_count != 0 {
            event_context.mark(
                qx_runtime::ServiceStatus::Ready,
                format!("user events={event_count}"),
                Some(runtime_timestamp_ms()),
            )?;
        }
        Ok(())
    };
    let run_config = BinanceUserStreamRunConfig::with_port(
        host,
        port,
        request_prefix,
        Duration::from_secs(10),
        policy,
    )?;
    let report = qx_adapter::run_binance_user_stream_with_config_loader(
        || load_binance_worker_auth(&worker),
        run_config,
        &mut should_stop,
        &mut sleep,
        &mut on_event,
    )?;
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        format!("user stream stopped events={}", report.events),
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}

fn run_binance_reconcile_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_root: &Path,
    pipeline_storage: PipelineStorage,
    once: bool,
) -> Result<(), String> {
    let reconcile_symbols = worker
        .symbols
        .iter()
        .map(|symbol| {
            InstrumentId::parse(symbol)
                .filter(|instrument| instrument.venue.as_str().eq_ignore_ascii_case("BINANCE"))
                .map(|instrument| instrument.symbol)
                .ok_or_else(|| format!("worker {} 对账 symbol 非法: {symbol}", worker.id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut pipeline = pipeline_storage
        .open(binance_event_log_name(&worker), "USDT")
        .map_err(|error| format!("创建对账事件管线失败: {error}"))?;
    let mut source_seq = 0_u64;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        "reconciler starting",
        Some(runtime_timestamp_ms()),
    )?;
    while !context.should_stop() {
        // 每一轮重新读取凭据并创建连接器，使 Secret Manager 对投影文件的
        // 原子替换在下一轮对账生效；本轮请求仍使用同一认证上下文。
        let auth = load_binance_worker_auth(&worker)?;
        let mut venue = new_binance_venue(&worker, auth)?;
        venue
            .set_reconcile_symbols(reconcile_symbols.iter())
            .map_err(|error| format!("设置 Binance 对账 symbol 失败: {error:?}"))?;
        // fetch_and_reconcile 只允许在 Snapshotting 阶段执行；每一轮先显式
        // 进入对账态，避免把 Live 状态下的快照查询误当成已完成对账。
        pipeline
            .refresh()
            .map_err(|error| format!("刷新共享对账 EventLog 失败: {error:?}"))?;
        venue
            .restore_orders(pipeline.orders())
            .map_err(|error| format!("刷新对账订单状态失败: {error:?}"))?;
        venue.reconnect();
        let issues = venue
            .fetch_and_reconcile()
            .map_err(|error| format!("Binance 订单对账失败: {error:?}"))?;
        let balances = venue
            .fetch_account_balances(runtime_timestamp_ms())
            .map_err(|error| format!("Binance 账户余额同步失败: {error:?}"))?;
        let received_ts = runtime_timestamp_ms();
        let account_id = worker
            .account_id
            .clone()
            .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))?;
        let venue_id = worker
            .venue_id
            .clone()
            .ok_or_else(|| format!("worker {} 缺少 venue_id", worker.id))?;
        let balance_facts = balances
            .iter()
            .map(|balance| AccountBalance {
                asset: balance.asset.clone(),
                free: balance.free,
                locked: balance.locked,
                borrowed: Money::ZERO,
            })
            .collect::<Vec<_>>();
        let balance_discrepancies = pipeline
            .settlement_balance_discrepancies(&account_id, &venue_id, &balance_facts)
            .map_err(|error| format!("账户余额对账失败: {error:?}"))?;
        persist_reconcile_report(ReconcileReportInput {
            pipeline_root,
            worker_id: &worker.id,
            account_id: &account_id,
            venue_id: &venue_id,
            observed_ts: received_ts,
            issues: &issues,
            additional_order_issues: &[],
            balances_count: balances.len(),
            balance_discrepancies: &balance_discrepancies,
            position_snapshots_count: 0,
            funding_rate_snapshots_count: 0,
            cashflow_count: 0,
        })?;
        source_seq = source_seq.saturating_add(1);
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountBalanceSnapshot {
                    account_id: account_id.clone(),
                    venue_id: venue_id.clone(),
                    balances: balance_facts,
                },
                received_ts,
                received_ts,
                source_seq,
                format!("{}:balances:{}", worker.id, source_seq),
            ))
            .map_err(|error| format!("账户余额事实归约失败: {error:?}"))?;
        for issue in &issues {
            let client_order_id = match issue {
                AdapterReconcileIssue::MissingLocally { client_order_id }
                | AdapterReconcileIssue::MissingAtVenue { client_order_id }
                | AdapterReconcileIssue::StatusMismatch {
                    client_order_id, ..
                }
                | AdapterReconcileIssue::FilledMismatch {
                    client_order_id, ..
                } => *client_order_id,
            };
            source_seq = source_seq.saturating_add(1);
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::ReconcileRequired { client_order_id },
                    received_ts,
                    received_ts,
                    source_seq,
                    format!("{}:reconcile:{}", worker.id, client_order_id),
                ))
                .map_err(|error| format!("对账事实归约失败: {error:?}"))?;
        }
        context.heartbeat(received_ts)?;
        let balance_detail = balance_discrepancies
            .iter()
            .map(|discrepancy| {
                format!(
                    "{}:{}->{}",
                    discrepancy.asset, discrepancy.ledger_raw, discrepancy.venue_raw
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let status = if issues.is_empty() && balance_discrepancies.is_empty() {
            qx_runtime::ServiceStatus::Ready
        } else {
            qx_runtime::ServiceStatus::Degraded
        };
        context.mark(
            status,
            format!(
                "reconcile_issues={} balances={} balance_discrepancies={}{}",
                issues.len(),
                balances.len(),
                balance_discrepancies.len(),
                if balance_detail.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", balance_detail)
                }
            ),
            Some(runtime_timestamp_ms()),
        )?;
        if once {
            break;
        }
        for _ in 0..300 {
            if context.should_stop() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}

/// 执行一条经过 ControlPlane 审计的 Binance SubmitOrder 命令。
///
/// 该入口是显式副作用命令：配置、命令权限、订单账户/交易所和 EventLog
/// 都先校验；订单事实先写入 `OrderSubmitted`，Venue 返回的 Accepted/Fill
/// 再按同一管线归约。网络返回未知时，EventLog 会保留 Submitted 状态，
/// 后续只能通过对账恢复，绝不自动重试补单。
fn validate_binance_submit_worker(worker: &WorkerConfig) -> Result<&str, String> {
    if !worker.enabled
        || !matches!(
            worker.role,
            WorkerRole::UserStream | WorkerRole::Execution | WorkerRole::Reconciler
        )
    {
        return Err(format!(
            "worker {} 必须是已启用的 Binance 用户流、执行或对账角色",
            worker.id
        ));
    }
    if worker
        .venue_id
        .as_deref()
        .map(|venue| venue.to_ascii_lowercase().contains("binance"))
        != Some(true)
    {
        return Err(format!("worker {} 不是 Binance Venue", worker.id));
    }
    worker
        .account_id
        .as_deref()
        .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))
}

fn binance_submit_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
    let expected_account = worker.account_id.as_deref().unwrap_or_default();
    order_from_submit_command(command)
        .map(|order| {
            order.account_id == expected_account
                && order
                    .instrument
                    .venue
                    .as_str()
                    .eq_ignore_ascii_case("BINANCE")
        })
        .unwrap_or(false)
}

fn execute_binance_submit_effect(
    command: &ControlCommand,
    worker: &WorkerConfig,
    pipeline: &mut LiveEventPipeline,
    venue: &mut BinanceSpotVenue,
    now: u64,
    source_seq: &mut u64,
    runtime_config_path: Option<&Path>,
) -> Result<String, String> {
    let requested_order = order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    let expected_account = validate_binance_submit_worker(worker)?;
    if requested_order.account_id != expected_account
        || !requested_order
            .instrument
            .venue
            .as_str()
            .eq_ignore_ascii_case("BINANCE")
    {
        return Err("订单 account_id 或 instrument venue 与 worker 拓扑不一致".into());
    }

    venue
        .restore_orders(pipeline.orders())
        .map_err(|error| format!("装载待提交订单失败: {error:?}"))?;
    execute_submit_order_with_worker_risk(
        command,
        worker,
        venue,
        pipeline,
        now,
        source_seq,
        runtime_config_path,
    )
}

pub(crate) fn run_binance_submit_order(
    path: &Path,
    worker_id: &str,
    command_path: &Path,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let mut worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    resolve_worker_runtime_paths(&mut worker, path);
    validate_binance_submit_worker(&worker)?;
    let command: ControlCommand = serde_json::from_str(
        &std::fs::read_to_string(command_path)
            .map_err(|error| format!("读取 SubmitOrder 命令失败: {error}"))?,
    )
    .map_err(|error| format!("SubmitOrder 命令 JSON 无效: {error}"))?;
    order_from_submit_command(&command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    let pipeline_storage = PipelineStorage::from_config(&config)?;
    let now = runtime_timestamp_ms();
    let store = configured_control_store(&config)?;
    let (_, accepted_result) = store
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, now))
        .map_err(|error| format!("持久化 SubmitOrder Accepted 失败: {error:?}"))?;
    let accepted = accepted_result
        .map_err(|error| format!("SubmitOrder 未通过控制面权限/幂等校验: {error:?}"))?;

    let action = if command.dry_run {
        Ok("DRY_RUN_VALIDATED".into())
    } else {
        let mut pipeline = pipeline_storage
            .open(binance_event_log_name(&worker), "USDT")
            .map_err(|error| format!("打开执行 EventLog 失败: {error}"))?;
        let auth = load_binance_worker_auth(&worker);
        match auth.and_then(|auth| new_binance_venue(&worker, auth)) {
            Ok(mut venue) => {
                let mut source_seq = 0_u64;
                let validator = recovery_order_validator(&worker, &pipeline, Some(path));
                let result = execute_binance_submit_effect(
                    &command,
                    &worker,
                    &mut pipeline,
                    &mut venue,
                    now,
                    &mut source_seq,
                    Some(path),
                );
                let latest_pipeline = pipeline_storage
                    .open(binance_event_log_name(&worker), "USDT")
                    .map_err(|error| format!("刷新 Binance 多腿订单组 EventLog 失败: {error}"))?;
                sync_spread_group_after_order(&pipeline_storage.root, &latest_pipeline, &command)?;
                let (venue, recovery) = recover_spread_groups_for_venue(
                    SpreadRecoveryContext {
                        root: &pipeline_storage.root,
                        venue_id: worker.venue_id.as_deref().unwrap_or("BINANCE"),
                        accept_any_venue: false,
                        order_validator: Some(&validator),
                        pipeline: &mut pipeline,
                        worker_id: &worker.id,
                        now,
                        source_seq: &mut source_seq,
                    },
                    venue,
                )?;
                for message in recovery {
                    eprintln!("[HedgeRecovery] {message}");
                }
                drop(venue);
                result
            }
            Err(error) => Err(error),
        }
    };
    let (_, record_result) = store
        .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
        .map_err(|error| format!("持久化 SubmitOrder 终态失败: {error:?}"))?;
    let record = record_result.map_err(|error| format!("执行控制命令失败: {error:?}"))?;
    println!(
        "[执行 · SubmitOrder] accepted={:?} final={:?} command_id={} result={}",
        accepted.status, record.status, command.command_id, record.result_code
    );
    if record.status == qx_control::CommandStatus::Failed {
        return Err(record.result_code);
    }
    Ok(())
}

/// 运行持久化控制命令队列的 Binance 执行 worker。
#[allow(clippy::too_many_arguments)]
fn run_binance_execution_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    control_store: ControlStateBackend,
    queue: Arc<dyn ControlCommandQueueBackend>,
    runtime_config_path: PathBuf,
    dedicated_spread_recovery: bool,
    once: bool,
) -> Result<(), String> {
    let owner = worker.id.clone();
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        "execution queue polling",
        Some(runtime_timestamp_ms()),
    )?;
    while !context.should_stop() {
        let now = runtime_timestamp_ms();
        let control = control_store.load()?;
        let venue_id = worker.venue_id.as_deref().unwrap_or("BINANCE");
        if !dedicated_spread_recovery
            && has_pending_spread_recovery(&pipeline_storage.root, venue_id)?
        {
            let mut recovery_pipeline = pipeline_storage
                .open(binance_event_log_name(&worker), "USDT")
                .map_err(|error| format!("打开 Binance 多腿恢复 EventLog 失败: {error}"))?;
            let auth = load_binance_worker_auth(&worker)?;
            let mut recovery_venue = new_binance_venue(&worker, auth)?;
            recovery_venue
                .restore_orders(recovery_pipeline.orders())
                .map_err(|error| format!("恢复 Binance 多腿订单状态失败: {error:?}"))?;
            let mut recovery_seq = recovery_pipeline
                .log()
                .events()
                .last()
                .map(|event| event.source_seq)
                .unwrap_or(0);
            let validator =
                recovery_order_validator(&worker, &recovery_pipeline, Some(&runtime_config_path));
            let (_, diagnostics) = recover_spread_groups_for_venue(
                SpreadRecoveryContext {
                    root: &pipeline_storage.root,
                    venue_id,
                    accept_any_venue: false,
                    order_validator: Some(&validator),
                    pipeline: &mut recovery_pipeline,
                    worker_id: &worker.id,
                    now,
                    source_seq: &mut recovery_seq,
                },
                recovery_venue,
            )?;
            for message in diagnostics {
                eprintln!("[HedgeRecovery] {message}");
            }
        }
        for command in control.pending().filter(|command| {
            matches!(&command.kind, CommandKind::SubmitOrder)
                && binance_submit_matches_worker(command, &worker)
        }) {
            queue
                .enqueue_command(command.clone(), now)
                .map_err(|error| format!("补入 SubmitOrder 队列失败: {error:?}"))?;
        }
        for queued in queue
            .available_commands(now)
            .map_err(|error| format!("读取 SubmitOrder 队列失败: {error:?}"))?
        {
            if context.should_stop() {
                break;
            }
            let command = queued.command.clone();
            if !binance_submit_matches_worker(&command, &worker) {
                continue;
            }
            let lease = match queue.claim_command(command.command_id, &owner, now, 30) {
                Ok(lease) => lease,
                Err(qx_storage::StorageError::LeaseHeld { .. }) => continue,
                Err(error) => return Err(format!("领取 SubmitOrder 租约失败: {error:?}")),
            };
            if command_is_final(&control, command.command_id) {
                queue
                    .ack_command_at(command.command_id, &owner, lease.fencing_token, now)
                    .map_err(|error| format!("清理已终态 SubmitOrder 失败: {error:?}"))?;
                continue;
            }
            let action = if command.dry_run {
                Ok("DRY_RUN_VALIDATED".into())
            } else {
                let mut pipeline = pipeline_storage
                    .open(binance_event_log_name(&worker), "USDT")
                    .map_err(|error| format!("打开执行 EventLog 失败: {error}"))?;
                let auth = load_binance_worker_auth(&worker)?;
                let mut venue = new_binance_venue(&worker, auth)?;
                venue
                    .restore_orders(pipeline.orders())
                    .map_err(|error| format!("恢复执行订单状态失败: {error:?}"))?;
                let mut source_seq = 0_u64;
                let validator =
                    recovery_order_validator(&worker, &pipeline, Some(&runtime_config_path));
                let result = execute_binance_submit_effect(
                    &command,
                    &worker,
                    &mut pipeline,
                    &mut venue,
                    now,
                    &mut source_seq,
                    Some(&runtime_config_path),
                );
                let latest_pipeline = pipeline_storage
                    .open(binance_event_log_name(&worker), "USDT")
                    .map_err(|error| format!("刷新 Binance 多腿订单组 EventLog 失败: {error}"))?;
                sync_spread_group_after_order(&pipeline_storage.root, &latest_pipeline, &command)?;
                let (venue, recovery) = recover_spread_groups_for_venue(
                    SpreadRecoveryContext {
                        root: &pipeline_storage.root,
                        venue_id: worker.venue_id.as_deref().unwrap_or("BINANCE"),
                        accept_any_venue: false,
                        order_validator: Some(&validator),
                        pipeline: &mut pipeline,
                        worker_id: &worker.id,
                        now,
                        source_seq: &mut source_seq,
                    },
                    venue,
                )?;
                for message in recovery {
                    eprintln!("[HedgeRecovery] {message}");
                }
                drop(venue);
                result
            };
            let (_, record_result) = control_store
                .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
                .map_err(|error| format!("回写 SubmitOrder 终态失败: {error:?}"))?;
            let record = record_result.map_err(|error| format!("执行控制命令失败: {error:?}"))?;
            queue
                .ack_command_at(command.command_id, &owner, lease.fencing_token, now)
                .map_err(|error| format!("确认 SubmitOrder 队列失败: {error:?}"))?;
            context.mark(
                if record.status == qx_control::CommandStatus::Executed {
                    qx_runtime::ServiceStatus::Ready
                } else {
                    qx_runtime::ServiceStatus::Degraded
                },
                format!(
                    "command_id={} status={:?}",
                    command.command_id, record.status
                ),
                Some(now),
            )?;
        }
        context.heartbeat(now)?;
        if once {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn run_binance_spread_recovery_worker(
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
    let mut worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    resolve_worker_runtime_paths(&mut worker, path);
    if !worker.enabled {
        return Err(format!("worker {} 未启用", worker.id));
    }
    if !matches!(
        worker.role,
        WorkerRole::MarketData
            | WorkerRole::UserStream
            | WorkerRole::Execution
            | WorkerRole::SpreadRecovery
            | WorkerRole::Reconciler
    ) {
        return Err(format!(
            "worker {} 不是 Binance 数据/用户流/执行/多腿恢复/对账角色",
            worker.id
        ));
    }
    if worker
        .venue_id
        .as_deref()
        .map(|venue| venue.to_ascii_lowercase().contains("binance"))
        != Some(true)
    {
        return Err(format!("worker {} 不是 Binance Venue", worker.id));
    }
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
        WorkerRole::UserStream => {
            run_binance_user_worker(context, worker_for_run.clone(), pipeline_storage.clone())
        }
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

pub(crate) fn binance_worker_command(argv: &[String]) {
    let path = argv
        .get(2)
        .cloned()
        .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
    let worker_id = match argv.get(3).cloned() {
        Some(worker_id) => worker_id,
        None => {
            eprintln!("binance-worker 需要 worker-id");
            std::process::exit(2);
        }
    };
    let once = argv.iter().any(|argument| argument == "--once");
    if let Err(error) = run_binance_worker(Path::new(&path), &worker_id, once) {
        eprintln!("Binance worker 启动/运行失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn binance_public_probe_command(argv: &[String]) {
    let network = argv.get(2).cloned().unwrap_or_else(|| "testnet".into());
    let instrument = argv
        .get(3)
        .cloned()
        .unwrap_or_else(|| "BTCUSDT.BINANCE".into());
    let testnet = match network.as_str() {
        "testnet" => true,
        "mainnet" => false,
        _ => {
            eprintln!("binance-public-probe network 必须是 testnet 或 mainnet");
            std::process::exit(2);
        }
    };
    if let Err(error) = run_binance_public_probe(testnet, &instrument) {
        eprintln!("Binance public probe 失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn binance_private_probe_command(argv: &[String]) {
    let path = argv
        .get(2)
        .cloned()
        .unwrap_or_else(|| "deploy/qianxing.runtime.production.example.json".into());
    let worker_id = argv
        .get(3)
        .cloned()
        .unwrap_or_else(|| "binance-execution-main".into());
    if let Err(error) = run_binance_private_probe(Path::new(&path), &worker_id) {
        eprintln!("Binance private probe 失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn binance_submit_order_command(argv: &[String]) {
    let path = argv
        .get(2)
        .cloned()
        .unwrap_or_else(|| "deploy/qianxing.runtime.production.example.json".into());
    let worker_id = match argv.get(3).cloned() {
        Some(worker_id) => worker_id,
        None => {
            eprintln!("binance-submit-order 需要 worker-id");
            std::process::exit(2);
        }
    };
    let command_path = match argv.get(4).cloned() {
        Some(command_path) => command_path,
        None => {
            eprintln!("binance-submit-order 需要 command.json");
            std::process::exit(2);
        }
    };
    if let Err(error) =
        run_binance_submit_order(Path::new(&path), &worker_id, Path::new(&command_path))
    {
        eprintln!("Binance SubmitOrder 执行失败: {error}");
        std::process::exit(2);
    }
}
