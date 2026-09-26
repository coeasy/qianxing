//! 多腿价差订单组：信号组标识、持久化、终态判定与断线恢复。
//!
//! 订单组状态机本身来自 qx-zhenlu，这里只负责 CLI 侧的存储根解析与恢复编排。

use super::*;
pub(crate) fn spread_group_id(strategy_id: &str, run_id: u64, signal_id: u64) -> String {
    format!("spread-{strategy_id}-{run_id}-{signal_id}")
}

/// 在多腿命令进入队列前持久化完整订单组。组快照不是成交事实，事实仍由
/// 各 Venue EventLog 写入；它只提供跨 worker 的腿身份、路由和重启恢复索引。
pub(crate) fn persist_strategy_spread_group(
    root: &Path,
    group_id: &str,
    strategy_id: &str,
    orders: &[Order],
) -> Result<(), String> {
    if orders.len() < 2 {
        return Ok(());
    }
    let legs = orders
        .iter()
        .map(|order| SpreadOrderLeg {
            leg_id: format!("leg-{}", order.client_id),
            venue_id: order.instrument.venue.to_string(),
            order: order.clone(),
        })
        .collect();
    let group = SpreadOrderGroup::new(group_id, strategy_id, legs)
        .map_err(|error| format!("创建多腿订单组失败: {error:?}"))?;
    let mut store = FileSpreadOrderGroupStore::new(root.join("spread-groups"))?;
    if let Some(existing) = store.load(group_id)? {
        if existing != group {
            return Err(format!("多腿订单组 {} 已存在且内容不一致", group_id));
        }
        return Ok(());
    }
    store.save(&group)
}

/// 打开流水线存储根下的多腿订单组存储（`<root>/spread-groups`）。
///
/// 生产提交路径都用它拿到组存储，再把这个 `Option<&dyn SpreadOrderGroupStore>`
/// 交给 `qx-execution` 的提交入口——屏障判定住在网关，CLI 只负责解析存储根。
pub(crate) fn open_spread_group_store(root: &Path) -> Result<FileSpreadOrderGroupStore, String> {
    FileSpreadOrderGroupStore::new(root.join("spread-groups"))
}

/// 提交入口是否以 **fail-closed** 被拒绝（网关内的屏障、风控或配置缺失判定）。
///
/// 网关用 `FAIL_CLOSED:` 前缀标注"在写入任何事实之前就拒绝"，CLI 据此分类：
/// 这类提交没有腿订单事实可归约、也没有敞口需要补偿，因此组快照同步与恢复扫描
/// 都必须跳过，否则后续步骤会用"缺少 EventLog 订单"覆盖掉真正的原因文案。
pub(crate) fn is_fail_closed_rejection(result: &Result<String, String>) -> bool {
    result
        .as_ref()
        .err()
        .is_some_and(|error| error.starts_with("FAIL_CLOSED:"))
}

/// 将某条执行 worker 已写入 EventLog 的订单状态归约到多腿组快照。
/// 只依据本地事实更新，不根据命令成功返回值臆造成交；部分成交、取消和未知
/// 状态分别进入 SpreadOrderGroup 的补偿/对账状态机。
pub(crate) fn sync_spread_group_after_order(
    root: &Path,
    pipeline: &LiveEventPipeline,
    command: &ControlCommand,
    now: u64,
) -> Result<(), String> {
    let Some(group_id) = command.payload.get("spread_group_id") else {
        return Ok(());
    };
    let order = order_from_submit_command(command)
        .map_err(|error| format!("读取多腿 SubmitOrder 失败: {error:?}"))?;
    let current = pipeline
        .orders()
        .into_iter()
        .find(|candidate| candidate.client_id == order.client_id)
        .ok_or_else(|| format!("多腿订单组缺少 EventLog 订单 {}", order.client_id))?;
    let mut store = FileSpreadOrderGroupStore::new(root.join("spread-groups"))?;
    let Some(mut group) = store.load(group_id)? else {
        return Err(format!("找不到多腿订单组快照: {group_id}"));
    };
    let leg_id = format!("leg-{}", current.client_id);
    let recorded_filled = group
        .leg(&leg_id)
        .map_err(|error| format!("读取多腿订单腿失败: {error:?}"))?
        .order
        .filled
        .raw();
    let filled_delta = current.filled.raw().saturating_sub(recorded_filled);
    if filled_delta > 0 {
        let fill = qx_core::Fill {
            order_id: current.client_id,
            qty: Quantity::from_raw(filled_delta),
            price: current.limit.unwrap_or_else(|| Price::from_i64(1)),
            fee: Money::ZERO,
            ts: now,
            account_id: current.account_id.clone(),
            ..qx_core::Fill::default()
        };
        group
            .record_fill(&leg_id, &fill)
            .map_err(|error| format!("归约多腿成交失败: {error:?}"))?;
    }
    match current.status {
        OrderStatus::Accepted
        | OrderStatus::Working
        | OrderStatus::PartiallyFilled
        | OrderStatus::Filled => group
            .record_accepted(&leg_id)
            .map_err(|error| format!("归约多腿 Accepted 失败: {error:?}"))?,
        OrderStatus::Cancelled | OrderStatus::Expired => group
            .record_cancelled(&leg_id)
            .map_err(|error| format!("归约多腿 Cancelled 失败: {error:?}"))?,
        OrderStatus::Rejected => group
            .record_rejected(&leg_id)
            .map_err(|error| format!("归约多腿 Rejected 失败: {error:?}"))?,
        OrderStatus::Unknown => group
            .record_unknown(&leg_id)
            .map_err(|error| format!("归约多腿 Unknown 失败: {error:?}"))?,
        OrderStatus::PendingSubmit | OrderStatus::Submitted | OrderStatus::CancelPending => {}
    }
    store.save(&group)
}

