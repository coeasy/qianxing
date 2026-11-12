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
            push_ccxt_group_issue(
                issues,
                &group,
                "native_order_group_query_unknown",
                "OKX 原生 OCO 查询失败或响应未知",
            );
            continue;
        };
        if is_bitget {
            updates = updates.saturating_add(reconcile_bitget_uta_oco_observation(
                &group, &remote, worker, &mut store, pipeline, now, source_seq, issues,
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

fn okx_algo_client_order_id(group_id: &str) -> String {
    let mut hash = qx_core::Fnv1a::new();
    hash.write_text(group_id);
    format!("QX{:016X}", hash.finish())
}

fn reconcile_bitget_uta_oco_observation(
    group: &qx_core::OrderGroupState,
    remote: &serde_json::Value,
    worker: &WorkerConfig,
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
            let mut updates = 0_usize;
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
        bitget_cancel_children_are_unfilled, classify_okx_oco_observation,
        okx_algo_client_order_id, sync_ccxt_order_groups_from_user_stream, OkxOcoObservation,
    };

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
