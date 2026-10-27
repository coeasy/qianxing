//! Service-owned Paper account admission, cash seeding and venue execution adapters.

use crate::backtest_bindings::execution_cost_binding_from_config;
use crate::configured_backends::{configured_command_queue, configured_control_store};
use crate::paper_algo_host::run_paper_algo_execution_cycle;
use crate::pipeline::open_account_pipeline;
use crate::pipeline_metrics_report::PipelineMetricsReporter;
use crate::risk_host::worker_account_admission_lock;
use crate::spread_recovery_host::{
    command_is_final, dedicated_spread_recovery_configured, is_fail_closed_rejection,
    is_retryable_spread_submission, open_spread_group_store,
    recover_paper_spread_groups_for_account, recovery_order_validator,
    sync_in_flight_spread_orders, sync_spread_group_after_order,
};
use crate::worker_host::{run_polling_worker, run_worker};
use qx_control::{order_from_submit_command, CommandKind, ControlCommand};
use qx_core::{AccountCashflow, CashflowKind, InstrumentId, Money, VenueFamily};
use qx_execution::execute_paper_submit_effect;
use qx_runtime::{
    lease_clock, required_account_event_log, LiveEventPipeline, OutboxRecovery, RuntimeConfig,
    RuntimeEventEnvelope, RuntimeExternalEvent, ServiceStatus, WorkerConfig, WorkerRole,
};
use qx_storage::StorageError;
use qx_zhenlu::FileSpreadOrderGroupStore;
use std::path::Path;

fn runtime_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// 为 Paper 账户写入一次可恢复、可幂等的初始资金事实。
/// 资金只通过 AccountCashflow 进入 Ledger，不直接修改内存余额。
pub fn seed_paper_initial_cash_from_config(
    pipeline: &mut LiveEventPipeline,
    config: &RuntimeConfig,
    worker: &WorkerConfig,
    now: u64,
) -> Result<(), String> {
    let account_cash = worker
        .account_id
        .as_deref()
        .and_then(|account_id| config.account(account_id))
        .and_then(|account| account.initial_cash_raw);
    let amount_raw = account_cash.or(worker.paper_initial_cash_raw);
    seed_paper_initial_cash_amount(pipeline, worker, amount_raw, account_cash.is_some(), now)
}

