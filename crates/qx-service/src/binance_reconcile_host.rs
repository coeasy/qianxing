//! Binance order/account reconciliation host.

use crate::exchange_gateway::{ExchangeGatewayPort, GatewayBusinessHealthStatus};
use crate::execution_capability_gate::{
    require_account_query_execution_capability, AccountQueryPurpose, ExecutionConnectorProfile,
};
use crate::market_bridges::venue_balance_fact_identity;
use crate::pipeline::PipelineStorage;
use crate::pipeline_metrics_report::PipelineMetricsReporter;
use crate::worker_host::run_polling_worker;
use qx_adapter::BinanceSpotVenue;
use qx_core::{
    AccountBalance, ExactAccountBalanceAmounts, ExactDecimal, InstrumentId, Money, OrderStatus,
};
use qx_execution::EventLogReconcilePort;
use qx_runtime::{
    account_worker_settlement_currency, required_account_event_log, LiveEventPipeline,
    RuntimeConfig, RuntimeEventEnvelope, RuntimeExternalEvent, ServiceStatus, WorkerConfig,
    WorkerContext,
};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[allow(clippy::too_many_arguments)]
pub fn run_binance_reconcile_worker<F>(
    context: WorkerContext,
    worker: WorkerConfig,
    pipeline_root: PathBuf,
    pipeline_storage: PipelineStorage,
    runtime_config: RuntimeConfig,
    once: bool,
    mut create_venue: F,
) -> Result<(), String>
where
    F: FnMut(&WorkerConfig) -> Result<BinanceSpotVenue, String>,
{
    let reconcile_symbols = resolve_reconcile_symbols(&worker)?;
    let log_name = required_account_event_log(&worker)?;
    let settlement_currency = account_worker_settlement_currency(&runtime_config, &worker)?;
    let mut pipeline = pipeline_storage
        .open(log_name.clone(), settlement_currency)
        .map_err(|error| format!("创建对账事件管线失败: {error}"))?;
    let mut metrics = PipelineMetricsReporter::new(&runtime_config, context.id(), &log_name);
    context.mark(
        ServiceStatus::Ready,
        "reconciler starting",
        Some(runtime_timestamp_ms()),
    )?;
    metrics.publish_if_due(true, runtime_timestamp_ms());
    run_polling_worker(&context, once, || {
        let mut account_query_observed = false;
        let cycle_started = Instant::now();
        let result = (|| -> Result<u64, String> {
            let query_capabilities = require_account_query_execution_capability(
                &worker,
                &runtime_config.environment,
                AccountQueryPurpose::Reconcile,
                ExecutionConnectorProfile::BinanceSpot,
            )?;
            // Rebuild the venue each round so an atomic credentials-file rotation is observed.
            let mut venue = create_venue(&worker)?;
            venue
                .set_reconcile_symbols(reconcile_symbols.iter())
                .map_err(|error| format!("设置 Binance 对账 symbol 失败: {error:?}"))?;
            pipeline
                .refresh()
                .map_err(|error| format!("刷新共享对账 EventLog 失败: {error:?}"))?;
            venue
                .restore_orders(pipeline.orders())
                .map_err(|error| format!("刷新对账订单状态失败: {error:?}"))?;
            venue.reconnect();
            let account_id = worker
                .account_id
                .clone()
                .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))?;
            let venue_id = worker
                .venue_id
                .clone()
                .ok_or_else(|| format!("worker {} 缺少 venue_id", worker.id))?;
            let remote_orders = venue
                .fetch_remote_snapshot()
                .map_err(|error| format!("读取 Binance 订单对账快照失败: {error:?}"))?;
            let received_ts = runtime_timestamp_ms();
            let mut trade_recovery_issues = append_missing_reconciled_fills(
                &mut pipeline,
                &mut venue,
                &worker.id,
                &remote_orders,
                received_ts,
            );
            append_reconciled_order_statuses(
                &mut pipeline,
                &worker.id,
                &venue_id,
                &remote_orders,
                &venue,
                received_ts,
            )?;
            venue
                .refresh_orders(pipeline.orders())
                .map_err(|error| format!("刷新已对账订单状态失败: {error:?}"))?;
            trade_recovery_issues.extend(sync_binance_order_groups(
                &pipeline_storage.root,
                &pipeline,
                &worker,
                &mut venue,
            )?);
            let issues = venue
                .reconcile_remote(&remote_orders)
                .map_err(|error| format!("Binance 订单对账失败: {error:?}"))?;
            let balances = venue
                .fetch_account_balances(runtime_timestamp_ms())
                .map_err(|error| format!("Binance 账户余额同步失败: {error:?}"))?;
            account_query_observed = true;
            metrics.observe_gateway_port_business_status(
                ExchangeGatewayPort::AccountQuery,
                GatewayBusinessHealthStatus::Healthy,
                received_ts,
            );
            let balance_facts = balances
                .iter()
                .map(|balance| AccountBalance {
                    asset: balance.asset.clone(),
                    free: balance.free,
                    locked: balance.locked,
                    borrowed: Money::ZERO,
                    net_cash_balance: None,
                    exact_amounts: Some(ExactAccountBalanceAmounts {
                        free: balance.free_exact,
                        locked: balance.locked_exact,
                        borrowed: ExactDecimal::ZERO,
                        net_cash_balance: None,
                    }),
                })
                .collect::<Vec<_>>();
            let balance_discrepancies = pipeline
                .settlement_balance_discrepancies(&account_id, &venue_id, &balance_facts)
                .map_err(|error| format!("账户余额对账失败: {error:?}"))?;
            crate::reconcile_report::persist_reconcile_report(
                crate::reconcile_report::ReconcileReportInput {
                    pipeline_root: &pipeline_root,
                    worker_id: &worker.id,
                    account_id: &account_id,
                    venue_id: &venue_id,
                    observed_ts: received_ts,
                    issues: &issues,
                    additional_order_issues: &trade_recovery_issues,
                    balances_count: balances.len(),
                    balance_discrepancies: &balance_discrepancies,
                    position_snapshots_count: None,
                    funding_rate_snapshots_count: None,
                    cashflow_count: None,
                },
            )?;
            let (mut source_seq, balance_correlation) =
                venue_balance_fact_identity(&pipeline, &worker.id, received_ts);
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
                    balance_correlation,
                ))
                .map_err(|error| format!("账户余额事实归约失败: {error:?}"))?;
            for issue in &issues {
                EventLogReconcilePort::new(&mut pipeline, &worker.id, received_ts, &mut source_seq)
                    .require_reconcile(issue.client_order_id(), issue.action().reason_code())
                    .map_err(|error| format!("对账事实归约失败: {error}"))?;
            }
            context.heartbeat(received_ts)?;
            let balance_detail = balance_discrepancies
                .iter()
                .map(|discrepancy| {
                    format!(
                        "{}:{}->{}",
                        discrepancy.asset,
                        discrepancy.ledger_raw,
                        discrepancy
                            .venue_raw
                            .map_or_else(|| "未报".to_string(), |raw| raw.to_string())
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            let status = if issues.is_empty()
                && trade_recovery_issues.is_empty()
                && balance_discrepancies.is_empty()
            {
                ServiceStatus::Ready
            } else {
                ServiceStatus::Degraded
            };
            metrics.observe_gateway_port_business_status(
                ExchangeGatewayPort::Reconciliation,
                if status == ServiceStatus::Ready {
                    GatewayBusinessHealthStatus::Healthy
                } else {
                    GatewayBusinessHealthStatus::Degraded
                },
                received_ts,
            );
            context.mark(
            status,
            format!(
                "reconcile_issues={} balances={} balance_discrepancies={} private_query_ops={:?} omitted_optional={:?}{}",
                issues.len(),
                balances.len(),
                balance_discrepancies.len(),
                query_capabilities.allowed_operations,
                query_capabilities.omitted_optional_operations,
                if balance_detail.is_empty() {
                    String::new()
                } else {
                    format!(" [{balance_detail}]")
                }
            ),
            Some(runtime_timestamp_ms()),
        )?;
            metrics.observe(&pipeline);
            metrics.publish_if_due(true, received_ts);
            Ok(30_000)
        })();
        if !context.should_stop() {
            metrics.observe_gateway_port_operation_duration(
                ExchangeGatewayPort::Reconciliation,
                cycle_started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            );
        }
        if result.is_err() {
            metrics.observe_gateway_port_error_category(
                ExchangeGatewayPort::Reconciliation,
                "unknown",
            );
            let now_ms = runtime_timestamp_ms();
            metrics.observe_gateway_port_business_status(
                ExchangeGatewayPort::AccountQuery,
                if account_query_observed {
                    GatewayBusinessHealthStatus::Degraded
                } else {
                    GatewayBusinessHealthStatus::Unhealthy
                },
                now_ms,
            );
            metrics.observe_gateway_port_business_status(
                ExchangeGatewayPort::Reconciliation,
                GatewayBusinessHealthStatus::Unhealthy,
                now_ms,
            );
            metrics.publish(false, now_ms);
        }
        result
    })?;
    metrics.publish(false, runtime_timestamp_ms());
    Ok(())
}