pub(crate) struct SpreadRecoveryContext<'a> {
    pub(crate) root: &'a Path,
    pub(crate) venue_id: &'a str,
    pub(crate) accept_any_venue: bool,
    pub(crate) order_validator: Option<&'a dyn HedgeOrderValidator>,
    pub(crate) pipeline: &'a mut LiveEventPipeline,
    pub(crate) worker_id: &'a str,
    pub(crate) now: u64,
    pub(crate) source_seq: &'a mut u64,
}

/// 扫描单机多腿恢复快照，并在当前执行 Venue 上处理已确认的风险敞口。
///
/// 该函数只处理 `HedgeRequired`，永远跳过 `ReconcileRequired`；后者必须先
/// 由用户流/对账 worker 得到确定的远端事实。补偿订单仍由
/// `HedgeRecoveryWorker` 生成确定性 client_order_id，并通过同一 EventLog
/// 事实端口归约，因而执行 worker 重启不会重复提交。
pub(crate) fn recover_spread_groups_for_venue<V: Venue>(
    context: SpreadRecoveryContext<'_>,
    venue: V,
) -> Result<(V, Vec<String>), String> {
    let SpreadRecoveryContext {
        root,
        venue_id,
        accept_any_venue,
        order_validator,
        pipeline,
        worker_id,
        now,
        source_seq,
    } = context;
    let probe_store = FileSpreadOrderGroupStore::new(root.join("spread-groups"))?;
    let groups = probe_store
        .group_ids()?
        .into_iter()
        .filter_map(|group_id| match probe_store.load(&group_id) {
            Ok(Some(group))
                if group.status == SpreadOrderGroupStatus::HedgeRequired
                    && group.legs.iter().any(|leg| {
                        leg.order.filled.raw() > 0
                            && (venue_id.trim().is_empty()
                                || leg.venue_id.eq_ignore_ascii_case(venue_id))
                    }) =>
            {
                Some(Ok(group))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>, String>>()?;
    if groups.is_empty() {
        return Ok((venue, Vec::new()));
    }

    let mut store = FileSpreadOrderGroupStore::new(root.join("spread-groups"))?;
    let mut router = if accept_any_venue {
        VenuePortAdapter::new_for_any_venue(venue)
    } else {
        VenuePortAdapter::new(venue)
    };
    let mut diagnostics = Vec::new();
    for group in groups {
        let group_id = group.group_id.clone();
        let outcome = HedgeRecoveryWorker::new(
            group,
            &mut store,
            &mut router,
            pipeline,
            worker_id,
            now,
            source_seq,
        )
        .map_err(|error| format!("构造多腿恢复 worker 失败: {error:?}"))?
        .execute_with_validator(order_validator)?;
        if outcome.completed {
            diagnostics.push(format!("spread_group={group_id} hedge=completed"));
        }
        diagnostics.extend(
            outcome
                .errors
                .into_iter()
                .map(|error| format!("spread_group={group_id} hedge=pending reason={error}")),
        );
    }
    Ok((router.into_inner(), diagnostics))
}

pub(crate) fn has_pending_spread_recovery(root: &Path, venue_id: &str) -> Result<bool, String> {
    let store = FileSpreadOrderGroupStore::new(root.join("spread-groups"))?;
    for group_id in store.group_ids()? {
        let Some(group) = store.load(&group_id)? else {
            continue;
        };
        if group.status != SpreadOrderGroupStatus::HedgeRequired {
            continue;
        }
        if group.legs.iter().any(|leg| {
            leg.order.filled.raw() > 0
                && (venue_id.trim().is_empty() || leg.venue_id.eq_ignore_ascii_case(venue_id))
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn dedicated_spread_recovery_configured(
    config: &RuntimeConfig,
    execution_worker: &WorkerConfig,
) -> bool {
    let Some(account_id) = execution_worker.account_id.as_deref() else {
        return false;
    };
    let Some(venue_id) = execution_worker.venue_id.as_deref() else {
        return false;
    };
    config.workers.iter().any(|worker| {
        worker.enabled
            && worker.role == WorkerRole::SpreadRecovery
            && worker.account_id.as_deref() == Some(account_id)
            && worker
                .venue_id
                .as_deref()
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(venue_id))
    })
}

/// 恢复扫描的补偿腿 guard：账户级风控 + 该 worker 的冻结产品规格。
///
/// 两件事必须同源：正常提交链路上 `instrument_spec_path` 既是风控的规格来源，也是
/// 提交侧精度闸门与衍生品记账的规格来源。补偿提交若只带风控不带规格，同一笔越界
/// 成交就会走提交回包直接入账，而衍生品腿会被按乘数 1 记账。
pub(crate) struct RecoveryGuard<'a> {
    worker: &'a WorkerConfig,
    /// 风控读取的是恢复扫描开始时的不可变快照；本轮补偿产生的事实会在下一轮
    /// 扫描重新加载，避免在同一轮里用半更新状态重复计算风险。
    risk_pipeline: LiveEventPipeline,
    runtime_config_path: Option<&'a Path>,
}

pub(crate) fn recovery_order_validator<'a>(
    worker: &'a WorkerConfig,
    pipeline: &LiveEventPipeline,
    runtime_config_path: Option<&'a Path>,
) -> RecoveryGuard<'a> {
    RecoveryGuard {
        worker,
        risk_pipeline: pipeline.clone(),
        runtime_config_path,
    }
}

impl HedgeOrderValidator for RecoveryGuard<'_> {
    fn validate(&self, order: &Order) -> Result<(), String> {
        if let Some((risk, position)) = worker_risk_context(
            self.worker,
            order,
            &self.risk_pipeline,
            self.runtime_config_path,
        )? {
            risk.validate_order(order, &position)
                .map_err(|error| format!("{error:?}"))?;
        }
        Ok(())
    }

    /// 规格解析与风控共用 `load_worker_instrument_spec` 这唯一的读法。`Ok(None)`
    /// 只对应"该 worker 未配置规格"（现货 `contract_size=1` 的既有口径）；文件缺失、
    /// 形状不符等解析失败必须原样上报，由 worker 拒绝补偿。
    fn instrument_spec(&self, order: &Order) -> Result<Option<TradingInstrumentSpec>, String> {
        load_worker_instrument_spec(self.worker, order, self.runtime_config_path)
    }
}

/// Paper 恢复使用同一 `PaperVenue` 复现补偿单的 Accepted→Fill 过程，
/// 但只恢复 `spread-hedge-v1` 订单，避免恢复扫描误撮合普通策略订单。
pub(crate) fn recover_paper_spread_groups(
    root: &Path,
    pipeline: &mut LiveEventPipeline,
    worker_id: &str,
    now: u64,
    order_validator: Option<&dyn HedgeOrderValidator>,
    costs: &ExecutionCostBinding,
) -> Result<Vec<String>, String> {
    let hedge_orders = pipeline
        .orders()
        .into_iter()
        .filter(|order| {
            order
                .trace
                .as_ref()
                .and_then(|trace| trace.rule_version.as_deref())
                == Some("spread-hedge-v1")
        })
        .collect::<Vec<_>>();
    let mut venue = PaperVenue::new("paper", costs.fee_model());
    venue
        .restore_orders(hedge_orders)
        .map_err(|error| format!("恢复 Paper 补偿订单失败: {error:?}"))?;
    let mut source_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.source_seq)
        .unwrap_or(0);
    let (mut venue, mut diagnostics) = recover_spread_groups_for_venue(
        SpreadRecoveryContext {
            root,
            venue_id: "",
            accept_any_venue: true,
            order_validator,
            pipeline,
            worker_id,
            now,
            source_seq: &mut source_seq,
        },
        venue,
    )?;

    // 每条待补偿腿的规格都从它自己的订单解析：PaperVenue 由行情撮合出来的成交是
    // 被推下来的回报，走 `ingest_venue_events` 就不带规格，衍生品腿会按乘数 1 记账。
    let pending_legs = pipeline
        .orders()
        .into_iter()
        .filter(|order| {
            order
                .trace
                .as_ref()
                .and_then(|trace| trace.rule_version.as_deref())
                == Some("spread-hedge-v1")
                && !order.status.is_terminal()
        })
        .fold(BTreeMap::new(), |mut acc: BTreeMap<_, Order>, order| {
            acc.entry(order.instrument.clone()).or_insert(order);
            acc
        });
    for (instrument, leg_order) in pending_legs {
        let Some(quote) = pipeline.latest_quote_with_depth(&instrument) else {
            diagnostics.push(format!(
                "spread_hedge instrument={} pending reason=缺少最新行情",
                instrument
            ));
            continue;
        };
        // 外层 `Option` 是"本轮有没有 guard"，内层是"该 worker 有没有配置规格"。
        let spec = match order_validator {
            Some(validator) => match validator.instrument_spec(&leg_order) {
                Ok(spec) => spec,
                Err(error) => {
                    diagnostics.push(format!(
                        "spread_hedge instrument={instrument} pending reason=补偿腿产品规格解析失败 {error}"
                    ));
                    continue;
                }
            },
            None => None,
        };
        let events = venue.on_quote(&instrument, quote);
        if !events.is_empty() {
            let ingested = match spec.as_ref() {
                Some(spec) => ingest_venue_events_with_spec(
                    pipeline,
                    events,
                    worker_id,
                    now,
                    &mut source_seq,
                    spec,
                ),
                None => ingest_venue_events(pipeline, events, worker_id, now, &mut source_seq),
            };
            ingested.map_err(|error| format!("写入 Paper 补偿成交事实失败: {error}"))?;
        }
    }

    // 第一轮负责生成补偿单，行情驱动后第二轮确认其 Filled 并将组标记 Hedged。
    let (_venue, second_pass) = recover_spread_groups_for_venue(
        SpreadRecoveryContext {
            root,
            venue_id: "",
            accept_any_venue: true,
            order_validator,
            pipeline,
            worker_id,
            now,
            source_seq: &mut source_seq,
        },
        venue,
    )?;
    diagnostics.extend(second_pass);
    Ok(diagnostics)
}

pub(crate) fn persist_strategy_submit(
    control_store: &ControlStateBackend,
    command_queue: &dyn ControlCommandQueueBackend,
    command: &ControlCommand,
    now: u64,
) -> Result<String, String> {
    // 控制面审计戳留在毫秒域，命令队列信封的 enqueued_ts 属于租约域。
    let lease_now = lease_clock(now);
    let (plane, result) = control_store
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, now))
        .map_err(|error| format!("Strategy SubmitOrder Accepted 持久化失败: {error}"))?;
    match result {
        Ok(_) => {
            command_queue
                .enqueue_command(command.clone(), lease_now)
                .map_err(|error| format!("Strategy SubmitOrder 入队失败: {error:?}"))?;
            Ok("ORDER_INTENT_ACCEPTED".into())
        }
        Err(
            qx_control::ControlError::DuplicateCommand(_)
            | qx_control::ControlError::DuplicateRequest(_),
        ) => {
            let existing = plane
                .command(command.command_id)
                .ok_or_else(|| "Strategy 幂等命令缺少原命令".to_string())?;
            if existing.digest() != command.digest() {
                return Err("Strategy command_id 已被不同 OrderIntent 占用".into());
            }
            let status = plane
                .audit()
                .iter()
                .rev()
                .find(|record| record.command_id == command.command_id)
                .map(|record| record.status)
                .ok_or_else(|| "Strategy 幂等命令缺少审计记录".to_string())?;
            match status {
                qx_control::CommandStatus::Accepted => {
                    command_queue
                        .enqueue_command(command.clone(), lease_now)
                        .map_err(|error| {
                            format!("Strategy 幂等 SubmitOrder 入队失败: {error:?}")
                        })?;
                    Ok("ORDER_INTENT_ALREADY_ACCEPTED".into())
                }
                qx_control::CommandStatus::Executed => Ok("ORDER_INTENT_ALREADY_EXECUTED".into()),
                qx_control::CommandStatus::Failed => {
                    Err("Strategy 原 OrderIntent 已执行失败".into())
                }
                qx_control::CommandStatus::Rejected => Err("Strategy 原 OrderIntent 已拒绝".into()),
            }
        }
        Err(error) => Err(format!("Strategy SubmitOrder 被控制面拒绝: {error:?}")),
    }
}

pub(crate) fn command_is_final(control: &ControlPlane, command_id: u64) -> bool {
    control
        .audit()
        .iter()
        .rev()
        .find(|record| record.command_id == command_id)
        .is_some_and(|record| {
            matches!(
                record.status,
                qx_control::CommandStatus::Rejected
                    | qx_control::CommandStatus::Executed
                    | qx_control::CommandStatus::Failed
            )
        })
}
