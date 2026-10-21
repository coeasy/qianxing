use crate::*;

pub(crate) fn run_paper_pipeline_once(path: &Path) -> Result<(), String> {
    run_paper_pipeline_once_with_report(path, true).map(|_| ())
}

pub(crate) struct PaperPipelineOutcome {
    pub(crate) scheduler_id: String,
    pub(crate) strategy_ids: Vec<String>,
    pub(crate) execution_id: String,
    pub(crate) orders_total: usize,
    pub(crate) new_orders: usize,
    pub(crate) ledger_entries_total: usize,
    pub(crate) new_ledger_entries: usize,
}

pub(crate) fn run_paper_pipeline_once_with_report(
    path: &Path,
    print_summary: bool,
) -> Result<PaperPipelineOutcome, String> {
    let config = read_runtime_config(path)?;
    let scheduler_id = config
        .workers
        .iter()
        .find(|worker| worker.enabled && worker.role == WorkerRole::Scheduler)
        .map(|worker| worker.id.clone())
        .ok_or_else(|| "Paper 主链路缺少启用的 Scheduler worker".to_string())?;
    let strategy_ids: Vec<String> = config
        .workers
        .iter()
        .filter(|worker| worker.enabled && worker.role == WorkerRole::Strategy)
        .map(|worker| worker.id.clone())
        .collect();
    if strategy_ids.is_empty() {
        return Err("Paper 主链路缺少启用的 Strategy worker".to_string());
    }
    let execution_id = config
        .workers
        .iter()
        .find(|worker| {
            worker.enabled
                && worker.role == WorkerRole::Execution
                && VenueFamily::parse_option(worker.venue_id.as_deref()) == Some(VenueFamily::Paper)
        })
        .map(|worker| worker.id.clone())
        .ok_or_else(|| "Paper 主链路缺少启用的 Paper Execution worker".to_string())?;

    let quiet_workers = !print_summary;
    run_scheduler_worker(path, &scheduler_id, true, quiet_workers)?;
    // Each Strategy worker owns its own strategy instance and job lease. Running
    // only the first configured worker made this E2E helper falsely certify a
    // multi-strategy Paper topology after exercising just one strategy.
    for strategy_id in &strategy_ids {
        run_strategy_worker(path, strategy_id, true, quiet_workers)?;
    }

    // Paper 执行必须消费已经进入 EventLog 的市场事实。验收 fixture 不连接
    // 网络，因此在执行 worker 前显式注入一条可审计 L1 报价；真实部署由
    // MarketData worker 写入同一账户/运行时日志，不再使用固定价格兜底。
    let execution_worker = config
        .workers
        .iter()
        .find(|worker| worker.id == execution_id)
        .ok_or_else(|| "Paper Execution worker 配置在注入行情前消失".to_string())?;
    let root = Path::new(&config.storage.data_dir);
    let log_name = required_account_event_log(execution_worker)?;
    let mut market_pipeline =
        open_account_pipeline(&config, root, &log_name, OutboxRecovery::ReprojectOnOpen)
            .map_err(|error| format!("打开 Paper 行情 EventLog 失败: {error}"))?;
    // 快照本轮进场时账户事实流里已经躺着多少订单/账本分录。策略阶段只把意向排成
    // 命令（控制面），订单要等执行 worker 撮合才落进事实流，所以这里的读数就是
    // "本轮之前"的基线：同日重跑时上一轮的成交还在这本 EventLog 里，末行必须能
    // 把"本轮真的跑出一手"与"只是复用上一轮的既有事实"分开（V13 R2 #275）。
    let orders_before = market_pipeline.orders().len();
    let ledger_before = market_pipeline.ledger().entries().len();
    let instrument = config
        .strategy
        .instrument
        .as_deref()
        .and_then(InstrumentId::parse)
        .ok_or_else(|| "Paper 验收策略缺少合法 instrument".to_string())?;
    let market_ts = runtime_timestamp_ms();
    market_pipeline
        .ingest(RuntimeEventEnvelope::market_quote(
            instrument,
            QuoteTick::new(
                market_ts,
                Price::from_i64(99),
                Quantity::from_i64(1_000),
                Price::from_i64(100),
                Quantity::from_i64(1_000),
                market_ts,
            ),
            market_ts,
            market_ts,
            format!("{log_name}:paper-check-market:{market_ts}"),
        ))
        .map_err(|error| format!("注入 Paper 验收行情失败: {error:?}"))?;
    super::run_paper_execution_worker_with_output(path, &execution_id, true, quiet_workers)?;

    let root = Path::new(&config.storage.data_dir);
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == execution_id)
        .ok_or_else(|| "Paper Execution worker 配置在验收期间消失".to_string())?;
    let log_name = required_account_event_log(worker)?;
    let pipeline = open_account_pipeline(&config, root, &log_name, OutboxRecovery::ReprojectOnOpen)
        .map_err(|error| format!("打开 Paper 主链路 EventLog 失败: {error}"))?;
    if pipeline.orders().is_empty() || pipeline.ledger().entries().is_empty() {
        let control = configured_control_store(&config)?
            .load()
            .map_err(|error| format!("读取 Paper 主链路审计状态失败: {error}"))?;
        let recent: Vec<String> = control
            .audit()
            .iter()
            .rev()
            .take(3)
            .map(|record| format!("{:?}:{}", record.status, record.result_code))
            .collect();
        return Err(format!(
            "Paper 主链路验收未产生订单或 Ledger 事实；最近命令审计: {recent:?}"
        ));
    }
    let queue = configured_command_queue(&config, root)?;
    if !queue
        .pending_commands()
        .map_err(|error| format!("读取 Paper 主链路命令队列失败: {error:?}"))?
        .is_empty()
    {
        return Err("Paper 主链路验收结束后仍有未确认命令".into());
    }
    let control = configured_control_store(&config)?.load()?;
    let missing_execution_audit: Vec<u64> = pipeline
        .orders()
        .iter()
        .skip(orders_before)
        .filter(|order| {
            !control.audit().iter().any(|record| {
                record.status == qx_control::CommandStatus::Executed
                    && record.command_id == order.client_id
            })
        })
        .map(|order| order.client_id)
        .collect();
    if !missing_execution_audit.is_empty() {
        return Err(format!(
            "Paper 主链路验收缺少 SubmitOrder Executed 审计记录: {missing_execution_audit:?}"
        ));
    }
    // 末行按"本轮新增"而不是事实流的累计量给结论：同日重跑时上一轮的成交还躺在这本
    // EventLog 里，`orders()` 非空、`orders()[0]` 的 Executed 审计也还在，上面的闸门
    // 全都过——可本轮调度 skipped=1、策略与执行各 processed=0，端到端其实一手没跑。
    // 若照旧只印累计数与 ✓，就把一次空转报成了一次验收通过（V13 R2 #275）。
    let orders_now = pipeline.orders().len();
    let ledger_now = pipeline.ledger().entries().len();
    let new_orders = orders_now.saturating_sub(orders_before);
    let new_ledger = ledger_now.saturating_sub(ledger_before);
    if print_summary {
        if new_orders > 0 {
            println!(
                "[Paper · E2E] scheduler={} strategy={} execution={} orders={} (+{} 本轮新增) ledger_entries={} (+{} 本轮新增) ✓",
                scheduler_id,
                strategy_ids.join(","),
                execution_id,
                orders_now,
                new_orders,
                ledger_now,
                new_ledger
            );
        } else {
            println!(
                "[Paper · E2E] scheduler={} strategy={} execution={} orders={} (+0 本轮新增) ledger_entries={} 本轮零新增：当日调度已跳过、复用上一轮既有事实，未端到端重跑",
                scheduler_id,
                strategy_ids.join(","),
                execution_id,
                orders_now,
                ledger_now
            );
        }
    }
    Ok(PaperPipelineOutcome {
        scheduler_id,
        strategy_ids,
        execution_id,
        orders_total: orders_now,
        new_orders,
        ledger_entries_total: ledger_now,
        new_ledger_entries: new_ledger,
    })
}