fn sync_binance_order_groups(
    pipeline_root: &std::path::Path,
    pipeline: &LiveEventPipeline,
    worker: &WorkerConfig,
    venue: &mut BinanceSpotVenue,
) -> Result<Vec<serde_json::Value>, String> {
    use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

    let Some(account_id) = worker.account_id.as_deref() else {
        return Err(format!("worker {} 缺少 account_id", worker.id));
    };
    let mut store = FileOrderGroupStore::new(pipeline_root.join("order-groups"))?;
    let orders = pipeline
        .orders()
        .into_iter()
        .map(|order| (order.client_id, order))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut issues = Vec::new();
    for group_id in store.group_ids()? {
        let Some(mut group) = store.load(&group_id)? else {
            continue;
        };
        if group.legs.iter().any(|leg| {
            leg.order.account_id != account_id || !leg.order.instrument.venue.is_binance()
        }) || matches!(
            group.status,
            qx_core::OrderGroupStatus::Planned
                | qx_core::OrderGroupStatus::Filled
                | qx_core::OrderGroupStatus::Cancelled
                | qx_core::OrderGroupStatus::Rejected
        ) {
            continue;
        }
        if group.venue_group_id.is_none() {
            let (venue_group_id, recovered_child_ids) = venue
                .recover_order_group_identity(&group)
                .map_err(|error| {
                    format!("恢复 Binance OCO 列表身份失败 group_id={group_id}: {error:?}")
                })?;
            store.record_venue_group_id(&group_id, venue_group_id)?;
            group = store
                .load(&group_id)?
                .ok_or_else(|| format!("持久化后重新读取订单组失败 group_id={group_id}"))?;
            sync_order_group_snapshots(
                &mut store,
                &group,
                &orders,
                venue,
                Some(&recovered_child_ids),
            )?;
        } else {
            sync_order_group_snapshots(&mut store, &group, &orders, venue, None)?;
        }
        if let Some(current) = store.load(&group_id)? {
            if matches!(
                current.status,
                qx_core::OrderGroupStatus::Submitting
                    | qx_core::OrderGroupStatus::CancelPending
                    | qx_core::OrderGroupStatus::ReconcileRequired
                    | qx_core::OrderGroupStatus::PeerCancelRequired
            ) {
                issues.push(serde_json::json!({
                    "kind": "order_group_pending",
                    "action": "reconcile_required",
                    "group_id": group_id,
                    "status": format!("{:?}", current.status),
                    "reason": "Binance OCO 列表或子腿仍未达到可确认终态",
                }));
            }
        }
    }
    Ok(issues)
}

