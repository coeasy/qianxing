//! Typed row projections for the supported XLSX report sheets.

#![allow(clippy::items_after_test_module)] // Keep projection tests beside their row fixtures.

use super::{n, optional_integer, optional_text, t, Cell};
use serde_json::Value;

pub(super) fn overview_rows(summary: &Value, benchmark: Option<&Value>) -> Vec<Vec<Cell>> {
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
        ("Fees (raw compatibility projection)", "/metrics/fees_raw"),
        ("Fees exact by currency", "/metrics_exact/fees_by_currency"),
        (
            "Turnover (raw compatibility projection)",
            "/metrics/turnover_raw",
        ),
        ("Turnover exact", "/metrics_exact/turnover/amount"),
        ("Turnover currency", "/metrics_exact/turnover/currency"),
        ("Initial equity exact", "/metrics_exact/equity/initial"),
        ("Final equity exact", "/metrics_exact/equity/final"),
        (
            "Metrics definition version",
            "/metrics_definition/schema_version",
        ),
        (
            "Risk ratio period basis",
            "/metrics_definition/risk_ratio_period_basis",
        ),
        (
            "Benchmark period basis",
            "/metrics_definition/benchmark_period_basis",
        ),
        ("Exact fee basis", "/metrics_definition/fees_exact_basis"),
        ("Raw fee basis", "/metrics_definition/fees_raw_basis"),
        ("Fills", "/fills"),
        ("Rejected orders", "/rejected_orders"),
    ] {
        let Some(value) = summary.pointer(pointer).or_else(|| {
            pointer
                .strip_prefix("/metrics/")
                .and_then(|key| summary.get(key))
        }) else {
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

#[cfg(test)]
mod metrics_projection_tests {
    use super::{overview_rows, Cell};

    #[test]
    fn overview_exports_versioned_metric_definitions_and_exact_fee_facts() {
        let summary = serde_json::json!({
            "metrics": {"fees_raw": 17, "return_bps": 125, "turnover_raw": 17},
            "metrics_exact": {
                "fees_by_currency": {"USDT": "0.00000017"},
                "equity": {"initial": "100.00", "final": "101.25"},
                "turnover": {"amount": "0.00000017", "currency": "USDT"}
            },
            "metrics_definition": {
                "schema_version": 1,
                "risk_ratio_period_basis": "per_sample_no_annualization",
                "benchmark_period_basis": "exact_timestamp_aligned_observations_no_annualization",
                "fees_exact_basis": "ledger_fee_entries_grouped_by_currency",
                "fees_raw_basis": "compatibility_projection_only"
            }
        });

        let rows = overview_rows(&summary, None);
        let value = |label: &str| {
            rows.iter()
                .find(|row| matches!(row.first(), Some(Cell::Text(text)) if text == label))
                .and_then(|row| row.get(1))
                .expect("expected summary field")
        };

        assert!(matches!(value("Metrics definition version"), Cell::Number(v) if v == "1"));
        assert!(
            matches!(value("Risk ratio period basis"), Cell::Text(v) if v == "per_sample_no_annualization")
        );
        assert!(
            matches!(value("Fees exact by currency"), Cell::Text(v) if v == r#"{"USDT":"0.00000017"}"#)
        );
        assert!(
            matches!(value("Fees (raw compatibility projection)"), Cell::Number(v) if v == "17")
        );

        let html = crate::report_html::render_report_html(&summary, &[], &[], &[]);
        assert!(html.contains("0.00000017 USDT"));
        assert!(html.contains("per_sample_no_annualization"));
        assert!(html.contains("100.00"));
    }
}

pub(super) fn equity_rows(equity: &[(i64, i128, Option<i128>)]) -> Vec<Vec<Cell>> {
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

pub(super) fn fill_rows(fills: &[crate::report_html::FillPoint]) -> Vec<Vec<Cell>> {
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

pub(super) fn benchmark_rows(analysis: &Value) -> Result<Vec<Vec<Cell>>, String> {
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
