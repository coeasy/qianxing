//! Self-contained, offline XLSX report writer.

use crate::report_html::{monthly_series, read_equity_analysis_csv, read_fills_csv};
use crate::report_output_paths::report_artifact_paths;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Clone)]
enum Cell {
    Text(String),
    Number(String),
}

type Sheet = (&'static str, Vec<Vec<Cell>>);

mod openxml;
use openxml::write_workbook;
mod comparison;
pub(crate) use comparison::write_comparison_xlsx;
mod ashare;

pub(crate) fn write_multi_market_depth_xlsx(
    output: &Path,
    overview: Vec<Vec<String>>,
    quality: Vec<Vec<String>>,
    strategy_diagnostics: Option<&Value>,
) -> Result<PathBuf, String> {
    validate_xlsx_path(output)?;
    let convert = |rows: Vec<Vec<String>>| {
        rows.into_iter()
            .map(|row| row.into_iter().map(Cell::Text).collect())
            .collect()
    };
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建 XLSX 报告目录失败 {}: {error}", parent.display()))?;
    let mut sheets = vec![
        ("Overview", convert(overview)),
        ("DataQuality", convert(quality)),
    ];
    if let Some(diagnostics) = strategy_diagnostics {
        sheets.extend(strategy_diagnostics_sheets(diagnostics)?);
    }
    write_workbook(output, &sheets)?;
    Ok(output.to_path_buf())
}

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

pub(crate) fn write_multi_market_depth_comparison_xlsx(
    output: &Path,
    overview: Vec<Vec<String>>,
    ranking: Vec<Vec<String>>,
    diagnostic_runs: Option<&Value>,
) -> Result<PathBuf, String> {
    validate_xlsx_path(output)?;
    let convert = |rows: Vec<Vec<String>>| {
        rows.into_iter()
            .map(|row| row.into_iter().map(Cell::Text).collect())
            .collect()
    };
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建 XLSX 报告目录失败 {}: {error}", parent.display()))?;
    let mut sheets = vec![
        ("Overview", convert(overview)),
        ("Ranking", convert(ranking)),
    ];
    if let Some(runs) = diagnostic_runs.and_then(Value::as_array) {
        let mut records = Vec::new();
        for run in runs {
            let Some(run_id) = run.get("run_id").and_then(Value::as_str) else {
                continue;
            };
            for diagnostic in run
                .get("market_strategy_diagnostics")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let mut record = diagnostic.clone();
                record["run_id"] = Value::String(run_id.to_owned());
                records.push(record);
            }
        }
        if !records.is_empty() {
            sheets.extend(strategy_diagnostics_sheets(&Value::Array(records))?);
        }
    }
    write_workbook(output, &sheets)?;
    Ok(output.to_path_buf())
}

