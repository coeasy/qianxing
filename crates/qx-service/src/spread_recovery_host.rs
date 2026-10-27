//! Service-owned multi-leg spread recovery and Paper hedge execution.

use crate::backtest_bindings::ExecutionCostBinding;
use crate::configured_backends::ControlStateBackend;
use qx_core::{Order, TradingInstrumentSpec};
use qx_execution::{
    ingest_venue_events, ingest_venue_events_with_spec, HedgeOrderValidator, HedgeRecoveryWorker,
    VenuePortAdapter,
};
use qx_runtime::{LiveEventPipeline, RuntimeConfig, WorkerConfig, WorkerRole};
use qx_zhenlu::{
    FileSpreadOrderGroupStore, PaperVenue, SpreadOrderGroupStatus, SpreadOrderGroupStore, Venue,
};
use std::collections::BTreeMap;
use std::path::Path;

use qx_control::{order_from_submit_command, ControlCommand, ControlPlane, Permission};
use qx_storage::ControlCommandQueueBackend;
use qx_zhenlu::{SpreadOrderGroup, SpreadOrderLeg};

fn runtime_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// 在多腿命令进入队列前持久化完整订单组。组快照不是成交事实，事实仍由
/// 各 Venue EventLog 写入；它只提供跨 worker 的腿身份、路由和重启恢复索引。
pub fn persist_strategy_spread_group(
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
pub fn open_spread_group_store(root: &Path) -> Result<FileSpreadOrderGroupStore, String> {
    FileSpreadOrderGroupStore::new(root.join("spread-groups"))
}

/// 提交入口是否以 **fail-closed** 被拒绝（网关内的屏障、风控或配置缺失判定）。
///
/// 网关用 `FAIL_CLOSED:` 前缀标注"在写入任何事实之前就拒绝"，CLI 据此分类：
/// 这类提交没有腿订单事实可归约、也没有敞口需要补偿，因此组快照同步与恢复扫描
/// 都必须跳过，否则后续步骤会用"缺少 EventLog 订单"覆盖掉真正的原因文案。
pub fn is_fail_closed_rejection(result: &Result<String, String>) -> bool {
    result
        .as_ref()
        .err()
        .is_some_and(|error| error.starts_with("FAIL_CLOSED:"))
}

/// A sibling leg waits while another persisted submit intent is unresolved. Queue workers
/// leave this command pending so it can retry after the first leg is synchronized.
pub fn is_retryable_spread_submission(result: &Result<String, String>) -> bool {
    result
        .as_ref()
        .err()
        .is_some_and(|error| error.starts_with("RETRYABLE:"))
}

/// 将某条执行 worker 已写入 EventLog 的订单状态归约到多腿组快照。
/// 只依据本地 OMS 事实更新；存储在同一文件锁内执行读、归约、写，防止不同
/// Venue worker 同时归约不同腿时互相覆盖。精确数量和报价预算字段直接继承 OMS 快照。
pub fn sync_spread_group_after_order(
    root: &Path,
    pipeline: &LiveEventPipeline,
    command: &ControlCommand,
    _now: u64,
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
    store.apply_order_snapshot(group_id, &current)
}

/// Recover the group projection from EventLog after a worker exits between persisting venue
/// facts and synchronizing the cross-account group snapshot. Only intents owned by this account
/// and venue are considered; an absent order is left fenced for its original command replay.
pub fn sync_in_flight_spread_orders(
    root: &Path,
    pipeline: &LiveEventPipeline,
    group_id: Option<&str>,
    account_id: Option<&str>,
    venue_id: Option<&str>,
) -> Result<usize, String> {
    let Some(group_id) = group_id else {
        return Ok(0);
    };
    let snapshots = pipeline.orders();
    let mut store = FileSpreadOrderGroupStore::new(root.join("spread-groups"))?;
    let mut synchronized = 0_usize;
    let Some(group) = store.load(group_id)? else {
        // The execution gateway owns the canonical fail-closed classification for missing
        // groups; this opportunistic crash-recovery pass must not mask it.
        return Ok(0);
    };
    let Some(client_order_id) = group.submission_in_flight_order_id else {
        return Ok(0);
    };
    let Some(leg) = group
        .legs
        .iter()
        .find(|leg| leg.order.client_id == client_order_id)
    else {
        return Err(format!(
            "多腿提交意图 {group_id}/{client_order_id} 没有对应腿"
        ));
    };
    if account_id.is_some_and(|account| leg.order.account_id != account)
        || venue_id.is_some_and(|venue| !leg.venue_id.eq_ignore_ascii_case(venue))
    {
        return Ok(0);
    }
    if let Some(snapshot) = snapshots
        .iter()
        .find(|snapshot| snapshot.client_id == client_order_id)
    {
        store.apply_order_snapshot(group_id, snapshot)?;
        synchronized = synchronized.saturating_add(1);
    }
    Ok(synchronized)
}

/// Synchronize any persisted spread group containing a CancelOrder target after the cancellation
/// fact has been reduced into the account EventLog. Membership is resolved from the stored group,
/// never from caller-supplied group metadata.
pub fn sync_spread_groups_after_cancel(
    root: &Path,
    pipeline: &LiveEventPipeline,
    client_order_id: u64,
) -> Result<(), String> {
    let current = pipeline
        .orders()
        .into_iter()
        .find(|order| order.client_id == client_order_id)
        .ok_or_else(|| format!("撤单后 EventLog 缺少订单 {client_order_id}"))?;
    let mut store = FileSpreadOrderGroupStore::new(root.join("spread-groups"))?;
    let mut matched = None;
    for group_id in store.group_ids()? {
        let Some(group) = store.load(&group_id)? else {
            continue;
        };
        if group
            .legs
            .iter()
            .any(|leg| leg.order.client_id == client_order_id)
        {
            if matched.replace(group_id.clone()).is_some() {
                return Err(format!(
                    "订单 {client_order_id} 同时属于多个多腿组，拒绝推测归属"
                ));
            }
        }
    }
    if let Some(group_id) = matched {
        store.apply_order_snapshot(&group_id, &current)?;
    }
    Ok(())
}

pub struct SpreadRecoveryContext<'a> {
    pub root: &'a Path,
    pub account_id: Option<&'a str>,
    pub venue_id: &'a str,
    pub accept_any_venue: bool,
    pub order_validator: Option<&'a dyn HedgeOrderValidator>,
    pub pipeline: &'a mut LiveEventPipeline,
    pub worker_id: &'a str,
    pub now: u64,
    pub source_seq: &'a mut u64,
}

