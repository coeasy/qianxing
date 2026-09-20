//! 多腿（spread）订单组持久化与恢复补偿边界。
//!
//! 补偿腿必须重新通过与主腿相同的风控预检，费用模型也与主腿同源。

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

/// 将某条执行 worker 已写入 EventLog 的订单状态归约到多腿组快照。
/// 只依据本地事实更新，不根据命令成功返回值臆造成交；部分成交、取消和未知
/// 状态分别进入 SpreadOrderGroup 的补偿/对账状态机。
pub(crate) fn sync_spread_group_after_order(
    root: &Path,
    pipeline: &LiveEventPipeline,
    command: &ControlCommand,
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
    // 成交必须逐笔取自 EventLog 事实，不能由订单聚合状态反推：反推出来的价格和
    // 费用只能是占位值（市价单根本没有成交价），订单组一旦按名义额推算对冲数量
    // 就会静默失真。快照里已计入的部分按整笔成交跳过。
    let mut already_booked = recorded_filled;
    for fill in pipeline
        .log()
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            qx_core::EventKind::Filled { fill } if fill.order_id == current.client_id => Some(fill),
            _ => None,
        })
    {
        if already_booked >= fill.qty.raw() {
            already_booked -= fill.qty.raw();
            continue;
        }
        if already_booked > 0 {
            return Err(format!(
                "多腿订单组 {group_id} 的成交快照无法与 EventLog 逐笔对齐"
            ));
        }
        group
            .record_fill(&leg_id, fill)
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

pub(crate) fn recovery_order_validator<'a>(
    worker: &'a WorkerConfig,
    pipeline: &LiveEventPipeline,
    runtime_config_path: Option<&'a Path>,
) -> impl Fn(&Order) -> Result<(), String> + 'a {
    // 风控读取的是恢复扫描开始时的不可变快照；本轮补偿产生的事实会在
    // 下一轮扫描重新加载，避免在同一轮里用半更新状态重复计算风险。
    let risk_pipeline = pipeline.clone();
    move |order| {
        let (risk, position) =
            worker_risk_context(worker, order, &risk_pipeline, runtime_config_path)?;
        risk.validate_order(order, &position)
            .map_err(|error| format!("{error:?}"))?;
        Ok(())
    }
}

/// Paper 恢复使用同一 `PaperVenue` 复现补偿单的 Accepted→Fill 过程，
/// 但只恢复 `spread-hedge-v1` 订单，避免恢复扫描误撮合普通策略订单。
/// `fee_model` 必须与该 worker 下单时一致，否则补偿腿的成交成本会比正常腿
/// 低，多腿账本无法对账。
pub(crate) fn recover_paper_spread_groups(
    root: &Path,
    pipeline: &mut LiveEventPipeline,
    worker_id: &str,
    now: u64,
    order_validator: Option<&dyn HedgeOrderValidator>,
    fee_model: Box<dyn FeeModel + Send>,
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
    let mut venue = PaperVenue::new("paper").with_fee_model(fee_model);
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

    let instruments = pipeline
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
        .map(|order| order.instrument)
        .collect::<BTreeSet<_>>();
    for instrument in instruments {
        let Some(quote) = pipeline.latest_quote_with_depth(&instrument) else {
            diagnostics.push(format!(
                "spread_hedge instrument={} pending reason=缺少最新行情",
                instrument
            ));
            continue;
        };
        let events = venue.on_quote(&instrument, quote);
        if !events.is_empty() {
            ingest_venue_events(pipeline, events, worker_id, now, &mut source_seq)
                .map_err(|error| format!("写入 Paper 补偿成交事实失败: {error}"))?;
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
