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
    write_workbook(
        output,
        &[
            ("Overview", convert(overview)),
            ("DataQuality", convert(quality)),
        ],
    )?;
    Ok(output.to_path_buf())
}

pub(crate) fn write_multi_market_depth_comparison_xlsx(
    output: &Path,
    overview: Vec<Vec<String>>,
    ranking: Vec<Vec<String>>,
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
    write_workbook(
        output,
        &[
            ("Overview", convert(overview)),
            ("Ranking", convert(ranking)),
        ],
    )?;
    Ok(output.to_path_buf())
}

pub(crate) fn write_report_xlsx(
    summary_path: &Path,
    summary: &Value,
    benchmark: Option<&Value>,
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
    write_workbook(output, &sheets)?;
    Ok(output.to_path_buf())
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

fn validate_xlsx_path(output: &Path) -> Result<(), String> {
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
    let mut rows = vec![vec![t("timestamp"), t("quantity_raw"), t("price_raw")]];
    rows.extend(fills.iter().map(|fill| {
        vec![
            n(fill.ts.to_string()),
            t(fill.qty_raw.to_string()),
            t(fill.price_raw.to_string()),
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