pub(crate) fn write_report_xlsx(
    summary_path: &Path,
    summary: &Value,
    benchmark: Option<&Value>,
    strategy_diagnostics: Option<&Value>,
    output: &Path,
) -> Result<PathBuf, String> {
    if !output
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("xlsx"))
    {
        return Err(format!(
            "XLSX 报告路径必须以 .xlsx 结尾: {}",
            output.display()
        ));
    }
    let artifact_paths = report_artifact_paths(summary_path)?;
    let equity = read_equity_analysis_csv(&artifact_paths.equity_csv)?;
    let fills = read_fills_csv(&artifact_paths.fills_csv)?;
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建 XLSX 报告目录失败 {}: {error}", parent.display()))?;
    let output_absolute = if output.exists() {
        output
            .canonicalize()
            .map_err(|error| format!("解析 XLSX 输出路径失败 {}: {error}", output.display()))?
    } else {
        parent
            .canonicalize()
            .map_err(|error| format!("解析 XLSX 报告目录失败 {}: {error}", parent.display()))?
            .join(output.file_name().ok_or("XLSX 输出路径缺少文件名")?)
    };
    let mut inputs = vec![
        summary_path,
        artifact_paths.equity_csv.as_path(),
        artifact_paths.fills_csv.as_path(),
    ];
    if let Some(benchmark_path) = benchmark
        .and_then(|analysis| analysis.pointer("/benchmark/path"))
        .and_then(Value::as_str)
    {
        inputs.push(Path::new(benchmark_path));
    }
    for input in inputs {
        let input_absolute = input
            .canonicalize()
            .map_err(|error| format!("解析 XLSX 来源文件失败 {}: {error}", input.display()))?;
        if input_absolute == output_absolute {
            return Err(format!(
                "XLSX 输出不能覆盖报告来源文件: {}",
                input.display()
            ));
        }
    }
    let monthly_equity: Vec<_> = equity
        .iter()
        .map(|(timestamp, value, _)| (*timestamp, *value))
        .collect();
    let monthly = monthly_series(summary, &monthly_equity);
    let mut sheets = vec![
        ("Overview", overview_rows(summary, benchmark)),
        ("Equity", equity_rows(&equity)),
        ("Fills", fill_rows(&fills)),
        ("Monthly", execution::monthly_rows(&monthly)),
    ];
    if let Some(benchmark) = benchmark {
        sheets.push(("Benchmark", benchmark_rows(benchmark)?));
    }
    if let Some(analysis) = summary.get("ashare_analysis") {
        sheets.push(("Ashare", ashare::analysis_rows(analysis)?));
    }
    if let Some(attempts) = summary.get("execution_attempts").and_then(Value::as_array) {
        sheets.push(("Execution", execution::execution_attempt_rows(attempts)));
    }
    if let Some(diagnostics) = strategy_diagnostics {
        sheets.extend(strategy_diagnostics_sheets(diagnostics)?);
    }
    write_workbook(output, &sheets)?;
    Ok(output.to_path_buf())
}

/// Add a compact, filterable diagnostic summary and lossless JSON evidence to a workbook.
/// The evidence is split into UTF-16-safe chunks below Excel's 32,767-character cell limit.
fn strategy_diagnostics_sheets(diagnostics: &Value) -> Result<Vec<Sheet>, String> {
    const MAX_CELL_UTF16_UNITS: usize = 30_000;
    let records: Vec<&Value> = match diagnostics {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![diagnostics],
        _ => return Err("策略诊断 XLSX 输入必须是对象或对象数组".into()),
    };
    if records.is_empty() {
        return Err("策略诊断 XLSX 输入不能为空".into());
    }

    let has_run_id = records
        .iter()
        .any(|record| record.get("run_id").and_then(Value::as_str).is_some());
    let mut summary_header = vec![
        t("diagnostic_id"),
        t("instrument"),
        t("strategy_id"),
        t("data_fingerprint"),
        t("input_kind"),
        t("input_points"),
        t("as_of"),
        t("lookahead_status"),
        t("startup_status"),
        t("pit_visibility_status"),
    ];
    if has_run_id {
        summary_header.insert(0, t("run_id"));
    }
    let mut summary_rows = vec![summary_header];
    let mut evidence_rows = vec![vec![
        t("diagnostic_id"),
        t("chunk_index"),
        t("chunk_count"),
        t("json_utf8_fragment"),
    ]];

    for (index, record) in records.iter().enumerate() {
        if !record.is_object() {
            return Err(format!("策略诊断 XLSX 第 {} 项必须是对象", index + 1));
        }
        let detail = record
            .get("report")
            .filter(|value| value.is_object())
            .unwrap_or(record);
        let text = |key: &str| -> String {
            record
                .get(key)
                .and_then(Value::as_str)
                .or_else(|| detail.get(key).and_then(Value::as_str))
                .unwrap_or("—")
                .to_owned()
        };
        let points = record
            .get("input_points")
            .or_else(|| record.get("input_bars"))
            .or_else(|| record.get("input_events"))
            .or_else(|| detail.get("input_bars"))
            .or_else(|| detail.get("input_events"))
            .map(Value::to_string)
            .unwrap_or_else(|| "—".into());
        let as_of = record
            .get("as_of")
            .or_else(|| detail.get("as_of"))
            .map(Value::to_string)
            .unwrap_or_else(|| "—".into());
        let status = |pointer: &str| {
            detail
                .pointer(pointer)
                .and_then(Value::as_str)
                .unwrap_or("missing")
                .to_owned()
        };
        let diagnostic_id = (index + 1).to_string();
        let mut row = vec![
            t(&diagnostic_id),
            t(text("instrument")),
            t(text("strategy_id")),
            t(text("data_fingerprint")),
            t(text("input_kind")),
            t(&points),
            t(&as_of),
            t(status("/lookahead/status")),
            t(status("/startup/status")),
            t(status("/data_visibility/status")),
        ];
        if has_run_id {
            row.insert(0, t(text("run_id")));
        }
        summary_rows.push(row);

        let json = serde_json::to_string(record)
            .map_err(|error| format!("策略诊断 XLSX JSON 序列化失败: {error}"))?;
        let fragments = split_utf16_chunks(&json, MAX_CELL_UTF16_UNITS);
        let chunk_count = fragments.len().to_string();
        for (chunk_index, fragment) in fragments.into_iter().enumerate() {
            evidence_rows.push(vec![
                t(&diagnostic_id),
                t((chunk_index + 1).to_string()),
                t(&chunk_count),
                t(&fragment),
            ]);
        }
    }
    Ok(vec![
        ("StrategyDiagnostics", summary_rows),
        ("DiagnosticEvidence", evidence_rows),
    ])
}