/// Persist Binance OCO child state from the already-reduced private stream pipeline.
/// This deliberately does not make REST calls or attempt to recover a missing list id;
/// those cases remain behind the normal reconciliation barrier.
pub(crate) fn sync_binance_order_groups_from_user_stream(
    pipeline_root: &std::path::Path,
    pipeline: &LiveEventPipeline,
    worker: &WorkerConfig,
    venue: &BinanceSpotVenue,
) -> Result<(), String> {
    use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

    let Some(account_id) = worker.account_id.as_deref() else {
        return Err(format!("worker {} 缺少 account_id", worker.id));
    };
    let mut store = FileOrderGroupStore::new(pipeline_root.join("order-groups"))?;
    let orders = pipeline
        .orders()
        .into_iter()
        .map(|order| (order.client_id, order))
        .collect::<std::collections::BTreeMap<_, _>>();
    for group_id in store.group_ids()? {
        let Some(group) = store.load(&group_id)? else {
            continue;
        };
        if group.intent.kind != qx_core::OrderGroupKind::Oco
            || group.venue_group_id.is_none()
            || group.legs.iter().any(|leg| {
                leg.order.account_id != account_id || !leg.order.instrument.venue.is_binance()
            })
            || matches!(
                group.status,
                qx_core::OrderGroupStatus::Planned
                    | qx_core::OrderGroupStatus::Filled
                    | qx_core::OrderGroupStatus::Cancelled
                    | qx_core::OrderGroupStatus::Rejected
            )
        {
            continue;
        }
        sync_order_group_snapshots(&mut store, &group, &orders, venue, None)?;
    }
    Ok(())
}

