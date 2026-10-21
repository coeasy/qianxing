//! Pure CCXT reconciliation decisions shared by worker hosts and tests.

use qx_core::Order;
use qx_runtime::RuntimeBalanceDiscrepancy;

pub struct CcxtReconcileFact {
    pub client_order_id: u64,
    pub reason: String,
    pub event_tag: &'static str,
}

pub struct CcxtReconcileRound {
    pub order_issues: Vec<serde_json::Value>,
    pub require_reconcile: Vec<CcxtReconcileFact>,
}

pub fn ccxt_local_order_issue(
    kind: &str,
    order: &Order,
    venue_id: &str,
    reason: &str,
) -> serde_json::Value {
    serde_json::json!({
        "kind": kind,
        "source": "ccxt.reconcile.local_orders",
        "venue_id": venue_id,
        "client_order_id": order.client_id,
        "instrument": order.instrument.to_string(),
        "local_status": format!("{:?}", order.status),
        "reason": reason,
        "observed": true,
    })
}

/// Unifies report findings, reconciliation facts, and health status for one round.
pub fn ccxt_reconcile_round(
    remote_open_issues: &[serde_json::Value],
    local_order_issues: &[serde_json::Value],
) -> CcxtReconcileRound {
    let order_issues = remote_open_issues
        .iter()
        .chain(local_order_issues)
        .cloned()
        .collect::<Vec<_>>();
    let require_reconcile = order_issues
        .iter()
        .filter_map(|issue| {
            let client_order_id = issue.get("client_order_id")?.as_u64()?;
            let kind = issue
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("ccxt_reconcile_issue");
            let reason = issue
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(kind)
                .to_string();
            Some(CcxtReconcileFact {
                client_order_id,
                reason,
                event_tag: ccxt_reconcile_event_tag(kind),
            })
        })
        .collect();
    CcxtReconcileRound {
        order_issues,
        require_reconcile,
    }
}

pub fn ccxt_reconcile_service_status(
    round: &CcxtReconcileRound,
    balance_discrepancies: &[RuntimeBalanceDiscrepancy],
) -> qx_runtime::ServiceStatus {
    if round.order_issues.is_empty() && balance_discrepancies.is_empty() {
        qx_runtime::ServiceStatus::Ready
    } else {
        qx_runtime::ServiceStatus::Degraded
    }
}

fn ccxt_reconcile_event_tag(kind: &str) -> &'static str {
    match kind {
        "remote_open_local_terminal" => "remote-terminal",
        "remote_open_unmapped_local_order" => "remote-unmapped",
        "unknown_remote_open_order" => "remote-unknown",
        "local_order_missing_remote_id" => "missing-remote",
        "local_order_sync_error" => "order-error",
        _ => "reconcile",
    }
}