fn split_utf16_chunks(value: &str, max_units: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_units = 0;
    for character in value.chars() {
        let units = character.len_utf16();
        if current_units > 0 && current_units + units > max_units {
            chunks.push(std::mem::take(&mut current));
            current_units = 0;
        }
        current.push(character);
        current_units += units;
    }
    if !current.is_empty() || chunks.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn optional_text(value: Option<&str>) -> Cell {
    value.map_or_else(|| t("—"), t)
}

fn optional_integer(value: Option<i128>) -> Cell {
    value.map_or_else(|| t("—"), |value| t(value.to_string()))
}

fn optional_float(value: Option<f64>) -> Cell {
    value.map_or_else(|| t("—"), |value| n(value.to_string()))
}

pub(crate) fn validate_xlsx_path(output: &Path) -> Result<(), String> {
    if !output
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("xlsx"))
    {
        return Err(format!(
            "XLSX 报告路径必须以 .xlsx 结尾: {}",
            output.display()
        ));
    }
    Ok(())
}

fn overview_rows(summary: &Value, benchmark: Option<&Value>) -> Vec<Vec<Cell>> {
    let mut rows = vec![vec![t("Field"), t("Value")]];
    for (label, pointer) in [
        ("Run ID", "/run_id"),
        ("Instrument", "/instrument"),
        ("Strategy", "/strategy_id"),
        ("Data fingerprint", "/input/fingerprint"),
        (
            "Dataset quality manifest fingerprint",
            "/dataset_quality/manifest_fingerprint",
        ),
        (
            "Dataset quality content hash",
            "/dataset_quality/content_hash",
        ),
        ("Dataset rows", "/dataset_quality/quality_report/row_count"),
        (
            "Primary dataset manifest fingerprint",
            "/dataset_quality/legs/primary/manifest_fingerprint",
        ),
        (
            "Reference dataset manifest fingerprint",
            "/dataset_quality/legs/reference/manifest_fingerprint",
        ),
        (
            "Dataset interval (ms)",
            "/dataset_quality/quality_report/interval_ms",
        ),
        (
            "Dataset missing intervals",
            "/dataset_quality/quality_report/missing_intervals",
        ),
        (
            "Dataset duplicate rows",
            "/dataset_quality/quality_report/duplicate_rows",
        ),
        (
            "Dataset out-of-order rows",
            "/dataset_quality/quality_report/out_of_order_rows",
        ),
        (
            "Dataset timezone",
            "/dataset_quality/quality_report/timezone",
        ),
        (
            "Corporate action coverage",
            "/dataset_quality/quality_report/corporate_action_coverage",
        ),
        ("Return (bps)", "/metrics/return_bps"),
        ("Maximum drawdown (bps)", "/metrics/max_drawdown_bps"),
        ("Sharpe per period", "/metrics/sharpe"),
        ("Sortino", "/metrics/sortino"),
        ("Calmar", "/metrics/calmar"),
        ("Win rate", "/metrics/win_rate"),
        ("Profit factor", "/metrics/profit_factor"),
        ("Fills", "/fills"),
        ("Rejected orders", "/rejected_orders"),
    ] {
        let Some(value) = summary.pointer(pointer) else {
            continue;
        };
        let cell = match value {
            Value::Number(number) => Cell::Number(number.to_string()),
            Value::String(text) => Cell::Text(text.clone()),
            Value::Null => Cell::Text("—".into()),
            _ => Cell::Text(value.to_string()),
        };
        rows.push(vec![t(label), cell]);
    }
    if let Some(benchmark) = benchmark {
        let daily = benchmark.get("period_basis").and_then(Value::as_str)
            == Some("per_trading_day_no_annualization");
        let (period, mean_pointer, tracking_pointer, ratio_pointer) = if daily {
            (
                "trading day",
                "/metrics/mean_active_return_bps_per_trading_day",
                "/metrics/tracking_error_bps_per_trading_day",
                "/metrics/information_ratio_per_trading_day",
            )
        } else {
            (
                "bar",
                "/metrics/mean_active_return_bps_per_bar",
                "/metrics/tracking_error_bps_per_bar",
                "/metrics/information_ratio_per_bar",
            )
        };
        for (label, pointer) in [
            ("Benchmark instrument".to_string(), "/benchmark/instrument"),
            (
                "Benchmark price basis".to_string(),
                "/benchmark/price_basis",
            ),
            (
                "Benchmark return (bps)".to_string(),
                "/metrics/benchmark_return_bps",
            ),
            (
                "Active return (bps)".to_string(),
                "/metrics/active_return_bps",
            ),
            (
                format!("Mean active return (bps per {period})"),
                mean_pointer,
            ),
            (
                format!("Tracking error (bps per {period})"),
                tracking_pointer,
            ),
            (format!("Information ratio (per {period})"), ratio_pointer),
        ] {
            if let Some(value) = benchmark.pointer(pointer) {
                let cell = match value {
                    Value::Number(number) => Cell::Number(number.to_string()),
                    Value::String(text) => Cell::Text(text.clone()),
                    Value::Null => Cell::Text("—".into()),
                    _ => Cell::Text(value.to_string()),
                };
                rows.push(vec![t(&label), cell]);
            }
        }
    }
    rows
}