/// Immediately reconcile an OCO after Binance reports a child cancellation. Binance's
/// documented cancel behavior cancels the entire order list when one child is cancelled.
pub(crate) fn reconcile_binance_order_group_after_cancel(
    pipeline_root: &std::path::Path,
    pipeline: &mut LiveEventPipeline,
    worker: &WorkerConfig,
    venue: &mut BinanceSpotVenue,
    cancelled_client_order_id: u64,
    received_ts: u64,
) -> Result<Vec<serde_json::Value>, String> {
    use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

    let mut store = FileOrderGroupStore::new(pipeline_root.join("order-groups"))?;
    let mut matching_group = None;
    for group_id in store.group_ids()? {
        if let Some(group) = store.load(&group_id)? {
            if group.intent.kind == qx_core::OrderGroupKind::Oco
                && group.status != qx_core::OrderGroupStatus::Planned
                && group.legs.iter().any(|leg| {
                    leg.order.client_id == cancelled_client_order_id
                        && leg.order.account_id == worker.account_id.as_deref().unwrap_or_default()
                        && leg.order.instrument.venue.is_binance()
                })
            {
                matching_group = Some(group);
                break;
            }
        }
    }
    let Some(mut group) = matching_group else {
        return Ok(Vec::new());
    };
    let group_id = group.intent.group_id.clone();
    let recovered_child_ids = if group.venue_group_id.is_none() {
        let (venue_group_id, child_ids) =
            venue
                .recover_order_group_identity(&group)
                .map_err(|error| {
                    format!("撤单后恢复 Binance OCO 列表身份失败 group_id={group_id}: {error:?}")
                })?;
        store.record_venue_group_id(&group_id, venue_group_id)?;
        group = store
            .load(&group_id)?
            .ok_or_else(|| format!("撤单后订单组快照消失 group_id={group_id}"))?;
        Some(child_ids)
    } else {
        None
    };
    let group_order_ids = group
        .legs
        .iter()
        .map(|leg| leg.order.client_id)
        .collect::<std::collections::BTreeSet<_>>();

    let symbols = resolve_reconcile_symbols(worker)?;
    venue
        .set_reconcile_symbols(symbols.iter())
        .map_err(|error| format!("设置 Binance OCO 撤单后对账 symbol 失败: {error:?}"))?;
    venue
        .refresh_orders(pipeline.orders())
        .map_err(|error| format!("恢复 Binance OCO 撤单后 connector 状态失败: {error:?}"))?;
    let remote_orders = venue
        .fetch_remote_snapshot()
        .map_err(|error| format!("读取 Binance OCO 撤单后订单快照失败: {error:?}"))?;
    let group_orders = remote_orders
        .into_iter()
        .filter(|snapshot| group_order_ids.contains(&snapshot.client_order_id))
        .collect::<Vec<_>>();
    let mut issues =
        append_missing_reconciled_fills(pipeline, venue, &worker.id, &group_orders, received_ts);
    let venue_id = worker
        .venue_id
        .as_deref()
        .ok_or_else(|| format!("worker {} 缺少 venue_id", worker.id))?;
    append_reconciled_order_statuses(
        pipeline,
        &worker.id,
        venue_id,
        &group_orders,
        venue,
        received_ts,
    )?;
    venue
        .refresh_orders(pipeline.orders())
        .map_err(|error| format!("刷新 Binance OCO 撤单后订单状态失败: {error:?}"))?;
    let orders = pipeline
        .orders()
        .into_iter()
        .map(|order| (order.client_id, order))
        .collect::<std::collections::BTreeMap<_, _>>();
    sync_order_group_snapshots(
        &mut store,
        &group,
        &orders,
        venue,
        recovered_child_ids.as_ref(),
    )?;
    let current_group = store.load(&group_id)?;
    if let Some(current_group) = current_group {
        if matches!(
            current_group.status,
            qx_core::OrderGroupStatus::Active
                | qx_core::OrderGroupStatus::Submitting
                | qx_core::OrderGroupStatus::CancelPending
                | qx_core::OrderGroupStatus::ReconcileRequired
                | qx_core::OrderGroupStatus::PeerCancelRequired
        ) {
            issues.push(serde_json::json!({
                "kind": "order_group_pending",
                "action": "reconcile_required",
                "group_id": group_id,
                "status": format!("{:?}", current_group.status),
                "reason": "Binance OCO 子单撤销后订单组仍未收敛",
            }));
        }
    }
    issues.retain(|issue| {
        issue
            .get("group_id")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|issue_group_id| issue_group_id == group_id)
            && issue
                .get("client_order_id")
                .and_then(serde_json::Value::as_u64)
                .is_none_or(|client_order_id| group_order_ids.contains(&client_order_id))
    });
    Ok(issues)
}

