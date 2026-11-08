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
mod multi_account;
mod multi_market_depth;
pub(crate) use comparison::write_comparison_xlsx;
pub(crate) use multi_account::write_multi_account_report_xlsx;
pub(crate) use multi_market_depth::{
    write_multi_market_depth_comparison_xlsx, write_multi_market_depth_xlsx,
};
mod ashare;
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

mod execution;
mod rows;
use rows::{benchmark_rows, equity_rows, fill_rows, overview_rows};

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

mod strategy_diagnostics;
#[cfg(test)]
use strategy_diagnostics::split_utf16_chunks;
use strategy_diagnostics::strategy_diagnostics_sheets;

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
