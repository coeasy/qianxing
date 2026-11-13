//! Multi-account Bar and depth workbook projection and publication.

use super::*;

pub(crate) fn write_multi_account_report_xlsx(
    report: &Value,
    output: &Path,
) -> Result<PathBuf, String> {
    validate_xlsx_path(output)?;
    let accounts = report
        .get("accounts")
        .and_then(Value::as_array)
        .ok_or("多账户 XLSX 报告缺少 accounts 数组")?;
    if accounts.len() < 2 || accounts.len() > 32 {
        return Err("多账户 XLSX 报告的账户数必须为 2..=32".into());
    }
    let is_depth_report =
        report.get("report_kind").and_then(Value::as_str) == Some("multi_account_depth_backtest");
    let mut overview = vec![vec![t("Field"), t("Value")]];
    for (label, pointer) in [
        ("Batch ID", "/batch_id"),
        ("Settlement currency", "/settlement_currency"),
        ("Spec fingerprint", "/spec_fingerprint"),
        ("Account count", "/summary/accounts_total"),
        ("Succeeded", "/summary/accounts_succeeded"),
        ("Failed", "/summary/accounts_failed"),
        ("Weighted return bps", "/summary/weighted_return_bps"),
        ("Successful fills", "/summary/successful_fills"),
        ("Complete", "/summary/complete"),
    ] {
        if let Some(value) = report.pointer(pointer) {
            overview.push(vec![t(label), report_cell(value)]);
        }
    }
    overview.push(vec![
        t("Account semantics"),
        t(report
            .get("account_semantics")
            .and_then(Value::as_str)
            .unwrap_or("unknown")),
    ]);

    let mut account_header = vec![
        t("account_id"),
        t("status"),
        t("instrument"),
        t("strategy_id"),
        t("return_bps"),
        t("max_drawdown_bps"),
        t("fills"),
        t("equity_points"),
        t("run_id"),
        t("result_hash"),
        t("data_fingerprint"),
    ];
    if is_depth_report {
        account_header.splice(3..3, [t("tier"), t("snapshots")]);
    }
    let mut account_rows = vec![account_header];
    let mut equity_rows = vec![vec![t("account_id"), t("timestamp_ms"), t("equity_raw")]];
    let mut fill_rows = vec![vec![
        t("account_id"),
        t("timestamp_ms"),
        t("order_id"),
        t("qty_raw"),
        t("qty_exact"),
        t("price_raw"),
        t("price_exact"),
        t("fee_raw"),
        t("exact_fees_json"),
        t("strategy_id"),
        t("event_id"),
        t("event_seq"),
    ]];
    let mut event_rows = vec![vec![
        t("account_id"),
        t("seq"),
        t("timestamp_ms"),
        t("priority"),
        t("source_seq"),
        t("kind"),
        t("order_id"),
        t("payload"),
    ]];
    let mut diagnostics = Vec::new();
    for account in accounts {
        let account_id = account
            .get("account_id")
            .and_then(Value::as_str)
            .ok_or("多账户 XLSX 报告含缺少 account_id 的条目")?;
        let mut row = vec![
            t(account_id),
            report_cell(&account["status"]),
            report_cell(&account["instrument"]),
            report_cell(&account["strategy_id"]),
            report_cell(&account["return_bps"]),
            report_cell(&account["max_drawdown_bps"]),
            report_cell(&account["fills"]),
            report_cell(&account["equity_points"]),
            report_cell(&account["run_id"]),
            report_cell(&account["result_hash"]),
            report_cell(&account["data_fingerprint"]),
        ];
        if is_depth_report {
            row.splice(
                3..3,
                [
                    report_cell(&account["tier"]),
                    report_cell(&account["snapshots"]),
                ],
            );
        }
        account_rows.push(row);
        for point in account
            .get("equity_series")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            equity_rows.push(vec![
                t(account_id),
                report_cell(&point["timestamp_ms"]),
                report_cell(&point["equity_raw"]),
            ]);
        }
        for fill in account
            .get("fill_rows")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            fill_rows.push(vec![
                t(account_id),
                report_cell(&fill["timestamp_ms"]),
                report_cell(&fill["order_id"]),
                report_cell(&fill["qty_raw"]),
                report_cell(&fill["qty_exact"]),
                report_cell(&fill["price_raw"]),
                report_cell(&fill["price_exact"]),
                report_cell(&fill["fee_raw"]),
                report_cell(&fill["exact_fees_json"]),
                report_cell(&fill["strategy_id"]),
                report_cell(&fill["event_id"]),
                report_cell(&fill["event_seq"]),
            ]);
        }
        for event in account
            .get("event_rows")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            event_rows.push(vec![
                t(account_id),
                report_cell(&event["seq"]),
                report_cell(&event["timestamp_ms"]),
                report_cell(&event["priority"]),
                report_cell(&event["source_seq"]),
                report_cell(&event["kind"]),
                report_cell(&event["order_id"]),
                report_cell(&event["payload"]),
            ]);
        }
        if let Some(diagnostic) = account
            .get("strategy_bias_diagnostics")
            .filter(|value| value.get("identity_bound").and_then(Value::as_bool) == Some(true))
            .and_then(|value| value.get("report"))
        {
            diagnostics.push(serde_json::json!({
                "run_id": account_id,
                "report": diagnostic,
            }));
        }
    }
    let mut sheets: Vec<Sheet> = vec![
        ("Overview", overview),
        ("Accounts", account_rows),
        ("Equity", equity_rows),
        ("Fills", fill_rows),
        ("Events", event_rows),
    ];
    if !diagnostics.is_empty() {
        sheets.extend(strategy_diagnostics_sheets(&Value::Array(diagnostics))?);
    }
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建多账户 XLSX 报告目录失败 {}: {error}", parent.display()))?;
    write_workbook(output, &sheets)?;
    Ok(output.to_path_buf())
}