fn sync_order_group_snapshots(
    store: &mut qx_zhenlu::FileOrderGroupStore,
    group: &qx_core::OrderGroupState,
    orders: &std::collections::BTreeMap<u64, qx_core::Order>,
    venue: &BinanceSpotVenue,
    recovered_child_ids: Option<&std::collections::BTreeMap<u64, String>>,
) -> Result<(), String> {
    use qx_zhenlu::OrderGroupStore;

    let snapshots = group
        .legs
        .iter()
        .filter_map(|leg| orders.get(&leg.order.client_id))
        .collect::<Vec<_>>();
    // Resolve a group barrier only from a complete set of fresh, known child facts.
    if group.status == qx_core::OrderGroupStatus::ReconcileRequired
        && (snapshots.len() != group.legs.len()
            || snapshots
                .iter()
                .any(|snapshot| snapshot.status == OrderStatus::Unknown))
    {
        return Ok(());
    }
    for snapshot in snapshots {
        if snapshot.status == OrderStatus::Unknown {
            continue;
        }
        let venue_order_id = venue
            .venue_order_id(snapshot.client_id)
            .map(str::to_string)
            .or_else(|| recovered_child_ids.and_then(|ids| ids.get(&snapshot.client_id).cloned()));
        let current_group = store
            .load(&group.intent.group_id)?
            .ok_or_else(|| format!("同步时订单组消失 group_id={}", group.intent.group_id))?;
        let current_leg = current_group
            .legs
            .iter()
            .find(|leg| leg.order.client_id == snapshot.client_id)
            .ok_or_else(|| "订单组同步快照无法映射到子腿".to_string())?;
        let is_reconciling = current_group.status == qx_core::OrderGroupStatus::ReconcileRequired;
        if !is_reconciling
            && current_leg.order == *snapshot
            && (venue_order_id.is_none()
                || current_leg.venue_order_id.as_deref() == venue_order_id.as_deref())
        {
            continue;
        }
        if is_reconciling {
            store.reconcile_order_snapshot(&group.intent.group_id, snapshot, venue_order_id)?;
        } else {
            store.apply_order_snapshot(&group.intent.group_id, snapshot, venue_order_id)?;
        }
    }
    Ok(())
}

