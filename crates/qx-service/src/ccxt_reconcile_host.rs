use crate::ccxt_facts::{
    ccxt_balance_facts, ccxt_error_is_optional_derivatives_capability, ccxt_funding_fact,
    ccxt_open_order_issues, ccxt_position_facts, ingest_ccxt_cashflows,
};
use crate::ccxt_reconcile::{
    ccxt_local_order_issue, ccxt_reconcile_round, ccxt_reconcile_service_status,
};
use crate::exchange_gateway::{ExchangeGatewayPort, GatewayBusinessHealthStatus};
use crate::execution_capability_gate::{
    require_account_query_execution_capability, AccountQueryPurpose, ExecutionConnectorProfile,
};
use crate::market_bridges::venue_balance_fact_identity;
use crate::pipeline::PipelineStorage;
use crate::pipeline_metrics_report::PipelineMetricsReporter;
use crate::reconcile_report::{persist_reconcile_report, ReconcileReportInput};
use crate::worker_market_spec::load_worker_market_spec_for_instrument;
use qx_adapter::{CcxtProcessClient, CcxtProcessVenue};
use qx_core::{EventKind, VenueFamily};
use qx_execution::{ingest_venue_events, ingest_venue_events_with_spec, EventLogReconcilePort};
use qx_runtime::{
    account_worker_settlement_currency, required_account_event_log, RuntimeConfig,
    RuntimeEventEnvelope, RuntimeExternalEvent, WorkerConfig, WorkerContext, WorkerRole,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Instant;

/// Persist CCXT private-stream order facts into matching native OCO group snapshots.
/// The stream reducer must append the order facts to EventLog first; this function only
/// derives the group read model from that canonical local state and never calls the venue.
pub(crate) fn sync_ccxt_order_groups_from_user_stream(
    pipeline_root: &std::path::Path,
    pipeline: &qx_runtime::LiveEventPipeline,
    account_id: &str,
    venue_id: &str,
) -> Result<usize, String> {
    use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

    let mut updates = reconcile_order_group_amendment_overlays(pipeline_root, pipeline)?;
    let mut store = FileOrderGroupStore::new(pipeline_root.join("order-groups"))?;
    let orders = pipeline
        .orders()
        .into_iter()
        .map(|order| (order.client_id, order))
        .collect::<BTreeMap<_, _>>();
    for group_id in store.group_ids()? {
        let Some(group) = store.load(&group_id)? else {
            continue;
        };
        if group.intent.kind != qx_core::OrderGroupKind::Oco
            || group.venue_group_id.is_none()
            || group.legs.iter().any(|leg| {
                leg.order.account_id != account_id
                    || !leg
                        .order
                        .instrument
                        .venue
                        .as_str()
                        .eq_ignore_ascii_case(venue_id)
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
        let snapshots = group
            .legs
            .iter()
            .filter_map(|leg| orders.get(&leg.order.client_id))
            .collect::<Vec<_>>();
        // Never partially clear a group barrier from a stream batch. The REST reconciler
        // owns incomplete/unknown child sets after reconnect or a missed private event.
        if group.status == qx_core::OrderGroupStatus::ReconcileRequired
            && (snapshots.len() != group.legs.len()
                || snapshots
                    .iter()
                    .any(|snapshot| snapshot.status == qx_core::OrderStatus::Unknown))
        {
            continue;
        }
        for snapshot in snapshots {
            if snapshot.status == qx_core::OrderStatus::Unknown {
                continue;
            }
            let venue_order_id = pipeline.venue_order_id(snapshot.client_id);
            let current_group = store
                .load(&group_id)?
                .ok_or_else(|| format!("CCXT 用户流同步时订单组消失 group_id={group_id}"))?;
            let current_leg = current_group
                .legs
                .iter()
                .find(|leg| leg.order.client_id == snapshot.client_id)
                .ok_or_else(|| "CCXT 用户流订单快照无法映射到订单组子腿".to_string())?;
            let reconciling = current_group.status == qx_core::OrderGroupStatus::ReconcileRequired;
            if !reconciling
                && current_leg.order == *snapshot
                && (venue_order_id.is_none()
                    || current_leg.venue_order_id.as_deref() == venue_order_id.as_deref())
            {
                continue;
            }
            if reconciling {
                store.reconcile_order_snapshot(&group_id, snapshot, venue_order_id)?;
            } else {
                store.apply_order_snapshot(&group_id, snapshot, venue_order_id)?;
            }
            updates = updates.saturating_add(1);
        }
    }
    Ok(updates)
}

/// Resolve the group-side amendment overlay from the canonical account EventLog.
/// A matching pending fact keeps the group fenced; a confirmed requested snapshot
/// commits the overlay; an unchanged prior snapshot with no pending fact proves the
/// operation stopped before the venue call and safely releases the fence.
fn reconcile_order_group_amendment_overlays(
    pipeline_root: &std::path::Path,
    pipeline: &qx_runtime::LiveEventPipeline,
) -> Result<usize, String> {
    use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

    let mut store = FileOrderGroupStore::new(pipeline_root.join("order-groups"))?;
    let orders = pipeline
        .orders()
        .into_iter()
        .map(|order| (order.client_id, order))
        .collect::<BTreeMap<_, _>>();
    let mut updates = 0_usize;
    for group_id in store.group_ids()? {
        let Some(group) = store.load(&group_id)? else {
            continue;
        };
        for leg in group.legs.iter().filter(|leg| {
            leg.amendment.as_ref().is_some_and(|amendment| {
                amendment.phase == qx_core::OrderGroupAmendmentPhase::Pending
            })
        }) {
            let Some(amendment) = leg.amendment.as_ref() else {
                continue;
            };
            if pipeline.pending_amendment(leg.order.client_id).is_some() {
                continue;
            }
            let Some(snapshot) = orders.get(&leg.order.client_id) else {
                store.require_reconciliation(&group_id)?;
                continue;
            };
            if snapshot.same_submission_identity(&amendment.requested_order) {
                store.confirm_child_amendment(&group_id, &amendment.operation_id, snapshot)?;
                updates = updates.saturating_add(1);
            } else if snapshot.same_submission_identity(&amendment.prior_order) {
                store.abort_child_amendment(&group_id, &amendment.operation_id, snapshot)?;
                updates = updates.saturating_add(1);
            } else {
                store.require_reconciliation(&group_id)?;
            }
        }
    }
    Ok(updates)
}

pub struct CcxtReconcileWorkerInput {
    pub context: WorkerContext,
    pub worker: WorkerConfig,
    pub pipeline_root: PathBuf,
    pub pipeline_storage: PipelineStorage,
    pub runtime_config: RuntimeConfig,
    pub python: String,
    pub ccxt_config_path: String,
    pub runtime_config_path: PathBuf,
    pub once: bool,
}

fn ccxt_reconcile_call(
    client: &mut CcxtProcessClient,
    context: &qx_runtime::WorkerContext,
    request: serde_json::Value,
) -> Result<Option<serde_json::Value>, String> {
    match client.call_cancellable(request, || context.should_stop())? {
        qx_adapter::CcxtCallOutcome::Response(response) => Ok(Some(response)),
        qx_adapter::CcxtCallOutcome::Cancelled => Ok(None),
    }
}

// These reconciliation helpers pass explicit durable state and transport dependencies.
#[allow(clippy::too_many_arguments)]
fn reconcile_ccxt_native_order_groups(
    venue: &mut CcxtProcessVenue,
    worker: &WorkerConfig,
    pipeline_storage: &PipelineStorage,
    runtime_config_path: &std::path::Path,
    pipeline: &mut qx_runtime::LiveEventPipeline,
    now: u64,
    source_seq: &mut u64,
    issues: &mut Vec<serde_json::Value>,
) -> Result<usize, String> {
    use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

    let mut store = FileOrderGroupStore::new(pipeline_storage.root.join("order-groups"))?;
    let mut updates = 0_usize;
    let group_ids = store.group_ids()?;
    for group_id in group_ids {
        let Some(group) = store.load(&group_id)? else {
            continue;
        };
        if group.intent.kind != qx_core::OrderGroupKind::Oco
            || group.legs.iter().any(|leg| {
                leg.order.account_id != worker.account_id.as_deref().unwrap_or_default()
                    || !leg
                        .order
                        .instrument
                        .venue
                        .as_str()
                        .eq_ignore_ascii_case(worker.venue_id.as_deref().unwrap_or_default())
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
        let is_bitget = worker
            .venue_id
            .as_deref()
            .is_some_and(|venue| venue.eq_ignore_ascii_case("BITGET"));
        let client_id = okx_algo_client_order_id(&group.intent.group_id);
        let mut request = serde_json::json!({
            "op": "fetch_order_group",
            "kind": "oco",
            "instrument": group.legs[0].order.instrument.to_string(),
        });
        if is_bitget {
            if let Some(strategy_order_id) = group.venue_group_id.as_deref() {
                request["strategy_order_id"] =
                    serde_json::Value::String(strategy_order_id.to_owned());
            }
        } else if let Some(algo_id) = group.venue_group_id.as_deref() {
            request["algo_id"] = serde_json::Value::String(algo_id.to_owned());
        } else {
            request["algo_client_order_id"] = serde_json::Value::String(client_id.clone());
        }
        let fetched = venue
            .stream_call(request)
            .ok()
            .and_then(|response| response.get("order_group").cloned());
        let Some(remote) = fetched else {
            mark_ccxt_group_query_failed(
                &group, is_bitget, worker, &mut store, pipeline, now, source_seq, issues,
            )?;
            continue;
        };
        if is_bitget {
            updates = updates.saturating_add(reconcile_bitget_uta_oco_observation(
                venue,
                &group,
                &remote,
                worker,
                runtime_config_path,
                &mut store,
                pipeline,
                now,
                source_seq,
                issues,
            )?);
            continue;
        }
        let remote_algo_id = remote
            .get("algo_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let remote_client_id = remote
            .get("algo_client_order_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let instrument = group.legs[0].order.instrument.to_string();
        if remote_algo_id.is_empty()
            || !remote_algo_id.bytes().all(|byte| byte.is_ascii_digit())
            || (!remote_client_id.is_empty() && remote_client_id != client_id)
            || (group.venue_group_id.is_none() && remote_client_id != client_id)
            || remote.get("instrument").and_then(serde_json::Value::as_str)
                != Some(instrument.as_str())
            || remote.get("order_type").and_then(serde_json::Value::as_str) != Some("oco")
            || group
                .venue_group_id
                .as_deref()
                .is_some_and(|known| known != remote_algo_id)
        {
            mark_ccxt_group_unresolved(
                &group,
                worker,
                &mut store,
                pipeline,
                now,
                source_seq,
                "OKX OCO 查询返回的 algo/client/instrument 身份不匹配",
            )?;
            push_ccxt_group_issue(
                issues,
                &group,
                "native_order_group_identity_mismatch",
                "OKX 原生 OCO 查询身份与本地订单组不匹配",
            );
            continue;
        }
        if group.venue_group_id.is_none() {
            store.record_venue_group_id(&group.intent.group_id, remote_algo_id.to_owned())?;
        }
        let observation = match classify_okx_oco_observation(&remote) {
            Ok(observation) => observation,
            Err(reason) => {
                mark_ccxt_group_unresolved(
                    &group, worker, &mut store, pipeline, now, source_seq, reason,
                )?;
                push_ccxt_group_issue(issues, &group, "native_order_group_fact_incomplete", reason);
                continue;
            }
        };
        match observation {
            OkxOcoObservation::Live => {
                if group.status == qx_core::OrderGroupStatus::Submitting {
                    store.activate_native_group(&group.intent.group_id)?;
                } else if group.status == qx_core::OrderGroupStatus::ReconcileRequired {
                    let mut snapshots = Vec::new();
                    let mut complete = true;
                    for leg in &group.legs {
                        let Some(current) = pipeline
                            .orders()
                            .into_iter()
                            .find(|order| order.client_id == leg.order.client_id)
                        else {
                            complete = false;
                            break;
                        };
                        let snapshot = if current.status == qx_core::OrderStatus::Accepted
                            || current.status == qx_core::OrderStatus::Working
                        {
                            current
                        } else if current
                            .status
                            .can_transition_to(qx_core::OrderStatus::Accepted)
                        {
                            *source_seq = (*source_seq).saturating_add(1);
                            pipeline
                                .ingest(qx_runtime::RuntimeEventEnvelope::venue(
                                    qx_runtime::RuntimeExternalEvent::Accepted {
                                        client_order_id: leg.order.client_id,
                                        venue_order_id: None,
                                    },
                                    now,
                                    now,
                                    *source_seq,
                                    format!("{}:okx-oco-live:{}", worker.id, leg.order.client_id),
                                ))
                                .map_err(|error| {
                                    format!("归约 OKX OCO live 子腿事实失败: {error:?}")
                                })?;
                            let Some(snapshot) = pipeline
                                .orders()
                                .into_iter()
                                .find(|order| order.client_id == leg.order.client_id)
                            else {
                                complete = false;
                                break;
                            };
                            updates = updates.saturating_add(1);
                            snapshot
                        } else {
                            mark_ccxt_group_unresolved(
                                &group,
                                worker,
                                &mut store,
                                pipeline,
                                now,
                                source_seq,
                                "已确认 OCO 仍 live，但子腿状态不能收敛到 accepted",
                            )?;
                            complete = false;
                            break;
                        };
                        snapshots.push((snapshot, None));
                    }
                    if complete && snapshots.len() == group.legs.len() {
                        store_ccxt_group_snapshots(
                            &group.intent.group_id,
                            snapshots,
                            true,
                            &mut store,
                        )?;
                    }
                }
            }
            OkxOcoObservation::Cancelled => {
                updates = updates.saturating_add(reconcile_ccxt_cancelled_native_group(
                    &group, worker, &mut store, pipeline, now, source_seq,
                )?);
            }
            OkxOcoObservation::Triggered(order_ids) => {
                updates = updates.saturating_add(reconcile_ccxt_triggered_native_group(
                    &group,
                    &remote,
                    &order_ids,
                    venue,
                    worker,
                    runtime_config_path,
                    &mut store,
                    pipeline,
                    now,
                    source_seq,
                    issues,
                )?);
            }
            OkxOcoObservation::Unresolved(state) => {
                mark_ccxt_group_unresolved(
                    &group,
                    worker,
                    &mut store,
                    pipeline,
                    now,
                    source_seq,
                    &format!("OKX OCO 状态 {state:?} 尚不能安全归约"),
                )?;
                push_ccxt_group_issue(
                    issues,
                    &group,
                    "native_order_group_state_unresolved",
                    &format!("OKX OCO 状态 {state:?} 尚不能安全归约"),
                );
            }
        }
    }
    Ok(updates)
}

#[allow(clippy::too_many_arguments)]
fn mark_ccxt_group_query_failed(
    group: &qx_core::OrderGroupState,
    is_bitget: bool,
    worker: &WorkerConfig,
    store: &mut dyn qx_zhenlu::OrderGroupStore,
    pipeline: &mut qx_runtime::LiveEventPipeline,
    now: u64,
    source_seq: &mut u64,
    issues: &mut Vec<serde_json::Value>,
) -> Result<(), String> {
    let (venue_name, reason) = if is_bitget {
        ("Bitget", "Bitget OCO 策略/子单查询结果未知")
    } else {
        ("OKX", "OKX 原生 OCO 查询失败或响应未知")
    };
    mark_ccxt_group_unresolved(group, worker, store, pipeline, now, source_seq, reason)?;
    push_ccxt_group_issue(
        issues,
        group,
        "native_order_group_query_unknown",
        &format!("{venue_name} 原生 OCO 查询失败或响应未知"),
    );
    Ok(())
}

fn okx_algo_client_order_id(group_id: &str) -> String {
    let mut hash = qx_core::Fnv1a::new();
    hash.write_text(group_id);
    format!("QX{:016X}", hash.finish())
}

#[allow(clippy::too_many_arguments)]
fn reconcile_bitget_uta_oco_observation(
    venue: &mut CcxtProcessVenue,
    group: &qx_core::OrderGroupState,
    remote: &serde_json::Value,
    worker: &WorkerConfig,
    runtime_config_path: &std::path::Path,
    store: &mut dyn qx_zhenlu::OrderGroupStore,
    pipeline: &mut qx_runtime::LiveEventPipeline,
    now: u64,
    source_seq: &mut u64,
    issues: &mut Vec<serde_json::Value>,
) -> Result<usize, String> {
    let remote_id = remote
        .get("strategy_order_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let expected_id = group.venue_group_id.as_deref().unwrap_or_default();
    let instrument = group
        .legs
        .first()
        .map(|leg| leg.order.instrument.to_string())
        .unwrap_or_default();
    let Some(children) = remote
        .get("sub_orders")
        .and_then(serde_json::Value::as_array)
    else {
        mark_ccxt_group_unresolved(
            group,
            worker,
            store,
            pipeline,
            now,
            source_seq,
            "Bitget OCO 查询缺少子单列表",
        )?;
        push_ccxt_group_issue(
            issues,
            group,
            "native_order_group_fact_incomplete",
            "Bitget UTA OCO 查询缺少子单列表",
        );
        return Ok(0);
    };
    if expected_id.is_empty()
        || !expected_id.bytes().all(|byte| byte.is_ascii_digit())
        || remote_id != expected_id
        || remote.get("instrument").and_then(serde_json::Value::as_str) != Some(instrument.as_str())
        || remote.get("order_type").and_then(serde_json::Value::as_str) != Some("oco")
    {
        mark_ccxt_group_unresolved(
            group,
            worker,
            store,
            pipeline,
            now,
            source_seq,
            "Bitget OCO 策略单身份或子单事实与持久订单组不匹配",
        )?;
        push_ccxt_group_issue(
            issues,
            group,
            "native_order_group_identity_mismatch",
            "Bitget UTA OCO 查询身份与本地订单组不匹配",
        );
        return Ok(0);
    }
    match remote.get("state").and_then(serde_json::Value::as_str) {
        Some("live") if children.is_empty() => {
            if group.status == qx_core::OrderGroupStatus::Submitting {
                store.activate_native_group(&group.intent.group_id)?;
                return Ok(1);
            }
            if group.status != qx_core::OrderGroupStatus::ReconcileRequired {
                return Ok(0);
            }
            let mut snapshots = Vec::new();
            for leg in &group.legs {
                let current = pipeline
                    .orders()
                    .into_iter()
                    .find(|order| order.client_id == leg.order.client_id)
                    .ok_or_else(|| {
                        format!("Bitget OCO 子腿 {} 不在 EventLog", leg.order.client_id)
                    })?;
                let snapshot = if matches!(
                    current.status,
                    qx_core::OrderStatus::Accepted | qx_core::OrderStatus::Working
                ) {
                    current
                } else if current
                    .status
                    .can_transition_to(qx_core::OrderStatus::Accepted)
                {
                    *source_seq = source_seq.saturating_add(1);
                    pipeline
                        .ingest(qx_runtime::RuntimeEventEnvelope::venue(
                            qx_runtime::RuntimeExternalEvent::Accepted {
                                client_order_id: leg.order.client_id,
                                venue_order_id: None,
                            },
                            now,
                            now,
                            *source_seq,
                            format!("{}:bitget-oco-live:{}", worker.id, leg.order.client_id),
                        ))
                        .map_err(|error| format!("归约 Bitget OCO live 子腿事实失败: {error:?}"))?;
                    pipeline
                        .orders()
                        .into_iter()
                        .find(|order| order.client_id == leg.order.client_id)
                        .ok_or_else(|| {
                            format!("Bitget OCO live 子腿 {} 归约后缺失", leg.order.client_id)
                        })?
                } else {
                    mark_ccxt_group_unresolved(
                        group,
                        worker,
                        store,
                        pipeline,
                        now,
                        source_seq,
                        "Bitget OCO 仍 live，但本地子腿不能恢复为 accepted",
                    )?;
                    push_ccxt_group_issue(
                        issues,
                        group,
                        "native_order_group_state_unresolved",
                        "Bitget UTA OCO 仍 live，但本地子腿状态无法恢复",
                    );
                    return Ok(0);
                };
                snapshots.push((snapshot, None));
            }
            store_ccxt_group_snapshots(&group.intent.group_id, snapshots, true, store)?;
            Ok(group.legs.len())
        }
        Some("canceled") => {
            if !bitget_cancel_children_are_unfilled(group, children) {
                mark_ccxt_group_unresolved(
                    group,
                    worker,
                    store,
                    pipeline,
                    now,
                    source_seq,
                    "Bitget OCO 已取消，但子单列表不足以证明没有成交",
                )?;
                push_ccxt_group_issue(
                    issues,
                    group,
                    "native_order_group_fact_incomplete",
                    "Bitget UTA OCO 撤销缺少完整的零成交子单事实",
                );
                return Ok(0);
            }
            reconcile_ccxt_cancelled_native_group(group, worker, store, pipeline, now, source_seq)
        }
        Some("rejected") => reconcile_bitget_rejected_oco_group(
            group, children, worker, store, pipeline, now, source_seq, issues,
        ),
        Some("triggered") => reconcile_bitget_triggered_oco_children(
            venue,
            group,
            children,
            worker,
            runtime_config_path,
            store,
            pipeline,
            now,
            source_seq,
            issues,
        ),
        Some(state) => {
            mark_ccxt_group_unresolved(
                group,
                worker,
                store,
                pipeline,
                now,
                source_seq,
                "Bitget OCO 已触发/失败，子单腿角色或成交事实仍需精确归属",
            )?;
            push_ccxt_group_issue(
                issues,
                group,
                "native_order_group_state_unresolved",
                &format!("Bitget UTA OCO 状态 {state:?} 暂不能安全归约"),
            );
            Ok(0)
        }
        None => {
            mark_ccxt_group_unresolved(
                group,
                worker,
                store,
                pipeline,
                now,
                source_seq,
                "Bitget OCO 查询缺少策略状态",
            )?;
            push_ccxt_group_issue(
                issues,
                group,
                "native_order_group_fact_incomplete",
                "Bitget UTA OCO 查询缺少策略状态",
            );
            Ok(0)
        }
    }
}

pub(crate) fn bitget_rejected_oco_children_prove_no_execution(
    children: &[serde_json::Value],
) -> bool {
    if children.is_empty() {
        return true;
    }
    let mut child_ids = std::collections::BTreeSet::new();
    children.iter().all(|child| {
        let id = child
            .get("order_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()));
        id.is_some_and(|id| child_ids.insert(id.to_owned()))
            && child.get("status").and_then(serde_json::Value::as_str) == Some("failed")
            && child
                .get("cum_exec_qty")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| qx_core::ExactDecimal::parse(value).ok())
                .is_some_and(qx_core::ExactDecimal::is_zero)
    })
}

#[allow(clippy::too_many_arguments)]
fn reconcile_bitget_rejected_oco_group(
    group: &qx_core::OrderGroupState,
    children: &[serde_json::Value],
    worker: &WorkerConfig,
    store: &mut dyn qx_zhenlu::OrderGroupStore,
    pipeline: &mut qx_runtime::LiveEventPipeline,
    now: u64,
    source_seq: &mut u64,
    issues: &mut Vec<serde_json::Value>,
) -> Result<usize, String> {
    let local_orders = pipeline.orders();
    let clean_rejection = bitget_rejected_oco_children_prove_no_execution(children)
        && group.legs.iter().all(|leg| {
            leg.venue_order_id.is_none()
                && leg.order.filled.raw() == 0
                && leg
                    .order
                    .exact_amounts
                    .is_none_or(|amounts| amounts.filled.is_zero())
                && local_orders
                    .iter()
                    .find(|order| order.client_id == leg.order.client_id)
                    .is_some_and(|order| {
                        order.status == qx_core::OrderStatus::Rejected
                            || (!order.status.is_terminal()
                                && order
                                    .status
                                    .can_transition_to(qx_core::OrderStatus::Rejected)
                                && order.filled.raw() == 0
                                && order
                                    .exact_amounts
                                    .is_none_or(|amounts| amounts.filled.is_zero()))
                    })
        });
    if !clean_rejection {
        mark_ccxt_group_unresolved(
            group,
            worker,
            store,
            pipeline,
            now,
            source_seq,
            "Bitget OCO 策略单拒绝事实与子单/本地成交状态不一致",
        )?;
        push_ccxt_group_issue(
            issues,
            group,
            "native_order_group_rejection_unresolved",
            "Bitget UTA OCO 被拒绝，但无法证明没有子单成交",
        );
        return Ok(0);
    }
    store.require_reconciliation(&group.intent.group_id)?;
    let mut snapshots = Vec::with_capacity(group.legs.len());
    for leg in &group.legs {
        let current = pipeline
            .orders()
            .into_iter()
            .find(|order| order.client_id == leg.order.client_id)
            .ok_or_else(|| {
                format!(
                    "Bitget OCO 被拒绝的子腿 {} 不在 EventLog",
                    leg.order.client_id
                )
            })?;
        if current.status != qx_core::OrderStatus::Rejected {
            *source_seq = (*source_seq).saturating_add(1);
            pipeline
                .ingest(qx_runtime::RuntimeEventEnvelope::venue(
                    qx_runtime::RuntimeExternalEvent::OrderStatusReconciled {
                        client_order_id: current.client_id,
                        account_id: current.account_id.clone(),
                        venue_id: current.instrument.venue.as_str().to_owned(),
                        status: qx_core::OrderStatus::Rejected,
                        venue_order_id: None,
                        filled_raw: 0,
                        filled_exact: Some(qx_core::ExactDecimal::ZERO),
                    },
                    now,
                    now,
                    *source_seq,
                    format!("{}:bitget-oco-rejected:{}", worker.id, current.client_id),
                ))
                .map_err(|error| format!("归约 Bitget OCO 权威拒绝事实失败: {error:?}"))?;
        }
        let rejected = pipeline
            .orders()
            .into_iter()
            .find(|order| order.client_id == leg.order.client_id)
            .ok_or_else(|| format!("Bitget OCO 拒绝子腿 {} 归约后缺失", leg.order.client_id))?;
        if rejected.status != qx_core::OrderStatus::Rejected
            || rejected.filled.raw() != 0
            || rejected
                .exact_amounts
                .is_some_and(|amounts| !amounts.filled.is_zero())
        {
            mark_ccxt_group_unresolved(
                group,
                worker,
                store,
                pipeline,
                now,
                source_seq,
                "Bitget OCO 拒绝后的 OMS 子腿未收敛为零成交 Rejected",
            )?;
            push_ccxt_group_issue(
                issues,
                group,
                "native_order_group_rejection_unresolved",
                "Bitget UTA OCO 拒绝后本地子腿状态仍不一致",
            );
            return Ok(0);
        }
        snapshots.push((rejected, None));
    }
    store_ccxt_group_snapshots(&group.intent.group_id, snapshots, true, store)?;
    Ok(group.legs.len())
}

pub(crate) fn bitget_oco_child_matches_leg(
    child: &serde_json::Value,
    leg: &qx_core::OrderGroupLegState,
) -> bool {
    let side = match leg.order.side {
        qx_core::Side::Buy => "buy",
        qx_core::Side::Sell => "sell",
    };
    let qty = child
        .get("qty")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| qx_core::ExactDecimal::parse(value).ok());
    if child.get("side").and_then(serde_json::Value::as_str) != Some(side)
        || qty != Some(leg.order.quantity_decimal())
    {
        return false;
    }
    match child.get("order_type").and_then(serde_json::Value::as_str) {
        Some("market") => {
            leg.role == qx_core::OrderGroupLegRole::StopLoss
                && leg
                    .order
                    .exact_amounts
                    .is_some_and(|amounts| amounts.limit_price.is_none())
        }
        Some("limit") => {
            let price = child
                .get("price")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| qx_core::ExactDecimal::parse(value).ok());
            price.is_some()
                && price
                    == leg
                        .order
                        .exact_amounts
                        .and_then(|amounts| amounts.limit_price)
                && (leg.role == qx_core::OrderGroupLegRole::TakeProfit
                    || leg.role == qx_core::OrderGroupLegRole::StopLoss)
        }
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn reconcile_bitget_triggered_oco_children(
    venue: &mut CcxtProcessVenue,
    group: &qx_core::OrderGroupState,
    children: &[serde_json::Value],
    worker: &WorkerConfig,
    runtime_config_path: &std::path::Path,
    store: &mut dyn qx_zhenlu::OrderGroupStore,
    pipeline: &mut qx_runtime::LiveEventPipeline,
    now: u64,
    source_seq: &mut u64,
    issues: &mut Vec<serde_json::Value>,
) -> Result<usize, String> {
    let unresolved = |reason: &str,
                      store: &mut dyn qx_zhenlu::OrderGroupStore,
                      pipeline: &mut qx_runtime::LiveEventPipeline,
                      source_seq: &mut u64,
                      issues: &mut Vec<serde_json::Value>| {
        mark_ccxt_group_unresolved(group, worker, store, pipeline, now, source_seq, reason)?;
        push_ccxt_group_issue(issues, group, "native_order_group_state_unresolved", reason);
        Ok::<(), String>(())
    };

    if children.len() != 2 || group.legs.len() != 2 {
        unresolved(
            "Bitget OCO 触发后必须恰有两条可核验子单",
            store,
            pipeline,
            source_seq,
            issues,
        )?;
        return Ok(0);
    }
    let mut matched = Vec::with_capacity(2);
    let mut seen_ids = std::collections::BTreeSet::new();
    for child in children {
        let id = child
            .get("order_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()));
        let fill = child
            .get("cum_exec_qty")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| qx_core::ExactDecimal::parse(value).ok());
        let qty = child
            .get("qty")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| qx_core::ExactDecimal::parse(value).ok());
        let status = child.get("status").and_then(serde_json::Value::as_str);
        let role_matches = group
            .legs
            .iter()
            .enumerate()
            .filter(|(_, leg)| bitget_oco_child_matches_leg(child, leg))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let child_status_valid = match status {
            Some("filled") => fill == qty && fill.is_some_and(|amount| amount.is_positive()),
            Some("partially_filled") => {
                fill.is_some_and(|amount| amount.is_positive()) && fill != qty
            }
            Some("live" | "new") => fill.is_some_and(qx_core::ExactDecimal::is_zero),
            Some("cancelled" | "canceled") => true,
            _ => false,
        };
        if id.is_none()
            || !seen_ids.insert(id.unwrap_or_default().to_owned())
            || fill.is_none()
            || qty.is_none_or(|quantity| {
                fill.is_some_and(|amount| {
                    amount.checked_cmp(quantity) == Some(std::cmp::Ordering::Greater)
                })
            })
            || !child_status_valid
            || role_matches.len() != 1
        {
            unresolved(
                "Bitget OCO 子单身份、腿角色、状态或成交数量无法唯一核验",
                store,
                pipeline,
                source_seq,
                issues,
            )?;
            return Ok(0);
        }
        let Some(id) = id else {
            unresolved(
                "Bitget OCO 子单缺少订单 ID",
                store,
                pipeline,
                source_seq,
                issues,
            )?;
            return Ok(0);
        };
        matched.push((role_matches[0], child, id.to_owned()));
    }
    if matched
        .iter()
        .map(|(index, _, _)| *index)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != 2
    {
        unresolved(
            "Bitget OCO 子单未能分别对应止盈腿与止损腿",
            store,
            pipeline,
            source_seq,
            issues,
        )?;
        return Ok(0);
    }
    store.require_reconciliation(&group.intent.group_id)?;
    let mut updates = 0;
    let mut snapshots = Vec::with_capacity(2);
    for (leg_index, child, remote_id) in matched {
        let leg = &group.legs[leg_index];
        let local = pipeline
            .orders()
            .into_iter()
            .find(|order| order.client_id == leg.order.client_id)
            .ok_or_else(|| format!("Bitget OCO 子腿 {} 不在 EventLog", leg.order.client_id))?;
        if pipeline
            .venue_order_id(local.client_id)
            .is_some_and(|known| known != remote_id)
        {
            unresolved(
                "Bitget OCO 子单 ID 与 EventLog 中已知身份冲突",
                store,
                pipeline,
                source_seq,
                issues,
            )?;
            return Ok(0);
        }
        if local.status.is_terminal() && pipeline.venue_order_id(local.client_id).is_none() {
            unresolved(
                "Bitget OCO 本地终态子腿缺少远端订单身份",
                store,
                pipeline,
                source_seq,
                issues,
            )?;
            return Ok(0);
        }
        let fees = pipeline
            .fee_baseline_for_order(local.client_id)
            .map_err(|error| format!("读取 Bitget OCO 手续费基线失败: {error}"))?;
        let exact_fees = pipeline
            .exact_fee_baseline_for_order(local.client_id)
            .map_err(|error| format!("读取 Bitget OCO 精确手续费基线失败: {error}"))?;
        let trade_ids = pipeline
            .fee_trade_ids_for_order(local.client_id)
            .map_err(|error| format!("读取 Bitget OCO 成交去重状态失败: {error}"))?;
        venue
            .restore_order_with_exact_fee_state(
                local.clone(),
                remote_id.clone(),
                fees,
                exact_fees,
                trade_ids,
            )
            .map_err(|error| format!("恢复 Bitget OCO 子腿失败: {error:?}"))?;
        let events = venue
            .sync_order(local.client_id, now)
            .map_err(|error| format!("查询 Bitget OCO 子腿精确成交失败: {error:?}"))?;
        let spec = load_worker_market_spec_for_instrument(
            worker,
            &local.instrument,
            Some(runtime_config_path),
        )?;
        updates += if let Some(spec) = spec {
            ingest_venue_events_with_spec(pipeline, events, &worker.id, now, source_seq, &spec)?
        } else {
            ingest_venue_events(pipeline, events, &worker.id, now, source_seq)?
        };
        let reconciled = pipeline
            .orders()
            .into_iter()
            .find(|order| order.client_id == local.client_id)
            .ok_or_else(|| format!("Bitget OCO 子腿 {} 对账后缺失", local.client_id))?;
        let expected_fill = child
            .get("cum_exec_qty")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| qx_core::ExactDecimal::parse(value).ok())
            .ok_or_else(|| "Bitget OCO 子单缺少有效累计成交量".to_string())?;
        let expected_status = match child.get("status").and_then(serde_json::Value::as_str) {
            Some("filled") => qx_core::OrderStatus::Filled,
            Some("partially_filled") => qx_core::OrderStatus::PartiallyFilled,
            Some("live" | "new") => qx_core::OrderStatus::Accepted,
            Some("cancelled" | "canceled") => qx_core::OrderStatus::Cancelled,
            _ => {
                unresolved(
                    "Bitget OCO 子单状态在对账期间变为未知值",
                    store,
                    pipeline,
                    source_seq,
                    issues,
                )?;
                return Ok(updates);
            }
        };
        if reconciled.status != expected_status
            || reconciled.exact_amounts.map(|amounts| amounts.filled) != Some(expected_fill)
        {
            unresolved(
                "Bitget OCO 子单精确成交与 order-info / fills 对账结果不一致",
                store,
                pipeline,
                source_seq,
                issues,
            )?;
            return Ok(updates);
        }
        if pipeline.venue_order_id(local.client_id).is_none()
            || reconciled.status != expected_status
        {
            let filled_raw = expected_fill
                .to_raw_truncated(9)
                .map_err(|error| format!("Bitget OCO 子腿成交量无法投影到历史事件字段: {error}"))?;
            *source_seq = (*source_seq).saturating_add(1);
            pipeline
                .ingest(qx_runtime::RuntimeEventEnvelope::venue(
                    qx_runtime::RuntimeExternalEvent::OrderStatusReconciled {
                        client_order_id: local.client_id,
                        account_id: local.account_id.clone(),
                        venue_id: local.instrument.venue.as_str().to_owned(),
                        status: expected_status,
                        venue_order_id: Some(remote_id.clone()),
                        filled_raw,
                        filled_exact: reconciled.exact_amounts.map(|amounts| amounts.filled),
                    },
                    now,
                    now,
                    *source_seq,
                    format!("{}:bitget-oco-child-id:{}", worker.id, local.client_id),
                ))
                .map_err(|error| format!("持久化 Bitget OCO 子腿远端身份失败: {error:?}"))?;
        }
        let reconciled = pipeline
            .orders()
            .into_iter()
            .find(|order| order.client_id == local.client_id)
            .ok_or_else(|| format!("Bitget OCO 子腿 {} 记录远端身份后缺失", local.client_id))?;
        snapshots.push((reconciled, Some(remote_id)));
    }
    store_ccxt_group_snapshots(&group.intent.group_id, snapshots, true, store)?;
    Ok(updates + group.legs.len())
}

pub(crate) fn bitget_cancel_children_are_unfilled(
    group: &qx_core::OrderGroupState,
    children: &[serde_json::Value],
) -> bool {
    !children.is_empty()
        && children.iter().all(|child| {
            let child_side = child.get("side").and_then(serde_json::Value::as_str);
            let child_qty = child
                .get("qty")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| qx_core::ExactDecimal::parse(value).ok());
            let matches_leg = group.legs.iter().any(|leg| {
                let side = match leg.order.side {
                    qx_core::Side::Buy => "buy",
                    qx_core::Side::Sell => "sell",
                };
                child_side == Some(side) && child_qty == Some(leg.order.quantity_decimal())
            });
            matches!(
                child.get("status").and_then(serde_json::Value::as_str),
                Some("cancelled" | "canceled" | "failed")
            ) && matches_leg
                && child
                    .get("cum_exec_qty")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| qx_core::ExactDecimal::parse(value).ok())
                    .is_some_and(qx_core::ExactDecimal::is_zero)
        })
}

enum OkxOcoObservation {
    Live,
    Cancelled,
    Triggered(Vec<serde_json::Value>),
    Unresolved(String),
}

fn classify_okx_oco_observation(
    remote: &serde_json::Value,
) -> Result<OkxOcoObservation, &'static str> {
    let state = remote
        .get("state")
        .and_then(serde_json::Value::as_str)
        .ok_or("OKX OCO 查询缺少 state")?;
    let order_ids = remote
        .get("order_ids")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .ok_or("OKX OCO 查询缺少 ordIdList")?;
    if order_ids.iter().any(|order_id| {
        !order_id
            .as_str()
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
    }) {
        return Err("OKX OCO 查询 ordIdList 含非法子单 ID");
    }
    let actual_size = remote
        .get("actual_size")
        .and_then(serde_json::Value::as_str)
        .and_then(|text| qx_core::ExactDecimal::parse(text).ok())
        .ok_or("OKX OCO 查询 actualSz 缺失或精度非法")?;
    if actual_size.is_negative() {
        return Err("OKX OCO 查询 actualSz 为负数");
    }
    match state {
        "live" if order_ids.is_empty() && actual_size.is_zero() => Ok(OkxOcoObservation::Live),
        "canceled" if order_ids.is_empty() && actual_size.is_zero() => {
            Ok(OkxOcoObservation::Cancelled)
        }
        "effective" | "partially_effective"
            if order_ids.len() == 1 && actual_size.is_positive() =>
        {
            Ok(OkxOcoObservation::Triggered(order_ids))
        }
        "live" | "canceled" | "effective" | "partially_effective" => {
            Err("OKX OCO state 与 ordIdList/actualSz 不一致")
        }
        _ => Ok(OkxOcoObservation::Unresolved(state.to_owned())),
    }
}

fn push_ccxt_group_issue(
    issues: &mut Vec<serde_json::Value>,
    group: &qx_core::OrderGroupState,
    code: &str,
    message: &str,
) {
    issues.push(serde_json::json!({
        "kind": code,
        "source": "ccxt.reconcile.order_groups",
        "reason": message,
        "observed": true,
        "group_id": group.intent.group_id,
        "venue_group_id": group.venue_group_id,
        "account_id": group.legs.first().map(|leg| leg.order.account_id.as_str()),
        "venue_id": group.legs.first().map(|leg| leg.order.instrument.venue.as_str()),
        "client_order_id": null,
        "venue_id": group.legs.first().map(|leg| leg.order.instrument.venue.as_str()),
        "instrument": group.legs.first().map(|leg| leg.order.instrument.to_string()),
    }));
}

fn store_ccxt_group_snapshots(
    group_id: &str,
    snapshots: Vec<(qx_core::Order, Option<String>)>,
    reconciling: bool,
    store: &mut dyn qx_zhenlu::OrderGroupStore,
) -> Result<(), String> {
    if snapshots.is_empty() {
        return Err("订单组对账没有可持久化的子腿事实".into());
    }
    let final_index = snapshots.len() - 1;
    for (index, (snapshot, remote_id)) in snapshots.into_iter().enumerate() {
        if reconciling && index < final_index {
            store.record_order_snapshot_during_reconciliation(group_id, &snapshot, remote_id)?;
        } else if reconciling {
            store.reconcile_order_snapshot(group_id, &snapshot, remote_id)?;
        } else {
            store.apply_order_snapshot(group_id, &snapshot, remote_id)?;
        }
    }
    Ok(())
}

fn mark_ccxt_group_unresolved(
    group: &qx_core::OrderGroupState,
    worker: &WorkerConfig,
    store: &mut dyn qx_zhenlu::OrderGroupStore,
    pipeline: &mut qx_runtime::LiveEventPipeline,
    now: u64,
    source_seq: &mut u64,
    reason: &str,
) -> Result<(), String> {
    store.require_reconciliation(&group.intent.group_id)?;
    for leg in &group.legs {
        let Some(current) = pipeline
            .orders()
            .into_iter()
            .find(|order| order.client_id == leg.order.client_id)
        else {
            continue;
        };
        if current.status.is_terminal() || current.status == qx_core::OrderStatus::Unknown {
            continue;
        }
        *source_seq = (*source_seq).saturating_add(1);
        pipeline
            .ingest(qx_runtime::RuntimeEventEnvelope::venue(
                qx_runtime::RuntimeExternalEvent::ReconcileRequired {
                    client_order_id: leg.order.client_id,
                },
                now,
                now,
                *source_seq,
                format!(
                    "{}:okx-oco-unknown:{}:{}",
                    worker.id, leg.order.client_id, reason
                ),
            ))
            .map_err(|error| format!("写入 OKX OCO ReconcileRequired 失败: {error:?}"))?;
    }
    let snapshots = pipeline.orders();
    for leg in &group.legs {
        if let Some(snapshot) = snapshots
            .iter()
            .find(|order| order.client_id == leg.order.client_id)
        {
            if snapshot.status == qx_core::OrderStatus::Unknown {
                store.record_order_snapshot_during_reconciliation(
                    &group.intent.group_id,
                    snapshot,
                    None,
                )?;
            }
        }
    }
    Ok(())
}

fn reconcile_ccxt_cancelled_native_group(
    group: &qx_core::OrderGroupState,
    worker: &WorkerConfig,
    store: &mut dyn qx_zhenlu::OrderGroupStore,
    pipeline: &mut qx_runtime::LiveEventPipeline,
    now: u64,
    source_seq: &mut u64,
) -> Result<usize, String> {
    if group.legs.iter().any(|leg| {
        leg.venue_order_id.is_some()
            || leg.order.filled.raw() != 0
            || leg
                .order
                .exact_amounts
                .is_some_and(|amounts| amounts.filled.is_positive())
    }) {
        return Ok(0);
    };
    if group.status == qx_core::OrderGroupStatus::Active {
        store.begin_native_cancel(&group.intent.group_id)?;
    } else if group.status != qx_core::OrderGroupStatus::CancelPending
        && group.status != qx_core::OrderGroupStatus::ReconcileRequired
    {
        store.require_reconciliation(&group.intent.group_id)?;
    }
    let reconciling = store
        .load(&group.intent.group_id)?
        .is_some_and(|current| current.status == qx_core::OrderGroupStatus::ReconcileRequired);
    let mut snapshots = Vec::new();
    for leg in &group.legs {
        let current = pipeline
            .orders()
            .into_iter()
            .find(|order| order.client_id == leg.order.client_id)
            .ok_or_else(|| format!("OKX OCO 撤销子腿 {} 不在 EventLog", leg.order.client_id))?;
        if current.status == qx_core::OrderStatus::Cancelled
            || current.status == qx_core::OrderStatus::Expired
        {
            snapshots.push((current, leg.venue_order_id.clone()));
            continue;
        }
        if current.status == qx_core::OrderStatus::Filled
            || current.status == qx_core::OrderStatus::Rejected
            || current.filled.raw() != 0
            || current
                .exact_amounts
                .is_some_and(|amounts| amounts.filled.is_positive())
        {
            store.require_reconciliation(&group.intent.group_id)?;
            return Err(format!(
                "OKX OCO 已撤销但子腿 {} 本地已有成交/拒绝事实",
                current.client_id
            ));
        }
        if current.status != qx_core::OrderStatus::CancelPending
            && current
                .status
                .can_transition_to(qx_core::OrderStatus::CancelPending)
        {
            *source_seq = (*source_seq).saturating_add(1);
            pipeline
                .ingest(qx_runtime::RuntimeEventEnvelope::venue(
                    qx_runtime::RuntimeExternalEvent::CancelPending {
                        client_order_id: leg.order.client_id,
                    },
                    now,
                    now,
                    *source_seq,
                    format!(
                        "{}:okx-oco-cancel-pending:{}",
                        worker.id, leg.order.client_id
                    ),
                ))
                .map_err(|error| format!("归约 OKX OCO CancelPending 失败: {error:?}"))?;
        }
        *source_seq = (*source_seq).saturating_add(1);
        pipeline
            .ingest(qx_runtime::RuntimeEventEnvelope::venue(
                qx_runtime::RuntimeExternalEvent::Cancelled {
                    client_order_id: leg.order.client_id,
                },
                now,
                now,
                *source_seq,
                format!("{}:okx-oco-cancelled:{}", worker.id, leg.order.client_id),
            ))
            .map_err(|error| format!("归约 OKX OCO Cancelled 失败: {error:?}"))?;
        let snapshot = pipeline
            .orders()
            .into_iter()
            .find(|order| order.client_id == leg.order.client_id)
            .ok_or_else(|| format!("OKX OCO 撤销后子腿 {} 不在 EventLog", leg.order.client_id))?;
        snapshots.push((snapshot, None));
    }
    store_ccxt_group_snapshots(&group.intent.group_id, snapshots, reconciling, store)?;
    let current = store.load(&group.intent.group_id)?;
    if current.is_some_and(|current| current.status == qx_core::OrderGroupStatus::CancelPending) {
        store.complete_native_cancel(&group.intent.group_id)?;
    }
    Ok(group.legs.len())
}

#[allow(clippy::too_many_arguments)]
fn reconcile_ccxt_triggered_native_group(
    group: &qx_core::OrderGroupState,
    remote: &serde_json::Value,
    order_ids: &[serde_json::Value],
    venue: &mut CcxtProcessVenue,
    worker: &WorkerConfig,
    runtime_config_path: &std::path::Path,
    store: &mut dyn qx_zhenlu::OrderGroupStore,
    pipeline: &mut qx_runtime::LiveEventPipeline,
    now: u64,
    source_seq: &mut u64,
    issues: &mut Vec<serde_json::Value>,
) -> Result<usize, String> {
    let mut updates = 0_usize;
    let Some(remote_id) = order_ids
        .first()
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .filter(|id| id.bytes().all(|byte| byte.is_ascii_digit()))
        .filter(|_| order_ids.len() == 1)
    else {
        mark_ccxt_group_unresolved(
            group,
            worker,
            store,
            pipeline,
            now,
            source_seq,
            "OKX OCO 触发后子单 ID 数量/格式不匹配",
        )?;
        push_ccxt_group_issue(
            issues,
            group,
            "native_order_group_trigger_children_ambiguous",
            "OKX OCO 触发后的子单身份无法唯一归属",
        );
        return Ok(0);
    };
    let role = match remote
        .get("actual_side")
        .and_then(serde_json::Value::as_str)
    {
        Some("tp") => qx_core::OrderGroupLegRole::TakeProfit,
        Some("sl") => qx_core::OrderGroupLegRole::StopLoss,
        _ => {
            mark_ccxt_group_unresolved(
                group,
                worker,
                store,
                pipeline,
                now,
                source_seq,
                "OKX OCO 触发事实缺少 actualSide",
            )?;
            push_ccxt_group_issue(
                issues,
                group,
                "native_order_group_trigger_side_unknown",
                "OKX OCO 触发事实缺少 tp/sl 子腿身份",
            );
            return Ok(0);
        }
    };
    let Some(triggered_leg) = group.legs.iter().find(|leg| leg.role == role) else {
        return Err("OKX OCO 持久组缺少 actualSide 对应的保护腿".into());
    };
    let Some(peer_leg) = group.legs.iter().find(|leg| leg.role != role) else {
        return Err("OKX OCO 持久组缺少对侧保护腿".into());
    };
    if peer_leg.order.filled.raw() != 0
        || peer_leg
            .order
            .exact_amounts
            .is_some_and(|amounts| amounts.filled.is_positive())
    {
        mark_ccxt_group_unresolved(
            group,
            worker,
            store,
            pipeline,
            now,
            source_seq,
            "OKX OCO 对侧腿已有成交，触发结果存在并发成交风险",
        )?;
        push_ccxt_group_issue(
            issues,
            group,
            "native_order_group_peer_fill_conflict",
            "OKX OCO 对侧腿已有成交，必须人工/账户级对账",
        );
        return Ok(0);
    }
    if matches!(
        group.status,
        qx_core::OrderGroupStatus::Submitting
            | qx_core::OrderGroupStatus::CancelPending
            | qx_core::OrderGroupStatus::Active
            | qx_core::OrderGroupStatus::PeerCancelRequired
    ) {
        store.require_reconciliation(&group.intent.group_id)?;
    }
    let mut triggered_order = pipeline
        .orders()
        .into_iter()
        .find(|order| order.client_id == triggered_leg.order.client_id)
        .ok_or_else(|| "OKX OCO 触发子腿不在 EventLog".to_string())?;
    let known_remote_id = pipeline.venue_order_id(triggered_order.client_id);
    if known_remote_id
        .as_deref()
        .is_some_and(|known| known != remote_id)
    {
        mark_ccxt_group_unresolved(
            group,
            worker,
            store,
            pipeline,
            now,
            source_seq,
            "OKX OCO 触发子腿远端 ID 与 EventLog 冲突",
        )?;
        push_ccxt_group_issue(
            issues,
            group,
            "native_order_group_child_identity_mismatch",
            "OKX OCO 触发子腿远端 ID 与 EventLog 冲突",
        );
        return Ok(0);
    }
    if triggered_order.status.is_terminal() && known_remote_id.is_none() {
        mark_ccxt_group_unresolved(
            group,
            worker,
            store,
            pipeline,
            now,
            source_seq,
            "本地触发子腿已终态但缺少远端订单 ID",
        )?;
        push_ccxt_group_issue(
            issues,
            group,
            "native_order_group_child_identity_missing",
            "本地触发子腿已终态但 EventLog 缺少远端订单 ID",
        );
        return Ok(0);
    }
    if !triggered_order.status.is_terminal() {
        let fee_baseline = pipeline
            .fee_baseline_for_order(triggered_order.client_id)
            .map_err(|error| format!("读取 OKX OCO 手续费基线失败: {error}"))?;
        let exact_fee_baseline = pipeline
            .exact_fee_baseline_for_order(triggered_order.client_id)
            .map_err(|error| format!("读取 OKX OCO 精确手续费基线失败: {error}"))?;
        let trade_ids = pipeline
            .fee_trade_ids_for_order(triggered_order.client_id)
            .map_err(|error| format!("读取 OKX OCO 成交去重状态失败: {error}"))?;
        venue
            .restore_order_with_exact_fee_state(
                triggered_order.clone(),
                remote_id.clone(),
                fee_baseline,
                exact_fee_baseline,
                trade_ids,
            )
            .map_err(|error| format!("恢复 OKX OCO 触发子腿失败: {error:?}"))?;
        let events = venue
            .sync_order(triggered_order.client_id, now)
            .map_err(|error| format!("查询 OKX OCO 触发子腿失败: {error:?}"))?;
        let spec = load_worker_market_spec_for_instrument(
            worker,
            &triggered_order.instrument,
            Some(runtime_config_path),
        )?;
        if let Some(spec) = spec {
            updates += ingest_venue_events_with_spec(
                pipeline, events, &worker.id, now, source_seq, &spec,
            )?;
        } else {
            updates += ingest_venue_events(pipeline, events, &worker.id, now, source_seq)?;
        }
    }
    triggered_order = pipeline
        .orders()
        .into_iter()
        .find(|order| order.client_id == triggered_leg.order.client_id)
        .ok_or_else(|| "OKX OCO 触发子腿归约后消失".to_string())?;
    if pipeline.venue_order_id(triggered_order.client_id).is_none()
        && !matches!(
            triggered_order.status,
            qx_core::OrderStatus::PendingSubmit
                | qx_core::OrderStatus::Submitted
                | qx_core::OrderStatus::CancelPending
                | qx_core::OrderStatus::Unknown
        )
    {
        *source_seq = (*source_seq).saturating_add(1);
        pipeline
            .ingest(qx_runtime::RuntimeEventEnvelope::venue(
                qx_runtime::RuntimeExternalEvent::OrderStatusReconciled {
                    client_order_id: triggered_order.client_id,
                    account_id: triggered_order.account_id.clone(),
                    venue_id: triggered_order.instrument.venue.as_str().to_owned(),
                    status: triggered_order.status,
                    venue_order_id: Some(remote_id.clone()),
                    filled_raw: triggered_order.filled.raw(),
                    filled_exact: triggered_order.exact_amounts.map(|amounts| amounts.filled),
                },
                now,
                now,
                *source_seq,
                format!(
                    "{}:okx-oco-child-id:{}",
                    worker.id, triggered_order.client_id
                ),
            ))
            .map_err(|error| format!("持久化 OKX OCO 子腿远端身份失败: {error:?}"))?;
    }
    let peer_order = pipeline
        .orders()
        .into_iter()
        .find(|order| order.client_id == peer_leg.order.client_id)
        .ok_or_else(|| "OKX OCO 对侧子腿不在 EventLog".to_string())?;
    if !peer_order.status.is_terminal() {
        if peer_order.status != qx_core::OrderStatus::CancelPending
            && peer_order
                .status
                .can_transition_to(qx_core::OrderStatus::CancelPending)
        {
            *source_seq = (*source_seq).saturating_add(1);
            pipeline
                .ingest(qx_runtime::RuntimeEventEnvelope::venue(
                    qx_runtime::RuntimeExternalEvent::CancelPending {
                        client_order_id: peer_order.client_id,
                    },
                    now,
                    now,
                    *source_seq,
                    format!(
                        "{}:okx-oco-peer-cancel-pending:{}",
                        worker.id, peer_order.client_id
                    ),
                ))
                .map_err(|error| format!("归约 OKX OCO 对侧 CancelPending 失败: {error:?}"))?;
        }
        *source_seq = (*source_seq).saturating_add(1);
        pipeline
            .ingest(qx_runtime::RuntimeEventEnvelope::venue(
                qx_runtime::RuntimeExternalEvent::Cancelled {
                    client_order_id: peer_order.client_id,
                },
                now,
                now,
                *source_seq,
                format!(
                    "{}:okx-oco-peer-cancelled:{}",
                    worker.id, peer_order.client_id
                ),
            ))
            .map_err(|error| format!("归约 OKX OCO 对侧 Cancelled 失败: {error:?}"))?;
    }
    let peer_snapshot = pipeline
        .orders()
        .into_iter()
        .find(|order| order.client_id == peer_leg.order.client_id)
        .ok_or_else(|| "OKX OCO 对侧子腿归约后消失".to_string())?;
    let group_reconciling = store
        .load(&group.intent.group_id)?
        .is_some_and(|current| current.status == qx_core::OrderGroupStatus::ReconcileRequired);
    store_ccxt_group_snapshots(
        &group.intent.group_id,
        vec![(triggered_order, Some(remote_id)), (peer_snapshot, None)],
        group_reconciling,
        store,
    )?;
    Ok(updates.saturating_add(2))
}

pub fn run_ccxt_reconcile_worker(input: CcxtReconcileWorkerInput) -> Result<(), String> {
    let CcxtReconcileWorkerInput {
        context,
        worker,
        pipeline_root,
        pipeline_storage,
        runtime_config,
        python,
        ccxt_config_path,
        runtime_config_path,
        once,
    } = input;
    if !worker.enabled || worker.role != WorkerRole::Reconciler {
        return Err(format!(
            "worker {} 不是已启用的 CCXT Reconciler worker",
            worker.id
        ));
    }
    let account_id = worker
        .account_id
        .clone()
        .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))?;
    let venue_id = worker
        .venue_id
        .clone()
        .ok_or_else(|| format!("worker {} 缺少 venue_id", worker.id))?;
    if VenueFamily::parse_option(Some(&venue_id)) != Some(VenueFamily::Other) {
        return Err(format!("worker {} 不是有效的 CCXT Venue", worker.id));
    }
    let log_name = required_account_event_log(&worker)?;
    let settlement_currency = account_worker_settlement_currency(&runtime_config, &worker)?;
    let mut pipeline = pipeline_storage
        .open(log_name.clone(), settlement_currency)
        .map_err(|error| format!("打开 CCXT 对账 EventLog 失败: {error}"))?;
    let mut metrics = PipelineMetricsReporter::new(&runtime_config, context.id(), &log_name);
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!("ccxt reconcile polling venue={venue_id}"),
        Some(runtime_timestamp_ms()),
    )?;
    metrics.publish_if_due(true, runtime_timestamp_ms());
    crate::worker_host::run_polling_worker(&context, once, || {
        let mut account_query_observed = false;
        let cycle_started = Instant::now();
        let result = (|| -> Result<u64, String> {
            let query_capabilities = require_account_query_execution_capability(
                &worker,
                &runtime_config.environment,
                AccountQueryPurpose::Reconcile,
                ExecutionConnectorProfile::Ccxt {
                    config_path: std::path::Path::new(&ccxt_config_path),
                },
            )?;
            let query_allowed = |operation: &str| {
                query_capabilities
                    .allowed_operations
                    .iter()
                    .any(|allowed| allowed == operation)
            };
            pipeline
                .refresh()
                .map_err(|error| format!("刷新 CCXT 对账 EventLog 失败: {error:?}"))?;
            let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
                .map_err(|error| format!("启动公共 CCXT Reconcile Worker 失败: {error}"))?;
            let balance_result = match ccxt_reconcile_call(
                &mut client,
                &context,
                serde_json::json!({"op": "fetch_balance"}),
            )
            .map_err(|error| format!("CCXT balance 对账失败: {error}"))?
            {
                Some(result) => result,
                None => return Ok(1),
            };
            let balances = ccxt_balance_facts(&balance_result)?;
            account_query_observed = true;
            let received_ts = runtime_timestamp_ms();
            metrics.observe_gateway_port_business_status(
                ExchangeGatewayPort::AccountQuery,
                GatewayBusinessHealthStatus::Healthy,
                received_ts,
            );
            // 与 Binance 对账链共用同一个身份构造点：seq 从日志尾端接上、correlation 带本轮时间戳。
            let (mut source_seq, balance_correlation) =
                venue_balance_fact_identity(&pipeline, &worker.id, received_ts);
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::AccountBalanceSnapshot {
                        account_id: account_id.clone(),
                        venue_id: venue_id.clone(),
                        balances: balances.clone(),
                    },
                    received_ts,
                    received_ts,
                    source_seq,
                    balance_correlation,
                ))
                .map_err(|error| format!("CCXT 余额事实归约失败: {error:?}"))?;

            let cashflow_since_ms = pipeline
                .log()
                .events()
                .iter()
                .filter_map(|event| match &event.kind {
                    EventKind::AccountCashflow { .. } => Some(event.ts),
                    _ => None,
                })
                .max();

            // 费率只是预估输入，真实资金费/利息/交割必须读取带外部 ID 的账单
            // 增量事实后才能进入 Ledger。优先使用统一 fetch_ledger；若交易所
            // 只暴露合约 funding history，则明确走该能力，不把缺失能力伪装成空账单。
            // 三个覆盖度计数各自记"本轮有没有去取"：能力缺失被跳过与取了且为空
            // 是两种状态，报告里必须能分开（V11 Q69）。
            let mut cashflows_observed = false;
            let mut cashflow_count = 0_usize;
            if query_allowed("cashflows.read") {
                match ccxt_reconcile_call(
                    &mut client,
                    &context,
                    serde_json::json!({
                        "op": "fetch_ledger",
                        "code": null,
                        "since_ms": cashflow_since_ms,
                    }),
                ) {
                    Ok(Some(cashflow_result)) => {
                        cashflows_observed = true;
                        cashflow_count = cashflow_count.saturating_add(ingest_ccxt_cashflows(
                            &mut pipeline,
                            &cashflow_result,
                            &account_id,
                            &venue_id,
                            &worker.id,
                            received_ts,
                            &mut source_seq,
                        )?);
                    }
                    Ok(None) => return Ok(1),
                    Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {}
                    Err(error) => return Err(format!("CCXT 资金流水对账失败: {error}")),
                }
            }
            // 部分交易所的 fetch_ledger 不包含 funding 账单；合约市场再读取
            // 专用 funding history。与 ledger 同一 external_id 会由流水事实幂等去重。
            for instrument in worker
                .symbols
                .iter()
                .filter(|_| query_allowed("funding.read"))
            {
                match ccxt_reconcile_call(
                    &mut client,
                    &context,
                    serde_json::json!({
                        "op": "fetch_funding_history",
                        "instrument": instrument,
                        "since_ms": cashflow_since_ms,
                    }),
                ) {
                    Ok(Some(history_result)) => {
                        cashflows_observed = true;
                        cashflow_count = cashflow_count.saturating_add(ingest_ccxt_cashflows(
                            &mut pipeline,
                            &history_result,
                            &account_id,
                            &venue_id,
                            &worker.id,
                            received_ts,
                            &mut source_seq,
                        )?);
                    }
                    Ok(None) => return Ok(1),
                    Err(history_error)
                        if ccxt_error_is_optional_derivatives_capability(&history_error) => {}
                    Err(history_error) => {
                        return Err(format!(
                            "CCXT 资金费账单对账失败 {instrument}: {history_error}"
                        ));
                    }
                }
            }

            // 合约账户的持仓、保证金和未实现盈亏必须进入同一可恢复事实流。
            // 现货交易所通常不支持 fetch_positions；这类能力缺失只跳过该类快照，
            // 不能把“未支持”误判成空仓。
            let mut positions_observed = false;
            let position_request = if worker.symbols.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::json!(worker.symbols)
            };
            if query_allowed("positions.read") {
                match ccxt_reconcile_call(
                    &mut client,
                    &context,
                    serde_json::json!({
                        "op": "fetch_positions",
                        "instruments": position_request,
                    }),
                ) {
                    Ok(Some(position_result)) => {
                        let positions = ccxt_position_facts(&position_result, &venue_id)?;
                        positions_observed = true;
                        source_seq = source_seq.saturating_add(1);
                        pipeline
                            .ingest(RuntimeEventEnvelope::venue(
                                RuntimeExternalEvent::AccountPositionSnapshot {
                                    account_id: account_id.clone(),
                                    venue_id: venue_id.clone(),
                                    positions,
                                },
                                received_ts,
                                received_ts,
                                source_seq,
                                format!("{}:positions:{}", worker.id, received_ts),
                            ))
                            .map_err(|error| format!("CCXT 持仓事实归约失败: {error:?}"))?;
                    }
                    Ok(None) => return Ok(1),
                    Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {}
                    Err(error) => return Err(format!("CCXT 持仓对账失败: {error}")),
                }
            }

            // 资金费率是风险输入，按 instrument 保存最新观察值；它不直接修改
            // Ledger，实际扣款已经由上面的 Cashflow 账单事实负责。
            let mut funding_rates_observed = false;
            for instrument in worker
                .symbols
                .iter()
                .filter(|_| query_allowed("funding.read"))
            {
                match ccxt_reconcile_call(
                    &mut client,
                    &context,
                    serde_json::json!({
                        "op": "fetch_funding_rate",
                        "instrument": instrument,
                    }),
                ) {
                    Ok(Some(funding_result)) => {
                        let (snapshot, event_ts) = ccxt_funding_fact(&funding_result, &venue_id)?;
                        funding_rates_observed = true;
                        source_seq = source_seq.saturating_add(1);
                        pipeline
                            .ingest(RuntimeEventEnvelope::venue(
                                RuntimeExternalEvent::FundingRateSnapshot { snapshot },
                                event_ts,
                                received_ts,
                                source_seq,
                                format!("{}:funding:{}:{}", worker.id, instrument, received_ts),
                            ))
                            .map_err(|error| format!("CCXT 资金费率事实归约失败: {error:?}"))?;
                    }
                    Ok(None) => return Ok(1),
                    Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {}
                    Err(error) => {
                        return Err(format!("CCXT 资金费率对账失败 {instrument}: {error}"));
                    }
                }
            }

            // 订单逐笔 fetch_order 只能覆盖本地已经知道的订单；进程在写入
            // Accepted 前崩溃、人工下单或其他系统下单，都会只存在于交易所。
            // 先拉取远端活动订单做只读发现，再交给下面的本地订单同步流程。
            let local_orders = pipeline.orders();
            let known_remote_orders = local_orders
                .iter()
                .filter_map(|order| {
                    pipeline
                        .venue_order_id(order.client_id)
                        .map(|remote_id| (remote_id, (order.client_id, order.status)))
                })
                .collect::<BTreeMap<_, _>>();
            let open_order_requests = if worker.symbols.is_empty() {
                vec![None]
            } else {
                worker.symbols.iter().map(Some).collect::<Vec<_>>()
            };
            // 一轮对账有两半发现：远端活动订单在本地归并不了（下面的 open_order_issues），
            // 与本地订单在远端查不到结果（下面的 local_order_issues）。两半必须喂同一份
            // 报告、同一次健康判定，能对上本地订单的还要各落一条待对账事实。
            let mut open_order_issues = Vec::new();
            let mut local_order_issues = Vec::new();
            let mut seen_open_order_keys = BTreeSet::new();
            for instrument in open_order_requests {
                let open_orders_result = ccxt_reconcile_call(
                    &mut client,
                    &context,
                    serde_json::json!({
                        "op": "fetch_open_orders",
                        "instrument": instrument,
                    }),
                );
                let open_orders_result = match open_orders_result {
                    Ok(Some(value)) => value,
                    Ok(None) => return Ok(1),
                    Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => continue,
                    Err(error) => return Err(format!("CCXT 活动订单对账失败: {error}")),
                };
                for issue in ccxt_open_order_issues(
                    &open_orders_result,
                    &local_orders,
                    &known_remote_orders,
                    &venue_id,
                )? {
                    let key = format!(
                        "{}:{}",
                        issue
                            .get("remote_order_id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown"),
                        issue
                            .get("instrument")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                    );
                    if seen_open_order_keys.insert(key) {
                        open_order_issues.push(issue);
                    }
                }
            }

            let shutdown_context = context.clone();
            let mut venue = CcxtProcessVenue::new(venue_id.clone(), Box::new(client))
                .with_shutdown_check(move || shutdown_context.should_stop());
            let mut updates =
                reconcile_order_group_amendment_overlays(&pipeline_storage.root, &pipeline)?;
            updates += reconcile_ccxt_native_order_groups(
                &mut venue,
                &worker,
                &pipeline_storage,
                &runtime_config_path,
                &mut pipeline,
                received_ts,
                &mut source_seq,
                &mut local_order_issues,
            )?;
            let recovered_remote_ids = pipeline
                .orders()
                .into_iter()
                .filter_map(|order| pipeline.venue_order_id(order.client_id))
                .collect::<BTreeSet<_>>();
            open_order_issues.retain(|issue| {
                !issue
                    .get("remote_order_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|remote_id| recovered_remote_ids.contains(remote_id))
            });
            let orders = pipeline.orders();
            let pending_amendment_ids = orders
                .iter()
                .filter(|order| pipeline.pending_amendment(order.client_id).is_some())
                .map(|order| order.client_id)
                .collect::<BTreeSet<_>>();
            for order in orders.into_iter().filter(|order| {
                !order.status.is_terminal() || pending_amendment_ids.contains(&order.client_id)
            }) {
                let Some(remote_id) = pipeline.venue_order_id(order.client_id) else {
                    local_order_issues.push(ccxt_local_order_issue(
                        "local_order_missing_remote_id",
                        &order,
                        &venue_id,
                        "CCXT 对账缺少远端订单号",
                    ));
                    continue;
                };
                let fee_baseline = pipeline
                    .fee_baseline_for_order(order.client_id)
                    .map_err(|error| format!("读取 CCXT 持久手续费基线失败: {error}"))?;
                let exact_fee_baseline = pipeline
                    .exact_fee_baseline_for_order(order.client_id)
                    .map_err(|error| format!("读取 CCXT 持久精确手续费基线失败: {error}"))?;
                let seen_trade_ids = pipeline
                    .fee_trade_ids_for_order(order.client_id)
                    .map_err(|error| format!("读取 CCXT 持久成交费用去重状态失败: {error}"))?;
                venue
                    .restore_order_with_exact_fee_state(
                        order.clone(),
                        remote_id,
                        fee_baseline,
                        exact_fee_baseline,
                        seen_trade_ids,
                    )
                    .map_err(|error| format!("恢复 CCXT 本地订单失败: {error:?}"))?;
                let pending_amendment = pipeline
                    .pending_amendment(order.client_id)
                    .map(|(operation_id, requested)| (operation_id.to_string(), requested.clone()));
                let mut amendment_recovered = false;
                let mut amendment_unresolved = false;
                if let Some((operation_id, requested)) = pending_amendment {
                    match venue.reconcile_amendment(&order, &requested, received_ts) {
                        Ok(Some(events)) => {
                            source_seq = source_seq.saturating_add(1);
                            pipeline
                                .ingest(RuntimeEventEnvelope::venue(
                                    RuntimeExternalEvent::OrderAmended {
                                        operation_id: operation_id.clone(),
                                        order: requested,
                                    },
                                    received_ts,
                                    received_ts,
                                    source_seq,
                                    format!("{}:amend-recovered:{}", worker.id, operation_id),
                                ))
                                .map_err(|error| {
                                    format!("CCXT 改单恢复确认事实归约失败: {error:?}")
                                })?;
                            updates += 1;
                            updates += if let Some(spec) = load_worker_market_spec_for_instrument(
                                &worker,
                                &order.instrument,
                                Some(&runtime_config_path),
                            )? {
                                ingest_venue_events_with_spec(
                                    &mut pipeline,
                                    events,
                                    &worker.id,
                                    received_ts,
                                    &mut source_seq,
                                    &spec,
                                )?
                            } else {
                                ingest_venue_events(
                                    &mut pipeline,
                                    events,
                                    &worker.id,
                                    received_ts,
                                    &mut source_seq,
                                )?
                            };
                            amendment_recovered = true;
                        }
                        Ok(None) => {
                            local_order_issues.push(ccxt_local_order_issue(
                                "pending_amendment_unresolved",
                                &order,
                                &venue_id,
                                "交易所订单条款尚未确认改单请求；保留 pending 且暂停该订单同步",
                            ));
                            amendment_unresolved = true;
                        }
                        Err(error) => {
                            local_order_issues.push(ccxt_local_order_issue(
                                "pending_amendment_unresolved",
                                &order,
                                &venue_id,
                                &format!("改单仍未能由远端订单确认: {error:?}"),
                            ));
                            continue;
                        }
                    }
                }
                if amendment_recovered {
                    if context.should_stop() {
                        return Ok(1);
                    }
                    continue;
                }
                if amendment_unresolved {
                    continue;
                }
                match venue.sync_order(order.client_id, received_ts) {
                    Ok(events) => {
                        updates += if let Some(spec) = load_worker_market_spec_for_instrument(
                            &worker,
                            &order.instrument,
                            Some(&runtime_config_path),
                        )? {
                            ingest_venue_events_with_spec(
                                &mut pipeline,
                                events,
                                &worker.id,
                                received_ts,
                                &mut source_seq,
                                &spec,
                            )?
                        } else {
                            ingest_venue_events(
                                &mut pipeline,
                                events,
                                &worker.id,
                                received_ts,
                                &mut source_seq,
                            )?
                        };
                    }
                    Err(error) => {
                        local_order_issues.push(ccxt_local_order_issue(
                            "local_order_sync_error",
                            &order,
                            &venue_id,
                            &format!("{error:?}"),
                        ));
                    }
                }
                if context.should_stop() {
                    return Ok(1);
                }
            }
            updates += reconcile_order_group_amendment_overlays(&pipeline_storage.root, &pipeline)?;
            // 两半发现从这一行起只有一份清单：事实流、报告与健康都读它。
            let round = ccxt_reconcile_round(&open_order_issues, &local_order_issues);
            for fact in &round.require_reconcile {
                EventLogReconcilePort::new(&mut pipeline, &worker.id, received_ts, &mut source_seq)
                    .with_tag(fact.event_tag)
                    .require_reconcile(fact.client_order_id, &fact.reason)
                    .map_err(|error| {
                        format!(
                            "CCXT 对账发现 {} 无法写入 ReconcileRequired（原因 {}）: {error}",
                            fact.client_order_id, fact.reason
                        )
                    })?;
            }
            let snapshot = pipeline.snapshot();
            let balance_discrepancies = pipeline
                .settlement_balance_discrepancies(&account_id, &venue_id, &balances)
                .map_err(|error| format!("CCXT 账户余额对账失败: {error:?}"))?;
            persist_reconcile_report(ReconcileReportInput {
                pipeline_root: &pipeline_root,
                worker_id: &worker.id,
                account_id: &account_id,
                venue_id: &venue_id,
                observed_ts: received_ts,
                issues: &[],
                additional_order_issues: &round.order_issues,
                balances_count: balances.len(),
                balance_discrepancies: &balance_discrepancies,
                position_snapshots_count: positions_observed.then(|| {
                    snapshot
                        .account_positions
                        .get(&(account_id.clone(), venue_id.clone()))
                        .map(Vec::len)
                        .unwrap_or(0)
                }),
                funding_rate_snapshots_count: funding_rates_observed
                    .then_some(snapshot.funding_rates.len()),
                cashflow_count: cashflows_observed.then_some(cashflow_count),
            })?;
            context.heartbeat(received_ts)?;
            let service_status = ccxt_reconcile_service_status(&round, &balance_discrepancies);
            metrics.observe_gateway_port_business_status(
                ExchangeGatewayPort::Reconciliation,
                if service_status == qx_runtime::ServiceStatus::Ready {
                    GatewayBusinessHealthStatus::Healthy
                } else {
                    GatewayBusinessHealthStatus::Degraded
                },
                received_ts,
            );
            context.mark(
            service_status,
            format!(
                "ccxt reconciled order_updates={updates} open_order_issues={} local_order_issues={} balance_discrepancies={} private_query_ops={:?} omitted_optional={:?}",
                open_order_issues.len(),
                local_order_issues.len(),
                balance_discrepancies.len(),
                query_capabilities.allowed_operations,
                query_capabilities.omitted_optional_operations,
            ),
            Some(received_ts),
        )?;
            metrics.observe(&pipeline);
            metrics.publish_if_due(true, received_ts);
            Ok(5_000)
        })();
        if !context.should_stop() {
            metrics.observe_gateway_port_operation_duration(
                ExchangeGatewayPort::Reconciliation,
                cycle_started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            );
        }
        if result.is_err() {
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
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        "ccxt reconciler stopped",
        Some(runtime_timestamp_ms()),
    )?;
    metrics.publish(false, runtime_timestamp_ms());
    Ok(())
}