fn equity_rows(equity: &[(i64, i128, Option<i128>)]) -> Vec<Vec<Cell>> {
    let mut rows = vec![vec![t("timestamp"), t("equity_raw"), t("position_raw")]];
    rows.extend(equity.iter().map(|(ts, value, position)| {
        vec![
            n(ts.to_string()),
            t(value.to_string()),
            optional_integer(*position),
        ]
    }));
    rows
}

fn fill_rows(fills: &[crate::report_html::FillPoint]) -> Vec<Vec<Cell>> {
    let mut rows = vec![vec![
        t("timestamp"),
        t("order_id"),
        t("leg"),
        t("account_id"),
        t("strategy_id"),
        t("signal_id"),
        t("intent_id"),
        t("venue_id"),
        t("venue_order_id"),
        t("quantity_raw"),
        t("quantity_exact"),
        t("price_raw"),
        t("price_exact"),
        t("fee_raw"),
        t("exact_fees_json"),
    ]];
    rows.extend(fills.iter().map(|fill| {
        vec![
            n(fill.ts.to_string()),
            n(fill.order_id.to_string()),
            optional_text(fill.leg.as_deref()),
            optional_text(fill.account_id.as_deref()),
            t(fill.strategy_id.clone()),
            optional_text(fill.signal_id.as_deref()),
            optional_text(fill.intent_id.as_deref()),
            optional_text(fill.venue_id.as_deref()),
            optional_text(fill.venue_order_id.as_deref()),
            t(fill.qty_raw.to_string()),
            optional_text(fill.qty_exact.as_deref()),
            t(fill.price_raw.to_string()),
            optional_text(fill.price_exact.as_deref()),
            optional_text(fill.fee_raw.map(|value| value.to_string()).as_deref()),
            optional_text(fill.exact_fees_json.as_deref()),
        ]
    }));
    rows
}