fn append_missing_reconciled_fills(
    pipeline: &mut LiveEventPipeline,
    venue: &mut BinanceSpotVenue,
    worker_id: &str,
    remote_orders: &[qx_zhenlu::VenueOrderSnapshot],
    received_ts: u64,
) -> Vec<serde_json::Value> {
    let mut issues = Vec::new();
    let mut source_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.source_seq)
        .unwrap_or(0);
    for remote in remote_orders {
        let Some(local) = pipeline
            .orders()
            .into_iter()
            .find(|order| order.client_id == remote.client_order_id)
        else {
            continue;
        };
        if local.status.is_terminal() {
            continue;
        }
        let Some(remote_filled) = remote.filled_exact else {
            continue;
        };
        let local_filled = if let Some(amounts) = local.exact_amounts {
            amounts.filled
        } else {
            match ExactDecimal::from_parts(local.filled.raw(), 9) {
                Ok(value) => value,
                Err(error) => {
                    issues.push(serde_json::json!({
                        "kind": "trade_history_recovery_failed",
                        "action": "reconcile_required",
                        "client_order_id": remote.client_order_id,
                        "reason": format!("本地累计成交精度转换失败: {error}"),
                    }));
                    continue;
                }
            }
        };
        if remote_filled
            .checked_cmp(local_filled)
            .is_none_or(|ordering| !ordering.is_gt())
        {
            continue;
        }
        let recorded_trade_ids = match pipeline.fee_trade_ids_for_order(remote.client_order_id) {
            Ok(ids) => ids,
            Err(error) => {
                issues.push(serde_json::json!({
                    "kind": "trade_history_recovery_failed",
                    "action": "reconcile_required",
                    "client_order_id": remote.client_order_id,
                    "reason": error,
                }));
                continue;
            }
        };
        let fills = match venue.fetch_missing_order_fills(
            remote.client_order_id,
            remote_filled,
            &recorded_trade_ids,
        ) {
            Ok(fills) => fills,
            Err(error) => {
                issues.push(serde_json::json!({
                    "kind": "trade_history_recovery_failed",
                    "action": "reconcile_required",
                    "client_order_id": remote.client_order_id,
                    "reason": format!("{error:?}"),
                }));
                continue;
            }
        };
        for (trade_id, fill) in fills {
            source_seq = source_seq.saturating_add(1);
            let trade_ts = fill.ts;
            let result = pipeline.ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Fill { fill },
                trade_ts,
                received_ts,
                source_seq,
                format!(
                    "{worker_id}:binance-trade:{}:{trade_id}",
                    remote.client_order_id
                ),
            ));
            if let Err(error) = result {
                issues.push(serde_json::json!({
                    "kind": "trade_history_recovery_failed",
                    "action": "reconcile_required",
                    "client_order_id": remote.client_order_id,
                    "trade_id": trade_id,
                    "reason": format!("写入成交事实失败: {error:?}"),
                }));
                break;
            }
        }
    }
    issues
}