fn runtime_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod native_order_group_reconcile_tests {
    use super::{
        bitget_cancel_children_are_unfilled, bitget_oco_child_matches_leg,
        bitget_rejected_oco_children_prove_no_execution, classify_okx_oco_observation,
        mark_ccxt_group_query_failed, okx_algo_client_order_id, reconcile_ccxt_native_order_groups,
        reconcile_order_group_amendment_overlays, sync_ccxt_order_groups_from_user_stream,
        OkxOcoObservation,
    };

    #[test]
    fn okx_oco_rpc_query_error_persists_reconciliation_barrier() {
        use qx_adapter::{CcxtProcessVenue, CcxtRpc};
        use qx_core::{
            ConditionalOrderKind, OrderGroupExecution, OrderGroupIntent, OrderGroupKind,
            OrderGroupLegIntent, OrderGroupLegRole, OrderGroupStatus, OrderStatus,
        };
        use qx_runtime::{
            LiveEventPipeline, RuntimeConfig, RuntimeEventEnvelope, RuntimeExternalEvent,
            WorkerConfig, WorkerRole,
        };
        use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};
        use std::sync::{Arc, Mutex};

        struct FailedGroupQuery(Arc<Mutex<Vec<serde_json::Value>>>);
        impl CcxtRpc for FailedGroupQuery {
            fn call(&mut self, request: serde_json::Value) -> Result<serde_json::Value, String> {
                self.0.lock().unwrap().push(request);
                Err("test query timeout".into())
            }
        }

        let root = std::env::current_dir()
            .unwrap()
            .join("target/test-temp")
            .join(format!(
                "qianxing-okx-rpc-query-unknown-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let take_profit = protection_order(
            91,
            "account",
            "OKX",
            ConditionalOrderKind::TakeProfit,
            "70000",
        );
        let stop_loss = protection_order(
            92,
            "account",
            "OKX",
            ConditionalOrderKind::StopLoss,
            "60000",
        );
        let intent = OrderGroupIntent::new(
            "okx-rpc-query-unknown",
            OrderGroupKind::Oco,
            OrderGroupExecution::NativeRequired,
            vec![
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::TakeProfit,
                    order: take_profit.clone(),
                },
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::StopLoss,
                    order: stop_loss.clone(),
                },
            ],
        )
        .unwrap();
        let mut store = FileOrderGroupStore::new(root.join("order-groups")).unwrap();
        store
            .create(&qx_core::OrderGroupState::from_intent(intent).unwrap())
            .unwrap();
        assert!(store.begin_submission("okx-rpc-query-unknown").unwrap());
        store
            .record_venue_group_id("okx-rpc-query-unknown", "99887766".into())
            .unwrap();
        store
            .activate_native_group("okx-rpc-query-unknown")
            .unwrap();

        let mut pipeline = LiveEventPipeline::open(&root, "account", "USDT").unwrap();
        let mut source_seq = 0_u64;
        for order in [&take_profit, &stop_loss] {
            pipeline.register_order(order.clone(), 10).unwrap();
            source_seq += 1;
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::Accepted {
                        client_order_id: order.client_id,
                        venue_order_id: None,
                    },
                    10,
                    10,
                    source_seq,
                    format!("accepted:{}", order.client_id),
                ))
                .unwrap();
            source_seq += 1;
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::OrderStatusReconciled {
                        client_order_id: order.client_id,
                        account_id: "account".into(),
                        venue_id: "OKX".into(),
                        status: OrderStatus::Working,
                        venue_order_id: Some(format!("7000{}", order.client_id % 10)),
                        filled_raw: 0,
                        filled_exact: Some(qx_core::ExactDecimal::ZERO),
                    },
                    10,
                    10,
                    source_seq,
                    format!("working:{}", order.client_id),
                ))
                .unwrap();
        }
        assert_eq!(
            store.load("okx-rpc-query-unknown").unwrap().unwrap().status,
            OrderGroupStatus::Active
        );

        let worker = WorkerConfig {
            id: "okx-reconciler".into(),
            role: WorkerRole::Reconciler,
            enabled: true,
            account_id: Some("account".into()),
            venue_id: Some("OKX".into()),
            endpoint: None,
            symbols: vec![],
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        };
        let sample = include_str!("../../../deploy/qianxing.runtime.ccxt.example.json");
        let mut runtime_config = RuntimeConfig::from_json(sample).unwrap();
        runtime_config.storage.data_dir = root.to_string_lossy().into_owned();
        let pipeline_storage =
            crate::pipeline::PipelineStorage::from_config(&runtime_config).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut venue =
            CcxtProcessVenue::new("OKX", Box::new(FailedGroupQuery(Arc::clone(&requests))));
        let mut issues = Vec::new();
        let updates = reconcile_ccxt_native_order_groups(
            &mut venue,
            &worker,
            &pipeline_storage,
            std::path::Path::new("unused-runtime-config.json"),
            &mut pipeline,
            20,
            &mut source_seq,
            &mut issues,
        )
        .unwrap();

        assert_eq!(updates, 0);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["op"], "fetch_order_group");
        assert_eq!(requests[0]["algo_id"], "99887766");
        assert_eq!(requests[0]["instrument"], "BTC/USDT.OKX");
        drop(requests);

        let restored = FileOrderGroupStore::new(root.join("order-groups"))
            .unwrap()
            .load("okx-rpc-query-unknown")
            .unwrap()
            .unwrap();
        assert_eq!(restored.status, OrderGroupStatus::ReconcileRequired);
        assert!(restored
            .legs
            .iter()
            .all(|leg| leg.order.status == OrderStatus::Unknown));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0]["reason"], "OKX 原生 OCO 查询失败或响应未知");
        std::fs::remove_dir_all(root).unwrap();
    }

    fn protection_order(
        client_id: u64,
        account_id: &str,
        venue_id: &str,
        kind: qx_core::ConditionalOrderKind,
        trigger: &str,
    ) -> qx_core::Order {
        use qx_core::{
            ConditionalOrderIntent, ExactDecimal, ExactOrderAmounts, InstrumentId, Order,
            OrderStatus, Quantity, Side, TriggerPriceType,
        };

        Order {
            client_id,
            instrument: InstrumentId::parse(&format!("BTC/USDT.{venue_id}")).unwrap(),
            side: Side::Sell,
            qty: Quantity::ZERO,
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: account_id.to_owned(),
            trace: None,
            policy: None,
            exact_amounts: Some(ExactOrderAmounts {
                quantity: ExactDecimal::parse("0.01").unwrap(),
                limit_price: None,
                filled: ExactDecimal::ZERO,
                quote_budget: None,
                quote_filled: ExactDecimal::ZERO,
                market_reference_price: None,
                conditional: Some(ConditionalOrderIntent {
                    kind,
                    trigger_price: Some(ExactDecimal::parse(trigger).unwrap()),
                    trigger_price_type: Some(TriggerPriceType::Mark),
                    trailing_percent: None,
                    trailing_amount: None,
                    trailing_trigger_price: None,
                }),
            }),
        }
    }

    fn pending_group_amendment_fixture(
        root: &std::path::Path,
        include_event_log_pending: bool,
        confirm_in_event_log: bool,
    ) -> (
        qx_zhenlu::FileOrderGroupStore,
        qx_runtime::LiveEventPipeline,
        String,
    ) {
        use qx_core::{
            ConditionalOrderKind, ExactDecimal, OrderGroupExecution, OrderGroupIntent,
            OrderGroupKind, OrderGroupLegIntent, OrderGroupLegRole, OrderGroupState, OrderStatus,
        };
        use qx_runtime::{LiveEventPipeline, RuntimeEventEnvelope, RuntimeExternalEvent};
        use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

        let group_id = "reconcile-oco-amend".to_string();
        let take_profit = protection_order(
            151,
            "account",
            "OKX",
            ConditionalOrderKind::TakeProfit,
            "70000",
        );
        let stop_loss = protection_order(
            152,
            "account",
            "OKX",
            ConditionalOrderKind::StopLoss,
            "60000",
        );
        let intent = OrderGroupIntent::new(
            &group_id,
            OrderGroupKind::Oco,
            OrderGroupExecution::NativeRequired,
            vec![
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::TakeProfit,
                    order: take_profit.clone(),
                },
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::StopLoss,
                    order: stop_loss.clone(),
                },
            ],
        )
        .unwrap();
        let mut group = OrderGroupState::from_intent(intent).unwrap();
        group.status = qx_core::OrderGroupStatus::Active;
        group.venue_group_id = Some("900015".into());
        group.legs[0].order.status = OrderStatus::Accepted;
        group.legs[0].venue_order_id = Some("700015".into());
        group.legs[1].order.status = OrderStatus::Cancelled;
        group.legs[1].venue_order_id = Some("700016".into());

        let mut requested = take_profit.clone();
        requested.exact_amounts.as_mut().unwrap().quantity = ExactDecimal::parse("0.012").unwrap();
        requested.status = OrderStatus::Accepted;

        let mut store = FileOrderGroupStore::new(root.join("order-groups")).unwrap();
        store.create(&group).unwrap();
        store
            .begin_child_amendment(&group_id, 151, "control:amend:151:151".into(), &requested)
            .unwrap();

        let mut pipeline = LiveEventPipeline::open(root, "account", "USDT").unwrap();
        let mut source_seq = 0_u64;
        for order in [&take_profit, &stop_loss] {
            pipeline.register_order(order.clone(), 10).unwrap();
            source_seq += 1;
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::Accepted {
                        client_order_id: order.client_id,
                        venue_order_id: Some(format!("7000{}", order.client_id % 10)),
                    },
                    10,
                    10,
                    source_seq,
                    format!("accepted:{}", order.client_id),
                ))
                .unwrap();
        }
        let mut event_order = requested.clone();
        event_order.status = OrderStatus::Accepted;
        if include_event_log_pending || confirm_in_event_log {
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::OrderAmendmentPending {
                        operation_id: "control:amend:151:151".into(),
                        order: event_order.clone(),
                    },
                    11,
                    11,
                    source_seq + 1,
                    "amend-pending:151",
                ))
                .unwrap();
        }
        if confirm_in_event_log {
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::OrderAmended {
                        operation_id: "control:amend:151:151".into(),
                        order: event_order,
                    },
                    12,
                    12,
                    source_seq + 2,
                    "amend-confirmed:151",
                ))
                .unwrap();
        }
        (store, pipeline, group_id)
    }

    #[test]
    fn group_amendment_recovery_keeps_pending_and_confirms_event_log_result() {
        use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

        let root = std::env::current_dir()
            .unwrap()
            .join("target/test-temp")
            .join(format!(
                "qianxing-group-amend-event-recovery-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let (store, pending_pipeline, group_id) =
            pending_group_amendment_fixture(&root, true, false);
        assert_eq!(
            reconcile_order_group_amendment_overlays(&root, &pending_pipeline).unwrap(),
            0
        );
        assert!(store
            .load(&group_id)
            .unwrap()
            .unwrap()
            .has_pending_amendment());

        let (_, confirmed_pipeline, _) =
            pending_group_amendment_fixture(&root.join("confirmed"), false, true);
        let updates =
            reconcile_order_group_amendment_overlays(&root.join("confirmed"), &confirmed_pipeline)
                .unwrap();
        assert_eq!(updates, 1);
        let confirmed_store =
            FileOrderGroupStore::new(root.join("confirmed/order-groups")).unwrap();
        let confirmed = confirmed_store.load(&group_id).unwrap().unwrap();
        assert!(!confirmed.has_pending_amendment());
        assert_eq!(confirmed.status, qx_core::OrderGroupStatus::Active);
        assert_eq!(
            confirmed.legs[0].order.exact_amounts.unwrap().quantity,
            qx_core::ExactDecimal::parse("0.012").unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn group_amendment_recovery_releases_overlay_when_venue_was_not_reached() {
        use qx_zhenlu::OrderGroupStore;

        let root = std::env::current_dir()
            .unwrap()
            .join("target/test-temp")
            .join(format!(
                "qianxing-group-amend-abort-recovery-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let (store, pipeline, group_id) = pending_group_amendment_fixture(&root, false, false);
        assert_eq!(
            reconcile_order_group_amendment_overlays(&root, &pipeline).unwrap(),
            1
        );
        let restored = store.load(&group_id).unwrap().unwrap();
        assert!(!restored.has_pending_amendment());
        assert_eq!(restored.status, qx_core::OrderGroupStatus::Active);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn okx_oco_observation_requires_consistent_child_and_fill_facts() {
        assert!(matches!(
            classify_okx_oco_observation(&serde_json::json!({
                "state": "live",
                "order_ids": [],
                "actual_size": "0"
            })),
            Ok(OkxOcoObservation::Live)
        ));
        assert!(matches!(
            classify_okx_oco_observation(&serde_json::json!({
                "state": "canceled",
                "order_ids": [],
                "actual_size": "0"
            })),
            Ok(OkxOcoObservation::Cancelled)
        ));
        assert!(matches!(
            classify_okx_oco_observation(&serde_json::json!({
                "state": "effective",
                "order_ids": ["1884789786215137280"],
                "actual_size": "0.01"
            })),
            Ok(OkxOcoObservation::Triggered(ids)) if ids.len() == 1
        ));
        assert!(classify_okx_oco_observation(&serde_json::json!({
            "state": "effective",
            "order_ids": [],
            "actual_size": "0.01"
        }))
        .is_err());
        assert!(classify_okx_oco_observation(&serde_json::json!({
            "state": "canceled",
            "order_ids": [],
            "actual_size": "0.001"
        }))
        .is_err());
        assert!(classify_okx_oco_observation(&serde_json::json!({
            "state": "live",
            "order_ids": ["bad-id"],
            "actual_size": "0"
        }))
        .is_err());
    }

    #[test]
    fn okx_oco_query_failure_persists_reconciliation_barrier_and_unknown_order_facts() {
        use qx_core::{
            ConditionalOrderKind, OrderGroupExecution, OrderGroupIntent, OrderGroupKind,
            OrderGroupLegIntent, OrderGroupLegRole, OrderGroupStatus, OrderStatus,
        };
        use qx_runtime::{
            LiveEventPipeline, RuntimeEventEnvelope, RuntimeExternalEvent, WorkerConfig, WorkerRole,
        };
        use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

        let root = std::env::current_dir()
            .unwrap()
            .join("target/test-temp")
            .join(format!(
                "qianxing-okx-query-unknown-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
        let take_profit = protection_order(
            81,
            "account",
            "OKX",
            ConditionalOrderKind::TakeProfit,
            "70000",
        );
        let stop_loss = protection_order(
            82,
            "account",
            "OKX",
            ConditionalOrderKind::StopLoss,
            "60000",
        );
        let intent = OrderGroupIntent::new(
            "okx-query-unknown",
            OrderGroupKind::Oco,
            OrderGroupExecution::NativeRequired,
            vec![
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::TakeProfit,
                    order: take_profit.clone(),
                },
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::StopLoss,
                    order: stop_loss.clone(),
                },
            ],
        )
        .unwrap();
        let mut store = FileOrderGroupStore::new(root.join("order-groups")).unwrap();
        store
            .create(&qx_core::OrderGroupState::from_intent(intent).unwrap())
            .unwrap();
        assert!(store.begin_submission("okx-query-unknown").unwrap());
        store
            .record_venue_group_id("okx-query-unknown", "99887766".into())
            .unwrap();
        store.activate_native_group("okx-query-unknown").unwrap();

        let mut pipeline = LiveEventPipeline::open(&root, "account", "USDT").unwrap();
        let mut source_seq = 0_u64;
        for order in [&take_profit, &stop_loss] {
            pipeline.register_order(order.clone(), 10).unwrap();
            source_seq += 1;
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::Accepted {
                        client_order_id: order.client_id,
                        venue_order_id: None,
                    },
                    10,
                    10,
                    source_seq,
                    format!("accepted:{}", order.client_id),
                ))
                .unwrap();
        }
        let group = store.load("okx-query-unknown").unwrap().unwrap();
        assert_eq!(group.status, OrderGroupStatus::Active);
        let worker = WorkerConfig {
            id: "okx-reconciler".into(),
            role: WorkerRole::Reconciler,
            enabled: true,
            account_id: Some("account".into()),
            venue_id: Some("OKX".into()),
            endpoint: None,
            symbols: vec![],
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        };
        let mut issues = Vec::new();
        mark_ccxt_group_query_failed(
            &group,
            false,
            &worker,
            &mut store,
            &mut pipeline,
            20,
            &mut source_seq,
            &mut issues,
        )
        .unwrap();

        let restored = FileOrderGroupStore::new(root.join("order-groups"))
            .unwrap()
            .load("okx-query-unknown")
            .unwrap()
            .unwrap();
        assert_eq!(restored.status, OrderGroupStatus::ReconcileRequired);
        assert!(restored
            .legs
            .iter()
            .all(|leg| leg.order.status == OrderStatus::Unknown));
        assert_eq!(
            pipeline
                .log()
                .events()
                .iter()
                .filter(|event| matches!(
                    &event.kind,
                    qx_core::EventKind::ReconcileRequired { client_order_id }
                        if [81, 82].contains(client_order_id)
                ))
                .count(),
            2
        );
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0]["reason"], "OKX 原生 OCO 查询失败或响应未知");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn okx_algo_client_id_matches_the_submit_identity_shape() {
        let first = okx_algo_client_order_id("protect-btc-1");
        assert!(first.starts_with("QX"));
        assert_eq!(first.len(), 18);
        assert!(first.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        assert_eq!(first, okx_algo_client_order_id("protect-btc-1"));
        assert_ne!(first, okx_algo_client_order_id("protect-btc-2"));
    }

    #[test]
    fn bitget_oco_cancel_requires_child_cancel_facts_and_zero_fills() {
        use qx_core::{
            ConditionalOrderKind, OrderGroupExecution, OrderGroupIntent, OrderGroupKind,
            OrderGroupLegIntent, OrderGroupLegRole, OrderGroupState,
        };
        let group = OrderGroupState::from_intent(
            OrderGroupIntent::new(
                "bitget-cancel-check",
                OrderGroupKind::Oco,
                OrderGroupExecution::NativeRequired,
                vec![
                    OrderGroupLegIntent {
                        role: OrderGroupLegRole::TakeProfit,
                        order: protection_order(
                            31,
                            "account",
                            "BITGET",
                            ConditionalOrderKind::TakeProfit,
                            "70000",
                        ),
                    },
                    OrderGroupLegIntent {
                        role: OrderGroupLegRole::StopLoss,
                        order: protection_order(
                            32,
                            "account",
                            "BITGET",
                            ConditionalOrderKind::StopLoss,
                            "60000",
                        ),
                    },
                ],
            )
            .unwrap(),
        )
        .unwrap();
        assert!(bitget_cancel_children_are_unfilled(
            &group,
            &[serde_json::json!({
                "side": "sell", "qty": "0.01", "status": "cancelled", "cum_exec_qty": "0"
            })]
        ));
        assert!(!bitget_cancel_children_are_unfilled(&group, &[]));
        assert!(!bitget_cancel_children_are_unfilled(
            &group,
            &[serde_json::json!({
                "side": "sell", "qty": "0.01", "status": "filled", "cum_exec_qty": "0.01"
            })]
        ));
        assert!(!bitget_cancel_children_are_unfilled(
            &group,
            &[serde_json::json!({
                "side": "buy", "qty": "0.01", "status": "cancelled", "cum_exec_qty": "0"
            })]
        ));
        assert!(!bitget_cancel_children_are_unfilled(
            &group,
            &[serde_json::json!({
                "side": "sell", "qty": "0.001", "status": "cancelled", "cum_exec_qty": "0"
            })]
        ));
        assert!(!bitget_cancel_children_are_unfilled(
            &group,
            &[serde_json::json!({
                "side": "sell", "qty": "0.01", "status": "cancelled", "cum_exec_qty": "unknown"
            })]
        ));
    }

    #[test]
    fn bitget_triggered_oco_children_match_exact_tp_and_sl_shapes() {
        use qx_core::{
            ConditionalOrderKind, ExactDecimal, OrderGroupExecution, OrderGroupIntent,
            OrderGroupKind, OrderGroupLegIntent, OrderGroupLegRole, OrderGroupState,
        };
        let mut tp = protection_order(
            41,
            "account",
            "BITGET",
            ConditionalOrderKind::TakeProfit,
            "70000",
        );
        let tp_amounts = tp.exact_amounts.as_mut().unwrap();
        tp_amounts.limit_price = Some(ExactDecimal::parse("70000").unwrap());
        tp_amounts.conditional = None;
        let sl = protection_order(
            42,
            "account",
            "BITGET",
            ConditionalOrderKind::StopLoss,
            "60000",
        );
        let group = OrderGroupState::from_intent(
            OrderGroupIntent::new(
                "bitget-role-match",
                OrderGroupKind::Oco,
                OrderGroupExecution::NativeRequired,
                vec![
                    OrderGroupLegIntent {
                        role: OrderGroupLegRole::TakeProfit,
                        order: tp,
                    },
                    OrderGroupLegIntent {
                        role: OrderGroupLegRole::StopLoss,
                        order: sl,
                    },
                ],
            )
            .unwrap(),
        )
        .unwrap();
        let take_profit = serde_json::json!({
            "side": "sell", "qty": "0.01", "order_type": "limit", "price": "70000"
        });
        let stop_loss = serde_json::json!({
            "side": "sell", "qty": "0.01", "order_type": "market", "price": ""
        });
        assert!(bitget_oco_child_matches_leg(&take_profit, &group.legs[0]));
        assert!(!bitget_oco_child_matches_leg(&take_profit, &group.legs[1]));
        assert!(bitget_oco_child_matches_leg(&stop_loss, &group.legs[1]));
        assert!(!bitget_oco_child_matches_leg(
            &serde_json::json!({
                "side": "sell", "qty": "0.01", "order_type": "limit", "price": "70001"
            }),
            &group.legs[0]
        ));
    }

    #[test]
    fn bitget_rejected_oco_requires_empty_or_failed_zero_fill_children() {
        assert!(bitget_rejected_oco_children_prove_no_execution(&[]));
        assert!(bitget_rejected_oco_children_prove_no_execution(&[
            serde_json::json!({
                "order_id": "901", "status": "failed", "cum_exec_qty": "0"
            }),
        ]));
        assert!(!bitget_rejected_oco_children_prove_no_execution(&[
            serde_json::json!({
                "order_id": "bad", "status": "failed", "cum_exec_qty": "0"
            }),
        ]));
        assert!(!bitget_rejected_oco_children_prove_no_execution(&[
            serde_json::json!({
                "order_id": "901", "status": "cancelled", "cum_exec_qty": "0"
            }),
        ]));
        assert!(!bitget_rejected_oco_children_prove_no_execution(&[
            serde_json::json!({
                "order_id": "901", "status": "failed", "cum_exec_qty": "0.001"
            }),
        ]));
    }

    #[test]
    fn bitget_rejected_oco_closes_both_orders_and_group_when_no_children_exist() {
        use super::reconcile_bitget_rejected_oco_group;
        use qx_core::{
            ConditionalOrderKind, OrderGroupExecution, OrderGroupIntent, OrderGroupKind,
            OrderGroupLegIntent, OrderGroupLegRole, OrderGroupStatus, OrderStatus,
        };
        use qx_runtime::{LiveEventPipeline, RuntimeEventEnvelope, RuntimeExternalEvent};
        use qx_runtime::{WorkerConfig, WorkerRole};
        use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

        let root = std::env::current_dir()
            .unwrap()
            .join("target/test-temp")
            .join(format!(
                "qianxing-bitget-rejected-oco-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
        let take_profit = protection_order(
            51,
            "account",
            "BITGET",
            ConditionalOrderKind::TakeProfit,
            "70000",
        );
        let stop_loss = protection_order(
            52,
            "account",
            "BITGET",
            ConditionalOrderKind::StopLoss,
            "60000",
        );
        let intent = OrderGroupIntent::new(
            "bitget-rejected-no-child",
            OrderGroupKind::Oco,
            OrderGroupExecution::NativeRequired,
            vec![
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::TakeProfit,
                    order: take_profit.clone(),
                },
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::StopLoss,
                    order: stop_loss.clone(),
                },
            ],
        )
        .unwrap();
        let mut store = FileOrderGroupStore::new(root.join("order-groups")).unwrap();
        store
            .create(&qx_core::OrderGroupState::from_intent(intent).unwrap())
            .unwrap();
        let mut pipeline = LiveEventPipeline::open(&root, "account", "USDT").unwrap();
        let mut source_seq = 0;
        for order in [&take_profit, &stop_loss] {
            pipeline.register_order(order.clone(), 10).unwrap();
            let mut submitted = order.clone();
            submitted.status = OrderStatus::Submitted;
            store
                .apply_order_snapshot("bitget-rejected-no-child", &submitted, None)
                .unwrap();
            source_seq += 1;
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::Accepted {
                        client_order_id: order.client_id,
                        venue_order_id: None,
                    },
                    10,
                    10,
                    source_seq,
                    format!("accepted:{}", order.client_id),
                ))
                .unwrap();
            let accepted = pipeline
                .orders()
                .into_iter()
                .find(|current| current.client_id == order.client_id)
                .unwrap();
            store
                .apply_order_snapshot("bitget-rejected-no-child", &accepted, None)
                .unwrap();
        }
        store
            .record_venue_group_id("bitget-rejected-no-child", "123456789".into())
            .unwrap();
        let group = store.load("bitget-rejected-no-child").unwrap().unwrap();
        let worker = WorkerConfig {
            id: "bitget-reconciler".into(),
            role: WorkerRole::Reconciler,
            enabled: true,
            account_id: Some("account".into()),
            venue_id: Some("BITGET".into()),
            endpoint: None,
            symbols: vec![],
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        };
        let mut issues = Vec::new();
        assert_eq!(
            reconcile_bitget_rejected_oco_group(
                &group,
                &[],
                &worker,
                &mut store,
                &mut pipeline,
                20,
                &mut source_seq,
                &mut issues,
            )
            .unwrap(),
            2
        );
        let reconciled = store.load("bitget-rejected-no-child").unwrap().unwrap();
        assert_eq!(reconciled.status, OrderGroupStatus::Rejected);
        assert!(reconciled
            .legs
            .iter()
            .all(|leg| leg.order.status == OrderStatus::Rejected));
        assert!(pipeline
            .orders()
            .iter()
            .all(|order| order.status == OrderStatus::Rejected));
        assert!(issues.is_empty());
    }

    #[test]
    fn ccxt_user_stream_syncs_only_matching_native_oco_group_facts() {
        use qx_core::{
            ConditionalOrderKind, OrderGroupExecution, OrderGroupIntent, OrderGroupKind,
            OrderGroupLegIntent, OrderGroupLegRole, OrderGroupStatus, OrderStatus,
        };
        use qx_runtime::{LiveEventPipeline, RuntimeEventEnvelope, RuntimeExternalEvent};
        use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

        let root = std::env::current_dir()
            .unwrap()
            .join("target/test-temp")
            .join(format!(
                "qianxing-ccxt-user-stream-oco-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
        let account_id = "main";
        let venue_id = "OKX";
        let tp = protection_order(
            101,
            account_id,
            venue_id,
            ConditionalOrderKind::TakeProfit,
            "70000",
        );
        let sl = protection_order(
            102,
            account_id,
            venue_id,
            ConditionalOrderKind::StopLoss,
            "60000",
        );
        let intent = OrderGroupIntent::new(
            "oco-stream-test",
            OrderGroupKind::Oco,
            OrderGroupExecution::NativeRequired,
            vec![
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::TakeProfit,
                    order: tp.clone(),
                },
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::StopLoss,
                    order: sl.clone(),
                },
            ],
        )
        .unwrap();
        let mut store = FileOrderGroupStore::new(root.join("order-groups")).unwrap();
        store
            .create(&qx_core::OrderGroupState::from_intent(intent).unwrap())
            .unwrap();
        assert!(store.begin_submission("oco-stream-test").unwrap());
        let mut submitted_tp = tp.clone();
        submitted_tp.status = OrderStatus::Submitted;
        let mut submitted_sl = sl.clone();
        submitted_sl.status = OrderStatus::Submitted;
        store
            .apply_order_snapshot("oco-stream-test", &submitted_tp, None)
            .unwrap();
        store
            .apply_order_snapshot("oco-stream-test", &submitted_sl, None)
            .unwrap();
        store
            .record_venue_group_id("oco-stream-test", "algo-77".into())
            .unwrap();
        store.activate_native_group("oco-stream-test").unwrap();

        let mut pipeline = LiveEventPipeline::open(&root, "account-main", "USDT").unwrap();
        let mut source_seq = 0;
        for (order, remote_id) in [(tp, "201"), (sl, "202")] {
            pipeline.register_order(order.clone(), 1).unwrap();
            source_seq += 1;
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::Accepted {
                        client_order_id: order.client_id,
                        venue_order_id: Some(remote_id.into()),
                    },
                    2,
                    2,
                    source_seq,
                    format!("ccxt-accepted-{}", order.client_id),
                ))
                .unwrap();
        }

        assert_eq!(
            sync_ccxt_order_groups_from_user_stream(&root, &pipeline, account_id, venue_id)
                .unwrap(),
            2
        );
        let synced = store.load("oco-stream-test").unwrap().unwrap();
        assert_eq!(synced.status, OrderGroupStatus::Active);
        for leg in synced.legs {
            assert!(matches!(
                leg.order.status,
                OrderStatus::Accepted | OrderStatus::Working
            ));
            assert_eq!(
                leg.venue_order_id.as_deref(),
                Some(if leg.order.client_id == 101 {
                    "201"
                } else {
                    "202"
                })
            );
        }
        assert_eq!(
            sync_ccxt_order_groups_from_user_stream(&root, &pipeline, account_id, venue_id)
                .unwrap(),
            0,
            "replaying the same stream projection must be idempotent"
        );
        assert_eq!(
            sync_ccxt_order_groups_from_user_stream(&root, &pipeline, "other-account", venue_id)
                .unwrap(),
            0,
            "a different account stream must not write this OCO group"
        );
        assert_eq!(
            sync_ccxt_order_groups_from_user_stream(&root, &pipeline, account_id, "BITGET")
                .unwrap(),
            0,
            "a different venue stream must not write this OCO group"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
