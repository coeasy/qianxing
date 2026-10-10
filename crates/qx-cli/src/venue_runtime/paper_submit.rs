use crate::*;

/// 为 Paper 账户写入一次可恢复、可幂等的初始资金事实。
/// 资金只通过 AccountCashflow 进入 Ledger，不直接修改内存余额。
pub(crate) fn seed_paper_initial_cash(
    pipeline: &mut LiveEventPipeline,
    worker: &WorkerConfig,
    now: u64,
) -> Result<(), String> {
    let Some(amount_raw) = worker.paper_initial_cash_raw else {
        return Ok(());
    };
    let account_id = worker
        .account_id
        .as_deref()
        .ok_or_else(|| format!("Paper worker {} 缺少 account_id", worker.id))?;
    let currency = pipeline.settlement_currency().to_string();
    let external_id = format!(
        "paper-initial-cash:{}:{}:{}",
        worker.id, account_id, currency
    );
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: account_id.into(),
                    venue_id: "paper".into(),
                    currency,
                    kind: CashflowKind::Transfer,
                    amount: Money::from_raw(amount_raw),
                    external_id: external_id.clone(),
                },
            },
            now,
            now,
            0,
            external_id,
        ))
        .map_err(|error| format!("写入 Paper 初始资金失败: {error:?}"))?;
    Ok(())
}

/// 使用同一控制面/队列/EventLog 语义跑一笔完全本地的 Paper 下单闭环。
/// 该入口用于验收执行编排，不连接网络，也不把 Paper 结果当作真实 Venue 结果。
pub(crate) fn run_paper_submit_order(path: &Path, command_path: &Path) -> Result<(), String> {
    // 这个入口按定义就在提交订单，A 股段配了又没有闸门可用时必须在做任何副作用之前拒掉（V11 Q65）。
    reject_ashare_rules_on_submit_path(path, None, "paper-submit-order")?;
    let config = read_runtime_config(path)?;
    // 初始资金马上要按 worker 那一格入账：先确认它没有和回测侧那一格各说一套（V12 R3）。
    reject_split_account_principal(&config)?;
    if let Some(note) = account_principal_note(&config) {
        println!("{note}");
    }
    let command: ControlCommand = serde_json::from_str(
        &std::fs::read_to_string(command_path)
            .map_err(|error| format!("读取 Paper SubmitOrder 命令失败: {error}"))?,
    )
    .map_err(|error| format!("Paper SubmitOrder 命令 JSON 无效: {error}"))?;
    order_from_submit_command(&command)
        .map_err(|error| format!("Paper SubmitOrder 订单载荷非法: {error:?}"))?;
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let store = configured_control_store(&config)?;
    let queue = configured_command_queue(&config, &root)?;
    let now = runtime_timestamp_ms();
    // 命令队列租约/入队时间在秒域，控制面审计戳保持毫秒（见 `lease_clock`）。
    let lease_now = lease_clock(now);
    // 一次性入口没有 worker_id 参数：按命令自己的账户/交易所挑那台 Paper Execution worker，
    // 而不是无差别取第一台 —— 多账户共享一份控制面时，取第一台会把订单写进另一台 worker 的
    // EventLog（与常驻循环用 `paper_submit_matches_worker` 分派命令是同一道口径）。
    let paper_worker = config
        .workers
        .iter()
        .find(|worker| {
            worker.enabled
                && worker.role == WorkerRole::Execution
                && paper_submit_matches_worker(&command, worker)
        })
        .cloned();
    if let Some(worker) = paper_worker.as_ref() {
        let mut pipeline = open_account_pipeline(
            &config,
            &root,
            &required_account_event_log(worker)?,
            OutboxRecovery::ReprojectOnOpen,
        )
        .map_err(|error| format!("打开 Paper 初始资金 EventLog 失败: {error}"))?;
        seed_paper_initial_cash(&mut pipeline, worker, now)?;
    }
    let (_, accepted_result) = store
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, now))
        .map_err(|error| format!("持久化 Paper SubmitOrder Accepted 失败: {error}"))?;
    let accepted = accepted_result
        .map_err(|error| format!("Paper SubmitOrder 未通过控制面校验: {error:?}"))?;
    queue
        .enqueue_command(command.clone(), lease_now)
        .map_err(|error| format!("写入 Paper SubmitOrder 队列失败: {error:?}"))?;
    let lease = queue
        .claim_command(command.command_id, "paper-execution", lease_now, 30)
        .map_err(|error| format!("领取 Paper SubmitOrder 租约失败: {error:?}"))?;

    let action = if command.dry_run {
        Ok("DRY_RUN_VALIDATED".into())
    } else if let Some(worker) = paper_worker
        .as_ref()
        .filter(|worker| worker.instrument_spec_path.is_some())
    {
        paper_submit_action(&config, path, &command, worker, &root, now)
    } else if paper_worker.is_some() {
        Err(
            "FAIL_CLOSED: Paper worker 缺少风控配置（instrument_spec_path），拒绝提交订单"
                .to_string(),
        )
    } else {
        // 挑不出任何一台与订单账户/交易所一致的启用 Paper worker：fail-closed，
        // 而不是退回「取第一台」把订单写进别的账户账本。
        Err(
            "FAIL_CLOSED: 找不到与订单账户一致的启用 Paper Execution worker，拒绝提交订单"
                .to_string(),
        )
    };
    let (_, record_result) = store
        .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
        .map_err(|error| format!("持久化 Paper SubmitOrder 终态失败: {error}"))?;
    let record = record_result.map_err(|error| format!("Paper 执行控制命令失败: {error:?}"))?;
    queue
        .ack_command_at(
            command.command_id,
            "paper-execution",
            lease.fencing_token,
            lease_now,
        )
        .map_err(|error| format!("确认 Paper SubmitOrder 队列失败: {error:?}"))?;
    println!(
        "[Paper · SubmitOrder] accepted={:?} final={:?} command_id={} result={}",
        accepted.status, record.status, command.command_id, record.result_code
    );
    if record.status == qx_control::CommandStatus::Failed {
        return Err(record.result_code);
    }
    Ok(())
}

