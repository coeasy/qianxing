use crate::ccxt_submit_args::CcxtSubmitArgs;
use crate::*;

/// 校验 CCXT 一次性提交目标 worker：必须是已启用的 CCXT Execution 角色，且带 account_id/venue_id。
///
/// 这里只做拓扑校验——CCXT 配置（exchange_id 与 venue_id 一致、credential_env 形状合法）
/// 由调用方在解析凭据路径之后用 `validate_ccxt_worker_binding` 复核（与 `run_ccxt_worker` 同源闸门）。
fn validate_ccxt_submit_worker(worker: &WorkerConfig) -> Result<(&str, &str), String> {
    if !worker.enabled || worker.role != WorkerRole::Execution {
        return Err(format!(
            "worker {} 必须是已启用的 CCXT Execution 角色才能提交订单",
            worker.id
        ));
    }
    if VenueFamily::parse_option(worker.venue_id.as_deref()) != Some(VenueFamily::Other) {
        return Err(format!(
            "worker {} 不是 CCXT Venue（venue_id 既不能缺省，也不能是 paper 或 binance）",
            worker.id
        ));
    }
    let account_id = worker
        .account_id
        .as_deref()
        .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))?;
    let venue_id = worker
        .venue_id
        .as_deref()
        .ok_or_else(|| format!("worker {} 缺少 venue_id", worker.id))?;
    Ok((account_id, venue_id))
}

/// 执行一条经过 ControlPlane 审计的 CCXT SubmitOrder 命令（一次性入口）。
///
/// 该入口与 `paper-submit-order` / `binance-submit-order` 同源：配置、命令权限、订单账户/
/// 交易所和 EventLog 先校验；订单事实先写入 `OrderSubmitted`，再由公共 CCXT Worker 归约
/// 成 Accepted/Fill。网络返回未知时 EventLog 保留 Submitted 状态，后续只能通过对账恢复，
/// 绝不自动重试补单。CCXT 凭据只经由环境变量名引用（写在 CCXT 配置 JSON 里），不进命令行、
/// 不进运行时配置、不回显进日志。
pub(crate) fn run_ccxt_submit_order(args: &CcxtSubmitArgs) -> Result<(), String> {
    let runtime_path: &Path = &args.path;
    let worker_id: &str = &args.worker_id;
    let ccxt_config: &Path = &args.ccxt_config;
    let command_path: &Path = &args.command_path;
    let config = read_runtime_config(runtime_path)?;
    let mut worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    resolve_worker_runtime_paths(&mut worker, runtime_path);
    validate_ccxt_submit_worker(&worker)?;
    // 与 CCXT worker 入口同一条闸门：一次性提交也不能把 A 股段收下就什么都不做（V11 Q65）。
    reject_ashare_rules_on_submit_path(runtime_path, Some(&worker), "ccxt-submit-order")?;
    // 与 `run_ccxt_worker` 同源的 CCXT 配置解析与绑定校验：相对路径按运行时配置目录解析，
    // exchange_id 必须与 worker.venue_id 一致，credential_env 的环境变量名必须非空。
    let ccxt_config_path = resolve_ccxt_config_path(runtime_path, &ccxt_config.to_string_lossy());
    validate_ccxt_worker_binding(&worker, Path::new(&ccxt_config_path))?;
    let settlement_currency = account_worker_settlement_currency(&config, &worker)?;
    let command: ControlCommand = serde_json::from_str(
        &std::fs::read_to_string(command_path)
            .map_err(|error| format!("读取 SubmitOrder 命令失败: {error}"))?,
    )
    .map_err(|error| format!("SubmitOrder 命令 JSON 无效: {error}"))?;
    order_from_submit_command(&command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    // 一次性入口也要走命令队列 + EventLog 同一条语义：先持久化 Accepted，再归约终态。
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
    } else if !ccxt_submit_matches_worker(&command, &worker) {
        // 与 `binance_submit` 的拓扑一致性口径同源：显式点名 worker 的一次性入口不得把订单
        // 写进另一个账户 / 另一家交易所的 EventLog（V13 R28 多账户隔离口径在一次性入口不能断）。
        Err("订单 account_id 或 instrument venue 与 worker 拓扑不一致".into())
    } else if let Err(reason) = require_worker_risk_spec(&worker, &command, Some(runtime_path)) {
        Err(reason)
    } else {
        ccxt_submit_action(
            &command,
            &worker,
            &pipeline_storage,
            &settlement_currency,
            &ccxt_config_path,
            runtime_path,
            now,
        )
    };
    let (_, record_result) = store
        .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
        .map_err(|error| format!("持久化 SubmitOrder 终态失败: {error:?}"))?;
    let record = record_result.map_err(|error| format!("执行控制命令失败: {error:?}"))?;
    println!(
        "[CCXT · SubmitOrder] accepted={:?} final={:?} command_id={} result={}",
        accepted.status, record.status, command.command_id, record.result_code
    );
    if record.status == qx_control::CommandStatus::Failed {
        return Err(record.result_code);
    }
    Ok(())
}