fn append_reconciled_order_statuses(
    pipeline: &mut LiveEventPipeline,
    worker_id: &str,
    venue_id: &str,
    remote_orders: &[qx_zhenlu::VenueOrderSnapshot],
    venue: &BinanceSpotVenue,
    received_ts: u64,
) -> Result<(), String> {
    let mut seen_client_order_ids = std::collections::BTreeSet::new();
    if remote_orders
        .iter()
        .any(|snapshot| !seen_client_order_ids.insert(snapshot.client_order_id))
    {
        return Err("Binance 订单快照包含重复 client_order_id".into());
    }
    let local_orders = pipeline
        .orders()
        .into_iter()
        .map(|order| (order.client_id, order))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut source_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.source_seq)
        .unwrap_or(0);
    for remote in remote_orders {
        let Some(local) = local_orders.get(&remote.client_order_id) else {
            continue;
        };
        if local.status == remote.status
            || local.status.is_terminal()
            || matches!(
                remote.status,
                OrderStatus::PendingSubmit
                    | OrderStatus::Submitted
                    | OrderStatus::CancelPending
                    | OrderStatus::Unknown
            )
            || local.status.reconciliation_path(remote.status).is_none()
            || !reconciled_fills_match(local, remote)
        {
            continue;
        }
        source_seq = source_seq.saturating_add(1);
        let correlation_id = format!(
            "{worker_id}:reconcile-order:{}:{:?}:{}:{}",
            remote.client_order_id,
            remote.status,
            remote.filled.raw(),
            remote
                .filled_exact
                .map_or_else(String::new, |value| value.to_string())
        );
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::OrderStatusReconciled {
                    client_order_id: remote.client_order_id,
                    account_id: local.account_id.clone(),
                    venue_id: venue_id.to_string(),
                    status: remote.status,
                    venue_order_id: venue
                        .venue_order_id(remote.client_order_id)
                        .map(str::to_string),
                    filled_raw: remote.filled.raw(),
                    filled_exact: remote.filled_exact,
                },
                received_ts,
                received_ts,
                source_seq,
                correlation_id,
            ))
            .map_err(|error| {
                format!(
                    "写入 Binance 订单对账状态失败 client_order_id={}: {error:?}",
                    remote.client_order_id
                )
            })?;
    }
    Ok(())
}

fn reconciled_fills_match(local: &qx_core::Order, remote: &qx_zhenlu::VenueOrderSnapshot) -> bool {
    if let Some(amounts) = local.exact_amounts {
        remote.filled_exact == Some(amounts.filled)
    } else {
        local.filled == remote.filled
            && remote.filled_exact.is_none_or(|remote_exact| {
                ExactDecimal::from_parts(local.filled.raw(), 9).ok() == Some(remote_exact)
            })
    }
}

fn resolve_reconcile_symbols(worker: &WorkerConfig) -> Result<Vec<String>, String> {
    worker
        .symbols
        .iter()
        .map(|symbol| {
            InstrumentId::parse(symbol)
                .filter(|instrument| instrument.venue.is_binance())
                .map(|instrument| instrument.symbol().to_owned())
                .ok_or_else(|| format!("worker {} 对账 symbol 非法: {symbol}", worker.id))
        })
        .collect()
}

fn runtime_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::resolve_reconcile_symbols;
    use qx_runtime::WorkerConfig;

    fn worker(symbol: &str) -> WorkerConfig {
        serde_json::from_value(serde_json::json!({
            "id": "reconciler",
            "role": "reconciler",
            "enabled": true,
            "account_id": "main",
            "venue_id": "binance",
            "symbols": [symbol]
        }))
        .expect("valid reconciliation worker")
    }

    #[test]
    fn reconciliation_symbol_validation_keeps_the_binance_symbol_only() {
        assert_eq!(
            resolve_reconcile_symbols(&worker("BTCUSDT.BINANCE")).unwrap(),
            ["BTCUSDT"]
        );
        assert!(resolve_reconcile_symbols(&worker("BTCUSDT.OKX")).is_err());
    }
}
