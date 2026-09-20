//! 对账报告落盘：把余额、持仓与订单差异写成结构化 JSON 报告。

use super::*;

fn reconcile_issue_json(issue: &AdapterReconcileIssue) -> serde_json::Value {
    match issue {
        AdapterReconcileIssue::MissingLocally { client_order_id } => serde_json::json!({
            "kind": "missing_locally",
            "client_order_id": client_order_id,
        }),
        AdapterReconcileIssue::MissingAtVenue { client_order_id } => serde_json::json!({
            "kind": "missing_at_venue",
            "client_order_id": client_order_id,
        }),
        AdapterReconcileIssue::StatusMismatch {
            client_order_id,
            local,
            venue,
        } => serde_json::json!({
            "kind": "status_mismatch",
            "client_order_id": client_order_id,
            "local": local,
            "venue": venue,
        }),
        AdapterReconcileIssue::FilledMismatch {
            client_order_id,
            local,
            venue,
        } => serde_json::json!({
            "kind": "filled_mismatch",
            "client_order_id": client_order_id,
            "local_raw": local.raw(),
            "venue_raw": venue.raw(),
        }),
    }
}

pub(crate) struct ReconcileReportInput<'a> {
    pub(crate) pipeline_root: &'a Path,
    pub(crate) worker_id: &'a str,
    pub(crate) account_id: &'a str,
    pub(crate) venue_id: &'a str,
    pub(crate) observed_ts: u64,
    pub(crate) issues: &'a [AdapterReconcileIssue],
    pub(crate) additional_order_issues: &'a [serde_json::Value],
    pub(crate) balances_count: usize,
    pub(crate) balance_discrepancies: &'a [RuntimeBalanceDiscrepancy],
    pub(crate) position_snapshots_count: usize,
    pub(crate) funding_rate_snapshots_count: usize,
    pub(crate) cashflow_count: usize,
}

pub(crate) fn persist_reconcile_report(input: ReconcileReportInput<'_>) -> Result<(), String> {
    let report = ReconcileReportSnapshot {
        schema_version: 1,
        worker_id: input.worker_id.into(),
        account_id: input.account_id.into(),
        venue_id: input.venue_id.into(),
        observed_ts: input.observed_ts,
        order_issues: input
            .issues
            .iter()
            .map(reconcile_issue_json)
            .chain(input.additional_order_issues.iter().cloned())
            .collect(),
        balances_count: input.balances_count,
        balance_discrepancies: input
            .balance_discrepancies
            .iter()
            .map(|value| serde_json::to_value(value).expect("balance discrepancy is serializable"))
            .collect(),
        position_snapshots_count: input.position_snapshots_count,
        funding_rate_snapshots_count: input.funding_rate_snapshots_count,
        cashflow_count: input.cashflow_count,
    };
    report.validate()?;
    JsonStateStore::new(input.pipeline_root)
        .save_json_at(format!("reconcile/{}.json", input.worker_id), &report)
        .map(|_| ())
        .map_err(|error| format!("保存对账报告失败: {error:?}"))
}

pub(crate) fn reconcile_command(argv: &[String]) {
    if let Some(path) = argv.get(2).cloned() {
        let worker_id = argv
            .get(3)
            .cloned()
            .unwrap_or_else(|| "reconciler-main".into());
        if let Err(error) = run_binance_worker(Path::new(&path), &worker_id, true) {
            eprintln!("Binance 对账失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    let local = [(1_u64, 10_i128), (2, 5)];
    let venue = [(1_u64, 10_i128), (2, 4)];
    let diffs = qx_genglu::reconcile_orders(
        &local
            .iter()
            .map(|(id, qty)| Order {
                client_id: *id,
                instrument: InstrumentId::parse("DEMO.SIM").unwrap(),
                side: Side::Buy,
                qty: Quantity::from_raw(*qty),
                limit: None,
                status: OrderStatus::PartiallyFilled,
                filled: Quantity::from_raw(*qty),
                account_id: "main".into(),
                trace: None,
                policy: None,
            })
            .collect::<Vec<_>>(),
        &venue,
    );
    println!("[更路 · reconcile] 差异数={} 明细={:?}", diffs.len(), diffs);
}
