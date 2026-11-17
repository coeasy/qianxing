//! Verified projection for synchronized, independently funded exchange accounts.

use super::*;

pub(super) fn read_report(
    body: &str,
    paths: &AppPaths,
) -> Result<qx_app::MultiAccountJointResearchReportV1, AppError> {
    let outcome: qx_app::MultiAccountJointBacktestOutcome = serde_json::from_str(body)
        .map_err(|error| invalid_report(format!("联合账户结果 JSON 无效: {error}")))?;
    let root = std::fs::canonicalize(&paths.artifact_root)
        .map_err(|error| AppError::from_io("解析 API 联合账户产物根目录", &error))?;
    let reader = RootedArtifactReader { root };
    let context = RunContext::new(CallerCapability::Research, &outcome.run_id);
    qx_app::read_multi_account_joint_report_v1_with_reader(&outcome, &context, &reader)
}

pub(super) fn is_joint_account_outcome(outcome: &Value) -> bool {
    outcome
        .get("run_manifest_path")
        .and_then(Value::as_str)
        .is_some()
        && outcome
            .get("accounts")
            .and_then(Value::as_array)
            .is_some_and(|accounts| {
                accounts.iter().any(|account| {
                    account.get("exchange_id").and_then(Value::as_str).is_some()
                        && account.get("outcome").is_some()
                })
            })
}

pub(super) fn build(
    outcome_value: &Value,
    paths: &AppPaths,
    options: &qx_app::MultiAccountJointReportReadOptionsV1,
) -> Result<ResearchArtifactView, AppError> {
    let outcome: qx_app::MultiAccountJointBacktestOutcome =
        serde_json::from_value(outcome_value.clone())
            .map_err(|error| invalid_report(format!("联合账户结果结构无效: {error}")))?;
    let artifact_root = std::fs::canonicalize(&paths.artifact_root)
        .map_err(|error| AppError::from_io("解析 API 研究产物根目录", &error))?;
    let artifact_reader = RootedArtifactReader {
        root: artifact_root,
    };
    let context = RunContext::new(CallerCapability::Research, &outcome.run_id);
    let report = qx_app::read_multi_account_joint_report_v1_with_reader_and_options(
        &outcome,
        &context,
        &artifact_reader,
        options,
    )?;

    let accounts = report
        .accounts
        .iter()
        .map(|account| {
            let verification = json!({
                "run_id": account.run_id,
                "result_hash": account.result_hash,
                "data_fingerprint": account.data_fingerprint,
                "verified": true,
                "checks": ["父级联合报告已重放核验该账户成员产物"],
                "mismatches": []
            });
            let detail = json!({
                "schema_version": 13,
                "run_kind": "multi_account_joint_member",
                "run_id": account.run_id,
                "instrument": format!("{}:{}", account.exchange_id, account.account_id),
                "verification": verification,
                "summary": {
                    "run_id": account.run_id,
                    "return_bps": account.return_bps,
                    "max_drawdown_bps": account.max_drawdown_bps,
                    "fills": account.successful_fills,
                    "settlement_currency": report.settlement_currency,
                    "markets_total": account.markets_total,
                    "strategies_total": account.strategies_total
                },
                "equity": {
                    "total": account.equity_points,
                    "truncated": true,
                    "rows": []
                },
                "fills": {
                    "total": account.fill_rows_total,
                    "truncated": account.fill_rows_truncated,
                    "rows": account.fill_rows
                },
                "events": {
                    "total": account.event_rows_total,
                    "offset": account.event_rows_offset,
                    "limit": account.event_rows_limit,
                    "previous_offset": account.event_rows_previous_offset,
                    "next_offset": account.event_rows_next_offset,
                    "truncated": account.event_rows_next_offset.is_some(),
                    "rows": account.event_rows
                },
                "bars": {
                    "source_status": "not_loaded_by_joint_report",
                    "total": 0,
                    "truncated": false,
                    "rows": []
                },
                "event_trace_available": false
            });
            AccountResearchView {
                account_id: account.account_id.clone(),
                exchange_id: Some(account.exchange_id.clone()),
                status: "succeeded",
                run_id: Some(account.run_id.clone()),
                instrument: None,
                data_fingerprint: Some(account.data_fingerprint.clone()),
                result_hash: Some(account.result_hash.clone()),
                return_bps: Some(account.return_bps),
                max_drawdown_bps: Some(account.max_drawdown_bps),
                fills: Some(account.successful_fills),
                equity_points: Some(account.equity_points),
                report: Some(detail),
            }
        })
        .collect::<Vec<_>>();
    let equity_rows = report
        .equity
        .rows
        .iter()
        .map(|point| json!({"ts": point.ts, "equity_raw": point.equity_raw}))
        .collect::<Vec<_>>();
    let event_total = report
        .accounts
        .iter()
        .try_fold(0_u64, |total, account| {
            total.checked_add(account.event_rows_total)
        })
        .ok_or_else(|| invalid_report("联合账户事件总数溢出"))?;

    Ok(ResearchArtifactView {
        schema_version: 13,
        run_kind: Some("multi_account_joint"),
        run_id: report.run_id.clone(),
        instrument: format!("multi-account-joint[{}]", report.settlement_currency),
        verification: report.verification.clone(),
        summary: json!({
            "run_id": report.run_id,
            "settlement_currency": report.settlement_currency,
            "spec_fingerprint": report.spec_fingerprint,
            "accounts_total": report.summary.accounts_total,
            "accounts_succeeded": report.summary.accounts_total,
            "accounts_failed": 0,
            "markets_total": report.summary.markets_total,
            "successful_initial_cash_raw": report.summary.initial_cash_raw,
            "final_equity_raw": report.summary.final_equity_raw,
            "successful_fills": report.summary.successful_fills,
            "weighted_return_bps": report.summary.return_bps,
            "max_drawdown_bps": report.summary.max_drawdown_bps,
            "complete": report.summary.complete,
            "event_rows_across_accounts": event_total
        }),
        equity: CsvView {
            total: report.equity.total,
            truncated: report.equity.truncated,
            rows: equity_rows,
        },
        fills: CsvView {
            total: report.fills.total,
            truncated: report.fills.truncated,
            rows: report.fills.rows.clone(),
        },
        events: EventView {
            total: 0,
            offset: 0,
            truncated: false,
            rows: Vec::new(),
        },
        bars: empty_bars("unsupported_input_kind"),
        depth: None,
        event_trace_available: Some(false),
        event_instrument: None,
        strategy_diagnostics: None,
        market_strategy_diagnostics: None,
        markets: None,
        accounts: Some(accounts),
        account_event_offsets: None,
    })
}