/// Accepted 之后到终态回写之间不允许提前退出函数：失败按「这一手有没有可能已经进交易所」分两类。
///
/// - 还没写入任何事实（EventLog 名解析不了、日志或订单组存储打不开、CCXT Worker 起不来）：
///   作为 `Err` 返回，让控制面当场记下终态。这些位置原先是 `?`，函数直接退出，命令永远停在
///   `Accepted`，同 `request_id` 重投只会撞幂等闸门。
/// - 成交之后才失败（订单组快照同步、对冲恢复）：只在 stderr 告警，终态仍归交易所那一手的裁决。
#[allow(clippy::too_many_arguments)]
fn ccxt_submit_action(
    command: &ControlCommand,
    worker: &WorkerConfig,
    pipeline_storage: &PipelineStorage,
    settlement_currency: &str,
    ccxt_config_path: &str,
    runtime_config_path: &Path,
    now: u64,
) -> Result<String, String> {
    let python = python_interpreter();
    let log_name = required_account_event_log(worker).map_err(|error| {
        terminal_submit_rejection(format!("解析 CCXT 执行 EventLog 名失败: {error}"))
    })?;
    let mut pipeline = pipeline_storage
        .open(log_name.clone(), settlement_currency.to_string())
        .map_err(|error| terminal_submit_rejection(format!("打开执行 EventLog 失败: {error}")))?;
    let client = CcxtProcessClient::spawn(&python, ccxt_config_path, None).map_err(|error| {
        terminal_submit_rejection(format!("启动公共 CCXT Worker 失败: {error}"))
    })?;
    let mut venue = CcxtProcessVenue::new(
        worker.venue_id.clone().unwrap_or_else(|| "ccxt".into()),
        Box::new(client),
    );
    let spread_store = open_spread_group_store(&pipeline_storage.root)
        .map_err(|error| terminal_submit_rejection(format!("打开多腿订单组存储失败: {error}")))?;
    let mut source_seq = 0_u64;
    let validator = recovery_order_validator(worker, &pipeline, Some(runtime_config_path));
    let requested_order = order_from_submit_command(command)
        .map_err(|error| terminal_submit_rejection(format!("CCXT 订单载荷非法: {error:?}")))?;
    // 市价单风控参考价：没有 limit 且配了风控规格时，先拉一次 ticker 写进同一 EventLog，
    // 让 `qx-execution` 网关按最新成交价做屏障判定（与 `run_ccxt_execution_worker` 同源）。
    if requested_order.limit.is_none() && worker.instrument_spec_path.is_some() {
        let ticker = venue
            .stream_call(serde_json::json!({
                "op": "fetch_ticker",
                "instrument": requested_order.instrument.to_string(),
            }))
            .map_err(|error| {
                terminal_submit_rejection(format!("CCXT 风控参考价查询失败: {error:?}"))
            })?;
        let ticker = ticker
            .get("ticker")
            .ok_or_else(|| terminal_submit_rejection("CCXT ticker 响应缺少 ticker".into()))?;
        let bid = Price::from_raw(raw_json_i128(ticker, "bid_raw")?);
        let ask = Price::from_raw(raw_json_i128(ticker, "ask_raw")?);
        let bid_qty = Quantity::from_raw(raw_json_i128(ticker, "bid_qty_raw")?);
        let ask_qty = Quantity::from_raw(raw_json_i128(ticker, "ask_qty_raw")?);
        let event_ts = ticker
            .get("timestamp_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(now);
        source_seq = source_seq.saturating_add(1);
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::MarketQuote {
                    instrument: requested_order.instrument.clone(),
                    bid,
                    ask,
                    bid_qty,
                    ask_qty,
                },
                event_ts,
                now,
                source_seq,
                format!("{}:risk-ticker:{}", worker.id, requested_order.client_id),
            ))
            .map_err(|error| {
                terminal_submit_rejection(format!("写入 CCXT 风控参考行情失败: {error:?}"))
            })?;
    }
    let result = execute_submit_order_with_worker_risk(
        command,
        worker,
        &mut venue,
        &mut pipeline,
        now,
        &mut source_seq,
        Some(runtime_config_path),
        Some(&spread_store),
    );
    if is_fail_closed_rejection(&result) {
        // 网关在写入任何事实之前就拒绝了：没有腿订单可归约、没有敞口要补偿，原样把
        // FAIL_CLOSED 原因交给控制面记录。
        drop(venue);
    } else {
        // 成交之后才失败（快照同步、对冲恢复）属于「这一手可能已经进交易所」：
        // 只在 stderr 告警，终态仍归交易所那一手的裁决（与 `run_ccxt_execution_worker` 同源）。
        let latest_pipeline = pipeline_storage
            .open(log_name, settlement_currency.to_string())
            .map_err(|error| {
                terminal_submit_rejection(format!("刷新 CCXT 多腿订单组 EventLog 失败: {error}"))
            })?;
        sync_spread_group_after_order(&pipeline_storage.root, &latest_pipeline, command, now)?;
        let (venue, recovery) = recover_spread_groups_for_venue(
            SpreadRecoveryContext {
                root: &pipeline_storage.root,
                venue_id: worker.venue_id.as_deref().unwrap_or("ccxt"),
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
    }
    result
}