mod execution;

fn benchmark_rows(analysis: &Value) -> Result<Vec<Vec<Cell>>, String> {
    let instrument = string_at(analysis, "/benchmark/instrument")?;
    let fingerprint = string_at(analysis, "/benchmark/fingerprint")?;
    let price_basis = string_at(analysis, "/benchmark/price_basis")?;
    let alignment = string_at(analysis, "/alignment")?;
    let period_basis = string_at(analysis, "/period_basis")?;
    let series = analysis
        .get("series")
        .and_then(Value::as_array)
        .ok_or("基准分析缺少逐 Bar 序列")?;
    let mut rows = vec![
        vec![
            t("instrument"),
            t(instrument),
            t("fingerprint"),
            t(fingerprint),
            t("price_basis"),
            t(price_basis),
            t("alignment"),
            t(alignment),
            t("period_basis"),
            t(period_basis),
        ],
        vec![
            t("start_ts"),
            t("end_ts"),
            t("portfolio_equity_start_raw"),
            t("portfolio_equity_end_raw"),
            t("benchmark_close_start_raw"),
            t("benchmark_close_end_raw"),
            t("portfolio_return_bps"),
            t("benchmark_return_bps"),
            t("active_return_bps"),
        ],
    ];
    for period in series {
        rows.push(vec![
            required_cell(period, "start_ts", true)?,
            required_cell(period, "end_ts", true)?,
            required_cell(period, "portfolio_equity_start_raw", false)?,
            required_cell(period, "portfolio_equity_end_raw", false)?,
            required_cell(period, "benchmark_close_start_raw", false)?,
            required_cell(period, "benchmark_close_end_raw", false)?,
            required_cell(period, "portfolio_return_bps", true)?,
            required_cell(period, "benchmark_return_bps", true)?,
            required_cell(period, "active_return_bps", true)?,
        ]);
    }
    Ok(rows)
}

fn required_cell(value: &Value, key: &str, numeric: bool) -> Result<Cell, String> {
    let value = value
        .get(key)
        .filter(|value| !value.is_null())
        .ok_or_else(|| format!("基准分析序列缺少 {key}"))?;
    let value = match value {
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        _ => return Err(format!("基准分析序列字段 {key} 必须为标量")),
    };
    Ok(if numeric { n(value) } else { t(value) })
}

fn string_at<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("基准分析缺少 {pointer}"))
}

fn t(value: impl Into<String>) -> Cell {
    Cell::Text(value.into())
}
fn n(value: impl Into<String>) -> Cell {
    Cell::Number(value.into())
}

fn report_cell(value: &Value) -> Cell {
    match value {
        Value::Null => t("—"),
        Value::Bool(value) => t(value.to_string()),
        Value::Number(value) => n(value.to_string()),
        Value::String(value) => t(value),
        Value::Array(_) | Value::Object(_) => t(value.to_string()),
    }
}

#[cfg(test)]
mod exact_fill_xlsx_tests {
    use super::{fill_rows, Cell};
    use crate::report_html::FillPoint;

