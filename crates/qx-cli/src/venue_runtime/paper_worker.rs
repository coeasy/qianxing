use crate::*;

mod paper_check;
pub(crate) use paper_check::collect_paper_check_report;

/// 独立的 Paper 多腿恢复 worker。它只扫描并推进 `HedgeRequired` 快照，
/// 不消费普通 SubmitOrder 队列，使恢复任务可以单独扩缩容、重启和观测。
pub(crate) fn run_paper_spread_recovery_worker(
    path: &Path,
    worker_id: &str,
    once: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker =
        qx_service::worker_host::configured_worker(&config, worker_id, WorkerRole::SpreadRecovery)?;
    validate_worker_market_specs(path, &worker)?;
    if VenueFamily::parse_option(worker.venue_id.as_deref()) != Some(VenueFamily::Paper) {
        return Err(format!(
            "worker {worker_id} 不是启用的 Paper SpreadRecovery worker"
        ));
    }
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let log_name = required_account_event_log(&worker)?;
    let runtime_config = config.clone();
    let runtime_config_path = path.to_path_buf();
    let registered_id = worker.id.clone();
    qx_service::worker_host::run_worker(
        config,
        &registered_id,
        WorkerRole::SpreadRecovery,
        "Paper SpreadRecovery",
        move |context| {
            context.mark(
                qx_runtime::ServiceStatus::Ready,
                "paper spread recovery scanning",
                Some(runtime_timestamp_ms()),
            )?;
            let mut metrics =
                PipelineMetricsReporter::new(&runtime_config, context.id(), &log_name);
            let mut recovery_stalls = 0_u32;
            qx_service::worker_host::run_polling_worker(&context, once, || {
                let now = runtime_timestamp_ms();
                // Paper 恢复接的是任意 venue 的待补偿腿（`accept_any_venue`），所以待恢复计数也
                // 按同一口径取：venue 传空串，不把自己限死在 "paper" 这个腿归属上。
                let pending_before = pending_spread_recovery_groups(&root, "")?;
                if pending_before > 0 {
                    let mut pipeline = open_account_pipeline(
                        &runtime_config,
                        &root,
                        &log_name,
                        OutboxRecovery::ReprojectOnOpen,
                    )
                    .map_err(|error| format!("打开 Paper 多腿恢复 EventLog 失败: {error}"))?;
                    let validator =
                        recovery_order_validator(&worker, &pipeline, Some(&runtime_config_path));
                    let costs = execution_cost_binding_from_config(
                        &runtime_config,
                        Some(&runtime_config_path),
                    )?;
                    for message in recover_paper_spread_groups(
                        &root,
                        &mut pipeline,
                        context.id(),
                        now,
                        Some(&validator),
                        &costs,
                    )? {
                        eprintln!("[HedgeRecovery] {message}");
                    }
                    metrics.absorb(&pipeline);
                    let pending_after = pending_spread_recovery_groups(&root, "")?;
                    recovery_stalls = if pending_after >= pending_before {
                        recovery_stalls.saturating_add(1)
                    } else {
                        0
                    };
                } else {
                    recovery_stalls = 0;
                }
                metrics.publish(true, now);
                context.heartbeat(now)?;
                Ok(spread_recovery_poll_delay(recovery_stalls).as_millis() as u64)
            })?;
            metrics.publish(false, runtime_timestamp_ms());
            Ok(())
        },
    )
}

pub(crate) fn run_paper_execution_worker(
    path: &Path,
    worker_id: &str,
    once: bool,
) -> Result<(), String> {
    run_paper_execution_worker_with_output(path, worker_id, once, false)
}

fn run_paper_execution_worker_with_output(
    path: &Path,
    worker_id: &str,
    once: bool,
    quiet: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker = qx_service::worker_host::configured_worker_for_roles(
        &config,
        worker_id,
        &[WorkerRole::Execution, WorkerRole::SpreadRecovery],
    )?;
    validate_worker_market_specs(path, &worker)?;
    if VenueFamily::parse_option(worker.venue_id.as_deref()) != Some(VenueFamily::Paper) {
        return Err(format!(
            "worker {worker_id} 不是启用的 Paper Execution worker"
        ));
    }
    // 与 binance/ccxt 同口径：会提交订单的 worker 收下 A 股段却不执行，等于让配置说假话（V11 Q65）。
    reject_ashare_rules_on_submit_path(path, Some(&worker), "paper-worker")?;
    // 这个 worker 起来后第一件事就是按 `paper_initial_cash_raw` 入账：先确认这一格没有和
    // 回测侧那一格各说一套，并且把"两处共用同一个本金"说出来（V12 R3）。
    reject_split_account_principal(&config)?;
    if let Some(note) = account_principal_note(&config).filter(|_| !quiet) {
        println!("{note}");
    }
    if worker.role == WorkerRole::SpreadRecovery {
        return run_paper_spread_recovery_worker(path, worker_id, once);
    }
    qx_service::paper_execution_host::run_paper_execution_worker(
        config,
        path.to_path_buf(),
        worker,
        once,
        quiet,
    )
}

/// 按真实 worker 边界顺序执行一轮本地 Paper 主链路。
///
/// 该入口不是新的业务分支，而是把 Scheduler、Strategy 和 Execution 的
/// `--once` 验收顺序固定下来，便于 CI、部署检查和故障恢复测试复用。
pub(crate) use pipeline::{run_paper_pipeline_once, run_paper_pipeline_once_with_report};
mod pipeline;

pub(crate) fn run_paper_check(path: &Path, as_json: bool) -> Result<(), String> {
    if !as_json {
        return run_paper_pipeline_once(path);
    }

    let report = collect_paper_check_report(path)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report)
            .map_err(|error| format!("编码 Paper 检查 JSON 失败: {error}"))?
    );
    if report.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        Ok(())
    } else {
        Err(report["paper_check"]["error"]
            .as_str()
            .unwrap_or("Paper 主链路验收未通过")
            .to_string())
    }
}