fn seed_paper_initial_cash_amount(
    pipeline: &mut LiveEventPipeline,
    worker: &WorkerConfig,
    amount_raw: Option<i128>,
    account_registry_source: bool,
    now: u64,
) -> Result<(), String> {
    let Some(amount_raw) = amount_raw else {
        return Ok(());
    };
    let account_id = worker
        .account_id
        .as_deref()
        .ok_or_else(|| format!("Paper worker {} 缺少 account_id", worker.id))?;
    let currency = pipeline.settlement_currency().to_string();
    // 保持旧 worker 级身份键，升级后重启旧配置不会重复注入既有本金；注册表资金则用
    // 账户级键，多个执行 worker 对同一本账户账簿只会入账一次。
    let external_id = if account_registry_source {
        format!("paper-initial-cash:account:{account_id}:{currency}")
    } else {
        format!(
            "paper-initial-cash:{}:{}:{}",
            worker.id, account_id, currency
        )
    };
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

/// Run one configured Paper execution worker under the shared service supervisor.
pub fn run_paper_execution_worker(
    config: RuntimeConfig,
    config_path: std::path::PathBuf,
    worker: WorkerConfig,
    once: bool,
    quiet: bool,
) -> Result<(), String> {
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let store = configured_control_store(&config)?;
    let queue = configured_command_queue(&config, &root)?;
    let log_name = required_account_event_log(&worker)?;
    let runtime_config = config.clone();
    let dedicated_spread_recovery = dedicated_spread_recovery_configured(&config, &worker);
    let algo_checkpoints = qx_runtime::AlgoExecutionCheckpointStore::new(&root);
    let registered_id = worker.id.clone();
    let runtime_config_path = config_path;
    run_worker(
        config,
        &registered_id,
        WorkerRole::Execution,
        "Paper",
        move |context| {
            let mut metrics =
                PipelineMetricsReporter::new(&runtime_config, context.id(), &log_name);
            let mut pipeline = open_account_pipeline(
                &runtime_config,
                &root,
                &log_name,
                OutboxRecovery::ReprojectOnOpen,
            )
            .map_err(|error| format!("打开 Paper 初始资金 EventLog 失败: {error}"))?;
            seed_paper_initial_cash_from_config(
                &mut pipeline,
                &runtime_config,
                &worker,
                runtime_timestamp_ms(),
            )?;
            metrics.absorb(&pipeline);
            drop(pipeline);
            let mut algo_page_cursor = None::<String>;
            context.mark(
                ServiceStatus::Ready,
                "paper execution queue polling",
                Some(runtime_timestamp_ms()),
            )?;
            run_polling_worker(&context, once, || {
                let now = runtime_timestamp_ms();
                // 命令队列租约/入队时间在秒域，控制面审计戳保持毫秒（见 `lease_clock`）。
                let lease_now = lease_clock(now);
                let control = store.load()?;
                for command in control.pending().filter(|command| {
                    matches!(command.kind, CommandKind::SubmitOrder)
                        && paper_submit_matches_worker(command, &worker)
                }) {
                    queue
                        .enqueue_command(command.clone(), lease_now)
                        .map_err(|error| format!("补入 Paper SubmitOrder 队列失败: {error:?}"))?;
                }
                let mut processed = 0_usize;
                for queued in queue
                    .available_commands(lease_now)
                    .map_err(|error| format!("读取 Paper SubmitOrder 队列失败: {error:?}"))?
                {
                    let command = queued.command.clone();
                    if !paper_submit_matches_worker(&command, &worker) {
                        continue;
                    }
                    let lease = match queue.claim_command(
                        command.command_id,
                        context.id(),
                        lease_now,
                        30,
                    ) {
                        Ok(lease) => lease,
                        Err(StorageError::LeaseHeld { .. }) => continue,
                        Err(error) => {
                            return Err(format!("领取 Paper SubmitOrder 租约失败: {error:?}"));
                        }
                    };
                    if command_is_final(&control, command.command_id) {
                        queue
                            .ack_command_at(
                                command.command_id,
                                context.id(),
                                lease.fencing_token,
                                lease_now,
                            )
                            .map_err(|error| {
                                format!("清理已终态 Paper SubmitOrder 失败: {error:?}")
                            })?;
                        continue;
                    }
                    let action = if command.dry_run {
                        Ok("DRY_RUN_VALIDATED".into())
                    } else {
                        // 跨腿屏障由 `qx-execution` 网关在写入任何事实之前自行判定，
                        // 这里只注入由存储根解析出的组快照。
                        let spread_store = open_spread_group_store(&root)?;
                        let mut market_pipeline = open_account_pipeline(
                            &runtime_config,
                            &root,
                            &log_name,
                            OutboxRecovery::ReprojectOnOpen,
                        )
                        .map_err(|error| format!("打开 Paper 行情 EventLog 失败: {error}"))?;
                        sync_in_flight_spread_orders(
                            &root,
                            &market_pipeline,
                            command.payload.get("spread_group_id").map(String::as_str),
                            worker.account_id.as_deref(),
                            worker.venue_id.as_deref(),
                        )?;
                        // Accepted/领取之后的可预期失败都要变成这条命令的终态：缺行情只是
                        // 「行情还没进来」，它不该把整个执行 worker 打停，更不该让同一条命令
                        // 在一次性入口和常驻循环里得到两种裁决（V13 第三十一遍 ② #273）。
                        let result = paper_submit_match_attempt(
                            &runtime_config,
                            &runtime_config_path,
                            &root,
                            &command,
                            &worker,
                            &mut market_pipeline,
                            &spread_store,
                            now,
                        );
                        let outcome = if is_fail_closed_rejection(&result)
                            || is_retryable_spread_submission(&result)
                        {
                            // 网关未写入任何事实：归约会把真正的 FAIL_CLOSED 文案覆盖成
                            // "订单组缺少 EventLog 订单"，因此这条路径直接保留原拒绝原因。
                            result
                        } else {
                            // 执行事实已写入同一个 pipeline，多腿订单组归约无需再打开一次日志。
                            sync_spread_group_after_order(&root, &market_pipeline, &command, now)?;
                            result
                        };
                        metrics.absorb(&market_pipeline);
                        outcome
                    };
                    if is_retryable_spread_submission(&action) {
                        // Keep the command pending while another leg's submit intent is
                        // unresolved; queue lease expiry provides the retry boundary.
                        continue;
                    }
                    let (_, record_result) = store
                        .transact(|plane| {
                            plane.execute(command.command_id, now, |_| action.clone())
                        })
                        .map_err(|error| format!("回写 Paper SubmitOrder 终态失败: {error}"))?;
                    let record = record_result
                        .map_err(|error| format!("Paper SubmitOrder 执行失败: {error:?}"))?;
                    queue
                        .ack_command_at(
                            command.command_id,
                            context.id(),
                            lease.fencing_token,
                            lease_now,
                        )
                        .map_err(|error| format!("确认 Paper SubmitOrder 失败: {error:?}"))?;
                    processed += 1;
                    context.mark(
                        if record.status == qx_control::CommandStatus::Executed {
                            ServiceStatus::Ready
                        } else {
                            ServiceStatus::Degraded
                        },
                        format!(
                            "command_id={} status={:?}",
                            command.command_id, record.status
                        ),
                        Some(now),
                    )?;
                }
                let mut algo_pipeline = open_account_pipeline(
                    &runtime_config,
                    &root,
                    &log_name,
                    OutboxRecovery::ReprojectOnOpen,
                )
                .map_err(|error| format!("打开 Paper 算法订单 EventLog 失败: {error}"))?;
                let (algo_advanced, next_page_cursor) = run_paper_algo_execution_cycle(
                    &runtime_config,
                    &runtime_config_path,
                    &worker,
                    &root,
                    &log_name,
                    &mut algo_pipeline,
                    &algo_checkpoints,
                    algo_page_cursor.as_deref(),
                    now,
                )?;
                processed += algo_advanced;
                algo_page_cursor = next_page_cursor;
                metrics.absorb(&algo_pipeline);
                if !dedicated_spread_recovery {
                    let mut recovery_pipeline = open_account_pipeline(
                        &runtime_config,
                        &root,
                        &log_name,
                        OutboxRecovery::ReprojectOnOpen,
                    )
                    .map_err(|error| format!("打开 Paper 多腿恢复 EventLog 失败: {error}"))?;
                    let validator = recovery_order_validator(
                        &worker,
                        &recovery_pipeline,
                        Some(&runtime_config_path),
                    );
                    let costs = execution_cost_binding_from_config(
                        &runtime_config,
                        Some(&runtime_config_path),
                    )?;
                    for message in recover_paper_spread_groups_for_account(
                        &root,
                        worker.account_id.as_deref(),
                        &mut recovery_pipeline,
                        context.id(),
                        now,
                        Some(&validator),
                        &costs,
                    )? {
                        eprintln!("[HedgeRecovery] {message}");
                    }
                    metrics.absorb(&recovery_pipeline);
                }
                metrics.publish(true, now);
                context.heartbeat(now)?;
                if once && !quiet {
                    println!(
                        "[Paper · Execution] worker={} processed={}",
                        context.id(),
                        processed
                    );
                }
                Ok(100)
            })?;
            metrics.publish(false, runtime_timestamp_ms());
            Ok(())
        },
    )
}

/// Accepted 之后到终态回写之间的每一条失败都必须变成 `action` 的值，不允许提前退出函数。
///
/// 命令在进到这一步之前已经写进控制面并入了队列：这里再用 `?` 把错误抛出函数，就会留下一条
/// 永远停在 `Accepted` 的命令、一份没人释放的租约，而运营者按提示重投同一条命令只会撞
/// `DuplicateRequest`（控制面对 `command_id` 与 `request_id` 都做幂等）。缺行情是这条路径上
/// 最常命中的失败，实测见 `logs/s769_pass32_btc_paper_submit.txt`（V13 第三十一遍 ② #273）。
pub fn paper_submit_action(
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
        root,
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
#[allow(clippy::too_many_arguments)] // 配置源路径与数据根目录分别用于资产解析和账户锁定，不能合并成同一语义。
pub fn paper_submit_match_attempt(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
    root: &Path,
    command: &ControlCommand,
    worker: &WorkerConfig,
    pipeline: &mut LiveEventPipeline,
    spread_store: &FileSpreadOrderGroupStore,
    now: u64,
) -> Result<String, String> {
    let log_name = required_account_event_log(worker).map_err(terminal_submit_rejection)?;
    let _admission_lock =
        worker_account_admission_lock(root, &log_name).map_err(terminal_submit_rejection)?;
    // Pipeline 可能在另一个 worker 等锁时打开；拿到锁后必须增量追平 EventLog，
    // 否则锁只能串行执行旧快照，无法释放已提交订单占用的资金预留。
    pipeline.refresh().map_err(|error| {
        terminal_submit_rejection(format!("刷新 Paper 风控 EventLog 失败: {error:?}"))
    })?;
    let order = order_from_submit_command(command)
        .map_err(|error| terminal_submit_rejection(format!("Paper 订单载荷非法: {error:?}")))?;
    // A blocked spread group must win over secondary account-risk diagnostics. The execution
    // gateway repeats this same shared predicate immediately before side effects.
    qx_execution::spread_group_barrier(Some(spread_store), command)
        .map_err(terminal_submit_rejection)?;
    let (risk, position) = crate::risk_host::worker_risk_context(
        worker,
        &order,
        pipeline,
        Some(config),
        Some(runtime_config_path),
    )
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
    execute_paper_submit_effect(qx_execution::PaperSubmitRequest {
            command,
            pipeline,
            now,
            risk: Some(risk),
            position: Some(position),
            market_quote: Some(market_quote),
            fee_model: // 与回测装配同一份成本口径：费率只从运行时配置的 `cost_rules_path` 来。
        execution_cost_binding_from_config(config, Some(runtime_config_path))
            .map_err(|error| terminal_submit_rejection(format!("读取执行成本规则失败: {error}")))?
            .fee_model(),
            allow_synthetic_quote: false,
            spread_store: Some(spread_store)
        })
}

/// 终态拒绝的文案：命令出不了 `Accepted`，所以必须把运营者真正能走的下一步说清楚。
///
/// Paper 与 Binance 的一次性提交入口共用这一条，两处对同一次拒绝给同一句指路。
pub fn terminal_submit_rejection(reason: String) -> String {
    format!("{reason}；本命令已记为终态失败，修好之后换新的 command_id 与 request_id 重新提交")
}

pub fn paper_submit_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
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