/// 扫描单机多腿恢复快照，并在当前执行 Venue 上处理已确认的风险敞口。
///
/// 该函数只处理 `HedgeRequired`，永远跳过 `ReconcileRequired`；后者必须先
/// 由用户流/对账 worker 得到确定的远端事实。补偿订单仍由
/// `HedgeRecoveryWorker` 生成确定性 client_order_id，并通过同一 EventLog
/// 事实端口归约，因而执行 worker 重启不会重复提交。
pub fn recover_spread_groups_for_venue<V: Venue>(
    context: SpreadRecoveryContext<'_>,
    venue: V,
) -> Result<(V, Vec<String>), String> {
    let SpreadRecoveryContext {
        root,
        account_id,
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
                    && (group.all_confirmed_exposure_hedged()
                        || group.compensation_targets().iter().any(|target| {
                            account_id.is_none_or(|account| target.account_id == account)
                                && (venue_id.trim().is_empty()
                                    || target.venue_id.eq_ignore_ascii_case(venue_id))
                        })) =>
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
    if !accept_any_venue && account_id.is_none_or(|account| account.trim().is_empty()) {
        return Err("live 多腿恢复缺少 account_id，拒绝使用错误账户处理补偿订单".into());
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
        let worker = HedgeRecoveryWorker::new(
            group,
            &mut store,
            &mut router,
            pipeline,
            worker_id,
            now,
            source_seq,
        )
        .map_err(|error| format!("构造多腿恢复 worker 失败: {error:?}"))?;
        let worker = match account_id {
            Some(account_id) if venue_id.trim().is_empty() => worker
                .for_account(account_id)
                .map_err(|error| format!("绑定多腿恢复账户失败: {error}"))?,
            Some(account_id) => worker
                .for_account_venue(account_id, venue_id)
                .map_err(|error| format!("绑定多腿恢复账户/交易所失败: {error}"))?,
            None => worker,
        };
        let outcome = worker.execute_with_validator(order_validator)?;
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

/// 待恢复的多腿分组数：`HedgeRequired` 且本 venue 至少有一条腿已成交。
///
/// 两条恢复循环要的都不是"有没有"而是"这一轮扫描之后少了没有"，所以这里给的是计数：
/// 扫描前后各数一次，分组没减少就计一轮原地不动（V13 R2 #201 让第五处内联扫描也这么记）。
pub fn pending_spread_recovery_groups(root: &Path, venue_id: &str) -> Result<usize, String> {
    pending_spread_recovery_groups_inner(root, venue_id, None)
}

pub fn pending_spread_recovery_groups_for_account(
    root: &Path,
    venue_id: &str,
    account_id: Option<&str>,
) -> Result<usize, String> {
    if account_id.is_none() {
        return pending_spread_recovery_groups(root, venue_id);
    }
    pending_spread_recovery_groups_inner(root, venue_id, account_id)
}

fn pending_spread_recovery_groups_inner(
    root: &Path,
    venue_id: &str,
    account_id: Option<&str>,
) -> Result<usize, String> {
    let store = FileSpreadOrderGroupStore::new(root.join("spread-groups"))?;
    let mut pending = 0_usize;
    for group_id in store.group_ids()? {
        let Some(group) = store.load(&group_id)? else {
            continue;
        };
        if group.status != SpreadOrderGroupStatus::HedgeRequired {
            continue;
        }
        if group.all_confirmed_exposure_hedged()
            || group.compensation_targets().iter().any(|target| {
                account_id.is_none_or(|account| target.account_id == account)
                    && (venue_id.trim().is_empty()
                        || target.venue_id.eq_ignore_ascii_case(venue_id))
            })
        {
            pending = pending.saturating_add(1);
        }
    }
    Ok(pending)
}

/// 一轮多腿恢复扫描之后隔多久再看（V13 R2 #168c）。
///
/// `stalls` 是"连续多少轮扫描一个分组都没推进"。归零时回到 100ms 的忙轮询；原地不动
/// 就按 `qx-core::retry` 那份退避往上爬，封顶 8 秒。这里刻意**不设具名放弃**：停在
/// `HedgeRequired` 的分组是一条腿已成交、另一条还没对冲的裸腿，停止扫描等于把敞口永久
/// 晾着；行情链和用户流可以报错收口，敞口不行。退避公式仍走 `RetryPolicy`，不在这里重算。
pub fn spread_recovery_poll_delay(stalls: u32) -> std::time::Duration {
    const BUSY_POLL: std::time::Duration = std::time::Duration::from_millis(100);
    if stalls == 0 {
        return BUSY_POLL;
    }
    let policy = qx_core::retry::RetryPolicy::new(
        u32::MAX,
        qx_core::retry::Backoff::exponential(
            std::time::Duration::from_millis(500),
            std::time::Duration::from_secs(8),
        ),
    );
    policy.delay_before_attempt(stalls)
}

pub fn dedicated_spread_recovery_configured(
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
pub struct RecoveryGuard<'a> {
    worker: &'a WorkerConfig,
    /// 风控读取的是恢复扫描开始时的不可变快照；本轮补偿产生的事实会在下一轮
    /// 扫描重新加载，避免在同一轮里用半更新状态重复计算风险。
    risk_pipeline: LiveEventPipeline,
    runtime_config_path: Option<&'a Path>,
}

pub fn recovery_order_validator<'a>(
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
        crate::risk_host::require_fresh_live_balance_snapshot(
            &self.worker.id,
            self.worker.account_id.as_deref(),
            self.worker.venue_id.as_deref(),
            &self.risk_pipeline,
            runtime_timestamp_ms(),
        )?;
        if let Some((risk, position)) = crate::risk_host::worker_risk_context(
            self.worker,
            order,
            &self.risk_pipeline,
            None,
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
        crate::worker_market_spec::load_worker_market_spec_for_instrument(
            self.worker,
            &order.instrument,
            self.runtime_config_path,
        )
    }
}

/// Paper 恢复使用同一 `PaperVenue` 复现补偿单的 Accepted→Fill 过程，
/// 但只恢复 `spread-hedge-v1` 订单，避免恢复扫描误撮合普通策略订单。
pub fn recover_paper_spread_groups(
    root: &Path,
    pipeline: &mut LiveEventPipeline,
    worker_id: &str,
    now: u64,
    order_validator: Option<&dyn HedgeOrderValidator>,
    costs: &ExecutionCostBinding,
) -> Result<Vec<String>, String> {
    recover_paper_spread_groups_inner(root, None, pipeline, worker_id, now, order_validator, costs)
}

pub fn recover_paper_spread_groups_for_account(
    root: &Path,
    account_id: Option<&str>,
    pipeline: &mut LiveEventPipeline,
    worker_id: &str,
    now: u64,
    order_validator: Option<&dyn HedgeOrderValidator>,
    costs: &ExecutionCostBinding,
) -> Result<Vec<String>, String> {
    if account_id.is_none() {
        return recover_paper_spread_groups(root, pipeline, worker_id, now, order_validator, costs);
    }
    recover_paper_spread_groups_inner(
        root,
        account_id,
        pipeline,
        worker_id,
        now,
        order_validator,
        costs,
    )
}

fn recover_paper_spread_groups_inner(
    root: &Path,
    account_id: Option<&str>,
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
    let mut exact_spec_orders = hedge_orders
        .iter()
        .filter(|order| order.exact_amounts.is_some() && !order.status.is_terminal())
        .map(|order| (order.instrument.clone(), order.clone()))
        .collect::<BTreeMap<_, _>>();
    let group_store = FileSpreadOrderGroupStore::new(root.join("spread-groups"))?;
    for group_id in group_store.group_ids()? {
        let Some(group) = group_store.load(&group_id)? else {
            continue;
        };
        if group.status != SpreadOrderGroupStatus::HedgeRequired {
            continue;
        }
        for target in group.compensation_targets() {
            if target.qty_exact.is_none()
                || account_id.is_some_and(|account| account != target.account_id)
            {
                continue;
            }
            let source_leg = group
                .leg(&target.leg_id)
                .map_err(|error| format!("读取精确补偿来源腿失败: {error:?}"))?;
            exact_spec_orders
                .entry(source_leg.order.instrument.clone())
                .or_insert_with(|| source_leg.order.clone());
        }
    }
    if !exact_spec_orders.is_empty() {
        let validator = order_validator.ok_or_else(|| {
            "FAIL_CLOSED: Paper exact spread recovery 缺少冻结规格解析器".to_string()
        })?;
        for (instrument, order) in exact_spec_orders {
            let spec = validator.instrument_spec(&order)?.ok_or_else(|| {
                format!(
                    "FAIL_CLOSED: Paper exact spread recovery 缺少 {} 的冻结 market spec",
                    instrument
                )
            })?;
            if spec.instrument != instrument {
                return Err(format!(
                    "FAIL_CLOSED: Paper spread market spec 标的不匹配: expected={} actual={}",
                    instrument, spec.instrument
                ));
            }
            venue
                .set_instrument_spec(spec)
                .map_err(|error| format!("Paper spread 冻结 market spec 配置失败: {error:?}"))?;
        }
    }
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
            account_id,
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
        if let Some(spec) = spec.as_ref() {
            venue
                .set_instrument_spec(spec.clone())
                .map_err(|error| format!("Paper spread 冻结 market spec 配置失败: {error:?}"))?;
        }
        let events = venue
            .try_on_quote(&instrument, quote)
            .map_err(|error| format!("Paper spread 精确行情撮合失败: {error:?}"))?;
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
            account_id,
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

pub fn persist_strategy_submit(
    control_store: &ControlStateBackend,
    command_queue: &dyn ControlCommandQueueBackend,
    command: &ControlCommand,
    now: u64,
) -> Result<String, String> {
    // 控制面审计戳留在毫秒域，命令队列信封的 enqueued_ts 属于租约域。
    let lease_now = qx_runtime::lease_clock(now);
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
            }
        }
        Err(error) => Err(format!("Strategy SubmitOrder 被控制面拒绝: {error:?}")),
    }
}

