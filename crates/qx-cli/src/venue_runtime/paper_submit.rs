use crate::*;

#[cfg(test)]
pub(crate) use qx_service::paper_execution_host::paper_submit_match_attempt;
pub(crate) use qx_service::paper_execution_host::{
    paper_submit_action, paper_submit_matches_worker, seed_paper_initial_cash_from_config,
};

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
        seed_paper_initial_cash_from_config(&mut pipeline, &config, worker, now)?;
    }
    let (_, accepted_result) = store
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, now))
        .map_err(|error| format!("持久化 Paper SubmitOrder Accepted 失败: {error}"))?;
    let accepted = accepted_result
        .map_err(|error| format!("Paper SubmitOrder 未通过控制面校验: {error:?}"))?;
    queue
        .enqueue_command(command.clone(), lease_now)
        .map_err(|error| {
            format!(
                "Paper SubmitOrder 已受理，但写入队列失败: {error:?}；命令仍保留为 Accepted。修复队列存储/容量后启动对应 Paper Execution worker，它会从控制面待办恢复队列项；不要复用 command_id 或 request_id。"
            )
        })?;
    // A one-shot CLI process is a different executor from the resident worker even though both
    // serve the same configured worker. Reusing `paper-execution` lets lease_rules treat two
    // processes as one owner and issue the same fencing token to both.
    let owner = format!("paper-submit-once-{}-{now}", std::process::id());
    let lease = match queue.claim_command(command.command_id, &owner, lease_now, 30) {
        Ok(lease) => lease,
        Err(qx_storage::StorageError::LeaseHeld {
            owner: active_owner,
            ..
        }) => {
            println!(
                "[Paper · SubmitOrder] accepted=Accepted final=Accepted command_id={} result=已移交给正在持有租约的 worker {active_owner}",
                command.command_id
            );
            return Ok(());
        }
        Err(error) => {
            // The command is already durable in both stores. Preserve Accepted for the queue
            // worker's reconciliation loop instead of guessing that this process owns it.
            return Err(format!(
                "Paper SubmitOrder 已受理并保留在队列中，但本次未领取租约: {error:?}；Paper Execution worker 会继续恢复处理，不要复用 command_id 或 request_id。"
            ));
        }
    };

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
        .ack_command_at(command.command_id, &owner, lease.fencing_token, lease_now)
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