/// Accepted 之后到终态回写之间的每一条失败都必须变成 `action` 的值，不允许提前退出函数。
///
/// 命令在进到这一步之前已经写进控制面并入了队列：这里再用 `?` 把错误抛出函数，就会留下一条
/// 永远停在 `Accepted` 的命令、一份没人释放的租约，而运营者按提示重投同一条命令只会撞
/// `DuplicateRequest`（控制面对 `command_id` 与 `request_id` 都做幂等）。缺行情是这条路径上
/// 最常命中的失败，实测见 `logs/s769_pass32_btc_paper_submit.txt`（V13 第三十一遍 ② #273）。
fn paper_submit_action(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
    command: &ControlCommand,
    worker: &WorkerConfig,
    root: &Path,
    now: u64,
) -> Result<String, String> {
    // 跨腿屏障已下沉 `qx-execution` 网关：这里只把存储根解析出的组快照注入提交，
    // 带 `spread_group_id` 的腿若拿不到组存储会在网关内以 FAIL_CLOSED 被拒绝。
    let spread_store = open_spread_group_store(root).map_err(|error| {
        terminal_submit_rejection(format!("打开 Paper 分组屏障存储失败: {error}"))
    })?;
    let mut pipeline = open_account_pipeline(
        config,
        root,
        &required_account_event_log(worker).map_err(|error| {
            terminal_submit_rejection(format!("Paper 执行 worker 缺少 event_log: {error}"))
        })?,
        OutboxRecovery::ReprojectOnOpen,
    )
    .map_err(|error| {
        terminal_submit_rejection(format!("打开 Paper 风控 EventLog 失败: {error}"))
    })?;
    paper_submit_match_attempt(
        config,
        runtime_config_path,
        command,
        worker,
        &mut pipeline,
        &spread_store,
        now,
    )
}

/// Accepted/领取之后那一段的唯一撮合尝试：一次性验收入口与常驻 worker 循环共用。
///
/// 这里的每一条失败都必须以 `Err` 返回，让调用方把它写成命令的终态 —— 命令走到这一步已经
/// 进了控制面与队列，`?` 抛出函数只会留下一条永不结束的 `Accepted` 和一份没人释放的租约。
/// 同一条命令在两个消费者那里必须拿到同一个裁决，否则一次性入口判失败、worker 循环却把
/// 整个执行 worker 打停（V13 第三十一遍 ② #273）。
pub(crate) fn paper_submit_match_attempt(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
    command: &ControlCommand,
    worker: &WorkerConfig,
    pipeline: &mut LiveEventPipeline,
    spread_store: &FileSpreadOrderGroupStore,
    now: u64,
) -> Result<String, String> {
    let order = order_from_submit_command(command)
        .map_err(|error| terminal_submit_rejection(format!("Paper 订单载荷非法: {error:?}")))?;
    let (risk, position) = worker_risk_context(worker, &order, pipeline, Some(runtime_config_path))
        .map_err(|error| terminal_submit_rejection(format!("Paper 风控快照构造失败: {error}")))?
        .ok_or_else(|| {
            terminal_submit_rejection(
                "FAIL_CLOSED: Paper worker 缺少风控配置（instrument_spec_path），拒绝提交订单"
                    .to_string(),
            )
        })?;
    // fail-closed：撮合行情必须来自同一 EventLog 的行情事实，缺行情拒绝执行。
    let market_quote = pipeline
        .latest_quote_with_depth(&order.instrument)
        .ok_or_else(|| {
            terminal_submit_rejection(format!(
                "FAIL_CLOSED: Paper SubmitOrder 缺少 {} 的最新行情事实",
                order.instrument
            ))
        })?;
    execute_paper_submit_effect(
        command,
        pipeline,
        now,
        Some(risk),
        Some(position),
        Some(market_quote),
        // 与回测装配同一份成本口径：费率只从运行时配置的 `cost_rules_path` 来。
        execution_cost_binding_from_config(config, Some(runtime_config_path))
            .map_err(|error| terminal_submit_rejection(format!("读取执行成本规则失败: {error}")))?
            .fee_model(),
        false,
        Some(spread_store),
    )
}

/// 终态拒绝的文案：命令出不了 `Accepted`，所以必须把运营者真正能走的下一步说清楚。
///
/// Paper 与 Binance 的一次性提交入口共用这一条，两处对同一次拒绝给同一句指路。
pub(crate) fn terminal_submit_rejection(reason: String) -> String {
    format!("{reason}；本命令已记为终态失败，修好之后换新的 command_id 与 request_id 重新提交")
}

pub(crate) fn paper_submit_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
    let expected_account = worker.account_id.as_deref().unwrap_or_default();
    order_from_submit_command(command)
        .map(|order| {
            order.account_id == expected_account
                && VenueFamily::parse_option(worker.venue_id.as_deref())
                    == Some(VenueFamily::Paper)
                // Paper 是虚拟执行域，订单 instrument 可以来自 Binance、OKX、
                // Bybit 或自定义市场；不能把虚拟账户误绑死在某一个真实 Venue。
                && (worker.symbols.is_empty()
                    || worker.symbols.iter().any(|symbol| {
                        InstrumentId::parse(symbol)
                            .as_ref()
                            == Some(&order.instrument)
                    }))
        })
        .unwrap_or(false)
}