pub fn command_is_final(control: &ControlPlane, command_id: u64) -> bool {
    control
        .audit()
        .iter()
        .rev()
        .find(|record| record.command_id == command_id)
        .is_some_and(|record| record.status.is_final())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{
        ExactDecimal, ExactFillAmounts, ExactOrderAmounts, ExactTradingRules, Fill, InstrumentId,
        OrderStatus, Price, Quantity, Side, TradingInstrumentSpec,
    };
    use qx_execution::HedgeOrderValidator;
    use qx_runtime::{RuntimeEventEnvelope, RuntimeExternalEvent};
    use qx_zhenlu::{SpreadOrderGroup, SpreadOrderLeg};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct ExactSpecValidator(TradingInstrumentSpec);

    impl HedgeOrderValidator for ExactSpecValidator {
        fn validate(&self, _order: &Order) -> Result<(), String> {
            Ok(())
        }

        fn instrument_spec(&self, order: &Order) -> Result<Option<TradingInstrumentSpec>, String> {
            (order.instrument == self.0.instrument)
                .then(|| self.0.clone())
                .map(Some)
                .ok_or_else(|| format!("test spec is not configured for {}", order.instrument))
        }
    }

    #[test]
    fn paper_recovery_matches_sub_nano_exact_compensation_with_frozen_spec() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qx-service-paper-exact-recovery-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let instrument = InstrumentId::parse("TINY/USDT.OKX").unwrap();
        let mut spec = TradingInstrumentSpec::spot_default(&instrument, "USDT");
        spec.base_currency = "TINY".into();
        spec.exact_rules = Some(ExactTradingRules {
            contract_size: ExactDecimal::ONE,
            price_tick: ExactDecimal::parse("0.01").unwrap(),
            quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        });
        spec.validate().unwrap();
        let quantity = ExactDecimal::parse("0.0000000002").unwrap();
        let mut source_order = Order {
            client_id: 4_001,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::ZERO,
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "account-exact".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(ExactOrderAmounts {
                quantity,
                limit_price: None,
                filled: ExactDecimal::ZERO,
                market_reference_price: Some(ExactDecimal::parse("100").unwrap()),
                quote_budget: None,
                quote_filled: ExactDecimal::ZERO,
            }),
        };
        let other_order = Order {
            client_id: 4_002,
            instrument: InstrumentId::parse("OTHER/USDT.OKX").unwrap(),
            side: Side::Sell,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "account-exact".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        let source_fill = Fill {
            order_id: source_order.client_id,
            ts: 3,
            exact_amounts: Some(ExactFillAmounts {
                quantity: ExactDecimal::parse("0.0000000001").unwrap(),
                price: ExactDecimal::parse("100").unwrap(),
                quote_cost: Some(ExactDecimal::parse("0.00000001").unwrap()),
                quote_budget_complete: false,
            }),
            ..Fill::default()
        };
        let mut group = SpreadOrderGroup::new(
            "paper-exact-recovery",
            "exact-recovery-strategy",
            vec![
                SpreadOrderLeg {
                    leg_id: "exact-leg".into(),
                    venue_id: "OKX".into(),
                    order: source_order.clone(),
                },
                SpreadOrderLeg {
                    leg_id: "empty-leg".into(),
                    venue_id: "OKX".into(),
                    order: other_order,
                },
            ],
        )
        .unwrap();
        group.begin_submission().unwrap();
        group.record_accepted("exact-leg").unwrap();
        group.record_accepted("empty-leg").unwrap();
        group.record_fill("exact-leg", &source_fill).unwrap();
        group.record_cancelled("exact-leg").unwrap();
        group.record_cancelled("empty-leg").unwrap();
        assert_eq!(group.status, SpreadOrderGroupStatus::HedgeRequired);

        let mut pipeline = LiveEventPipeline::open(&root, "paper-exact-recovery", "USDT").unwrap();
        pipeline.register_order(source_order.clone(), 1).unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Accepted {
                    client_order_id: source_order.client_id,
                    venue_order_id: Some("source-order".into()),
                },
                2,
                2,
                1,
                "exact-recovery:source:accepted",
            ))
            .unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::FillWithSpec {
                    fill: Box::new(source_fill),
                    spec: Box::new(spec.clone()),
                },
                3,
                3,
                2,
                "exact-recovery:source:fill",
            ))
            .unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Cancelled {
                    client_order_id: source_order.client_id,
                },
                4,
                4,
                3,
                "exact-recovery:source:cancelled",
            ))
            .unwrap();
        source_order.status = OrderStatus::Cancelled;
        source_order.exact_amounts.as_mut().unwrap().filled =
            ExactDecimal::parse("0.0000000001").unwrap();
        let mut group_store = FileSpreadOrderGroupStore::new(root.join("spread-groups")).unwrap();
        group_store.save(&group).unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                instrument.clone(),
                qx_guanxing::QuoteTick::new(
                    5,
                    Price::from_i64(99),
                    Quantity::ZERO,
                    Price::from_i64(100),
                    Quantity::ZERO,
                    4,
                )
                .with_exact_prices(
                    ExactDecimal::parse("99").unwrap(),
                    ExactDecimal::parse("100").unwrap(),
                )
                .with_exact_sizes(
                    ExactDecimal::parse("0.0000000002").unwrap(),
                    ExactDecimal::parse("0.0000000002").unwrap(),
                ),
                5,
                4,
                "exact-recovery:market",
            ))
            .unwrap();

        let validator = ExactSpecValidator(spec);
        let diagnostics = recover_paper_spread_groups_for_account(
            &root,
            Some("account-exact"),
            &mut pipeline,
            "paper-exact-recovery",
            6,
            Some(&validator),
            &ExecutionCostBinding::builtin_default(),
        )
        .unwrap();

        let restored = group_store.load("paper-exact-recovery").unwrap().unwrap();
        assert_eq!(
            restored.status,
            SpreadOrderGroupStatus::Hedged,
            "recovery diagnostics: {diagnostics:?}"
        );
        let compensation = pipeline
            .orders()
            .into_iter()
            .find(|order| {
                order
                    .trace
                    .as_ref()
                    .and_then(|trace| trace.rule_version.as_deref())
                    == Some("spread-hedge-v1")
            })
            .expect("exact compensation order should be in EventLog");
        assert_eq!(compensation.status, OrderStatus::Filled);
        assert_eq!(
            compensation.exact_amounts.unwrap().filled,
            ExactDecimal::parse("0.0000000001").unwrap()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn paper_recovery_scan_closes_a_fully_hedged_group_after_restart() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qx-service-spread-finalize-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let mut first = Order {
            client_id: 901,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(2),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "account-binance".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        let mut second = first.clone();
        second.client_id = 902;
        second.instrument = InstrumentId::parse("ETHUSDT.OKX").unwrap();
        second.side = Side::Sell;
        second.account_id = "account-okx".into();
        let mut group = SpreadOrderGroup::new(
            "restart-finalize",
            "basis-restart-finalize",
            vec![
                SpreadOrderLeg {
                    leg_id: "binance-leg".into(),
                    venue_id: "BINANCE".into(),
                    order: first.clone(),
                },
                SpreadOrderLeg {
                    leg_id: "okx-leg".into(),
                    venue_id: "OKX".into(),
                    order: second.clone(),
                },
            ],
        )
        .unwrap();
        group.begin_submission().unwrap();
        for (leg_id, order) in [("binance-leg", &mut first), ("okx-leg", &mut second)] {
            group.record_accepted(leg_id).unwrap();
            group
                .record_fill(
                    leg_id,
                    &qx_core::Fill {
                        order_id: order.client_id,
                        qty: Quantity::from_i64(1),
                        price: Price::from_i64(100),
                        ..qx_core::Fill::default()
                    },
                )
                .unwrap();
            group.record_cancelled(leg_id).unwrap();
            group
                .record_compensation_fill(leg_id, Quantity::from_i64(1))
                .unwrap();
        }
        assert_eq!(group.status, SpreadOrderGroupStatus::HedgeRequired);
        assert!(group.compensation_targets().is_empty());

        let mut store = FileSpreadOrderGroupStore::new(root.join("spread-groups")).unwrap();
        store.save(&group).unwrap();
        assert_eq!(
            pending_spread_recovery_groups_for_account(&root, "BINANCE", Some("account-binance"))
                .unwrap(),
            1,
            "scanner must select persisted completion that still needs a status commit"
        );

        let mut pipeline = LiveEventPipeline::open(&root, "paper-recovery", "USDT").unwrap();
        recover_paper_spread_groups_for_account(
            &root,
            Some("account-binance"),
            &mut pipeline,
            "paper-recovery",
            1_000,
            None,
            &ExecutionCostBinding::builtin_default(),
        )
        .unwrap();
        assert_eq!(
            store.load("restart-finalize").unwrap().unwrap().status,
            SpreadOrderGroupStatus::Hedged
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn in_flight_spread_replay_syncs_only_the_matching_account_snapshot() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qx-service-spread-inflight-sync-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let first = Order {
            client_id: 9_101,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "account-binance".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        let mut second = first.clone();
        second.client_id = 9_102;
        second.instrument = InstrumentId::parse("ETHUSDT.OKX").unwrap();
        second.account_id = "account-okx".into();
        let mut group = SpreadOrderGroup::new(
            "spread-inflight-replay",
            "replay-test",
            vec![
                SpreadOrderLeg {
                    leg_id: "binance".into(),
                    venue_id: "BINANCE".into(),
                    order: first.clone(),
                },
                SpreadOrderLeg {
                    leg_id: "okx".into(),
                    venue_id: "OKX".into(),
                    order: second,
                },
            ],
        )
        .unwrap();
        group.submission_in_flight_order_id = Some(first.client_id);
        let mut store = FileSpreadOrderGroupStore::new(root.join("spread-groups")).unwrap();
        store.save(&group).unwrap();

        let mut pipeline = LiveEventPipeline::open(&root, "account-binance-log", "USDT").unwrap();
        pipeline.register_order(first.clone(), 1).unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Accepted {
                    client_order_id: first.client_id,
                    venue_order_id: Some("binance-remote-9101".into()),
                },
                2,
                2,
                1,
                "spread-inflight-replay:accepted",
            ))
            .unwrap();

        assert_eq!(
            sync_in_flight_spread_orders(
                &root,
                &pipeline,
                Some("spread-inflight-replay"),
                Some("account-binance"),
                Some("BINANCE"),
            )
            .unwrap(),
            1
        );
        let synced = store.load("spread-inflight-replay").unwrap().unwrap();
        assert_eq!(synced.submission_in_flight_order_id, None);
        assert_eq!(synced.legs[0].order.status, OrderStatus::Accepted);

        group.submission_in_flight_order_id = Some(first.client_id);
        store.save(&group).unwrap();
        assert_eq!(
            sync_in_flight_spread_orders(
                &root,
                &pipeline,
                Some("spread-inflight-replay"),
                Some("account-okx"),
                Some("OKX"),
            )
            .unwrap(),
            0,
            "an account worker must not infer facts from a different account EventLog"
        );
        assert_eq!(
            store
                .load("spread-inflight-replay")
                .unwrap()
                .unwrap()
                .submission_in_flight_order_id,
            Some(first.client_id)
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