    #[test]
    fn xlsx_fill_sheet_exports_exact_quantity_price_and_fees() {
        let rows = fill_rows(&[FillPoint {
            ts: 1_700_000_000_000,
            order_id: 7,
            leg: Some("reference".into()),
            qty_raw: 0,
            price_raw: 0,
            fee_raw: Some(0),
            qty_exact: Some("0.001".into()),
            price_exact: Some("0.00001234".into()),
            exact_fees_json: Some(r#"[{"currency":"USDT","amount":"0.00000001"}]"#.into()),
            account_id: Some("account-1".into()),
            strategy_id: "strategy-1".into(),
            signal_id: None,
            intent_id: None,
            venue_id: Some("OKX".into()),
            venue_order_id: Some("venue-7".into()),
        }]);

        assert!(matches!(&rows[0][10], Cell::Text(value) if value == "quantity_exact"));
        assert!(matches!(&rows[0][12], Cell::Text(value) if value == "price_exact"));
        assert!(matches!(&rows[0][14], Cell::Text(value) if value == "exact_fees_json"));
        assert!(matches!(&rows[1][9], Cell::Text(value) if value == "0"));
        assert!(matches!(&rows[1][10], Cell::Text(value) if value == "0.001"));
        assert!(matches!(&rows[1][12], Cell::Text(value) if value == "0.00001234"));
        assert!(matches!(&rows[1][14], Cell::Text(value) if value.contains("0.00000001")));
    }
}

#[cfg(test)]
mod strategy_diagnostics_xlsx_tests {
    use super::*;

    #[test]
    fn diagnostic_json_chunks_obey_excel_cell_limit_and_reassemble_losslessly() {
        let payload = "行情📈诊断".repeat(12_000);
        let chunks = split_utf16_chunks(&payload, 30_000);
        assert!(chunks.len() > 1);
        assert!(chunks
            .iter()
            .all(|chunk| chunk.encode_utf16().count() <= 30_000));
        assert_eq!(chunks.concat(), payload);
    }

    #[test]
    fn diagnostics_workbook_has_summary_and_complete_evidence_sheets() {
        let diagnostic = serde_json::json!({
            "instrument": "BTCUSDT.BINANCE",
            "strategy_id": "momentum",
            "data_fingerprint": "sha256:test",
            "input_kind": "bar",
            "input_bars": 10,
            "as_of": 9,
            "lookahead": {"status": "potential_lookahead_detected"},
            "startup": {"status": "startup_sensitive"},
            "data_visibility": {"status": "visibility_unknown"},
            "details": "corporate_action.cash_dividend"
        });
        let sheets = strategy_diagnostics_sheets(&diagnostic).unwrap();
        assert_eq!(sheets.len(), 2);
        assert_eq!(sheets[0].0, "StrategyDiagnostics");
        assert_eq!(sheets[1].0, "DiagnosticEvidence");
        assert!(matches!(
            &sheets[0].1[1][7],
            Cell::Text(status) if status == "potential_lookahead_detected"
        ));
        let evidence = sheets[1]
            .1
            .iter()
            .skip(1)
            .filter_map(|row| match row.get(3) {
                Some(Cell::Text(fragment)) => Some(fragment.as_str()),
                _ => None,
            })
            .fold(String::new(), |mut output, fragment| {
                output.push_str(fragment);
                output
            });
        let restored: Value = serde_json::from_str(&evidence).unwrap();
        assert_eq!(restored, diagnostic);
    }

    #[test]
    fn comparison_diagnostic_workbook_rows_keep_the_run_id() {
        let diagnostic = serde_json::json!({
            "run_id": "run-alpha",
            "instrument": "BTCUSDT.BINANCE",
            "strategy_id": "momentum",
            "report": {
                "data_fingerprint": "sha256:test",
                "input_kind": "depth_midpoint_bar",
                "input_events": 10,
                "as_of": 9,
                "lookahead": {"status": "causal_event_contract"},
                "startup": {"status": "stable_across_requested_windows"},
                "data_visibility": {"status": "visibility_unknown"}
            }
        });
        let sheets = strategy_diagnostics_sheets(&Value::Array(vec![diagnostic])).unwrap();
        assert!(matches!(
            &sheets[0].1[0][0],
            Cell::Text(name) if name == "run_id"
        ));
        assert!(matches!(
            &sheets[0].1[1][0],
            Cell::Text(run_id) if run_id == "run-alpha"
        ));
        let evidence = sheets[1]
            .1
            .iter()
            .skip(1)
            .filter_map(|row| match row.get(3) {
                Some(Cell::Text(fragment)) => Some(fragment.as_str()),
                _ => None,
            })
            .collect::<String>();
        let restored: Value = serde_json::from_str(&evidence).unwrap();
        assert_eq!(restored["run_id"], "run-alpha");
    }
}
