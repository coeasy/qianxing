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
use std::time::{SystemTime, UNIX_EPOCH};

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
            let trade_recovery_issues = append_missing_reconciled_fills(
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
    metrics.publish(false, runtime_timestamp_ms());
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
