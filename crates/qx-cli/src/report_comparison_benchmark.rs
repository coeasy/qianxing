//! Shared benchmark analysis for same-input multi-run comparisons.

use qx_app::report_analysis::{ComparisonBenchmark as AppComparisonBenchmark, ComparisonEntry};
use serde_json::Value;
use std::path::Path;

pub(crate) fn rank_by_active_return(entries: &mut [ComparisonEntry]) {
    qx_app::report_analysis::rank_comparison_by_active_return(entries);
}

pub(crate) fn bind(
    entries: &mut [ComparisonEntry],
    benchmark_path: &Path,
    calendar_path: Option<&Path>,
) -> Result<Vec<Value>, String> {
    let mut analyses = Vec::with_capacity(entries.len());
    for entry in entries {
        let return_bps = i64::try_from(entry.return_bps)
            .map_err(|_| format!("摘要 {} 的收益率超出 i64 范围", entry.summary_path))?;
        let analysis = crate::report_benchmark::benchmark_analysis(
            Path::new(&entry.summary_path),
            benchmark_path,
            return_bps,
            calendar_path,
        )?;
        let metrics = analysis.get("metrics").ok_or("基准分析缺少 metrics")?;
        let daily = analysis.get("period_basis").and_then(Value::as_str)
            == Some("per_trading_day_no_annualization");
        let metric = |bar: &str, day: &str| {
            metrics
                .get(if daily { day } else { bar })
                .and_then(Value::as_f64)
        };
        entry.benchmark = Some(AppComparisonBenchmark {
            instrument: text(&analysis, "/benchmark/instrument")?,
            fingerprint: text(&analysis, "/benchmark/fingerprint")?,
            price_basis: text(&analysis, "/benchmark/price_basis")?,
            alignment: text(&analysis, "/alignment")?,
            period_basis: text(&analysis, "/period_basis")?,
            benchmark_return_bps: metrics.get("benchmark_return_bps").and_then(Value::as_f64),
            active_return_bps: metrics.get("active_return_bps").and_then(Value::as_f64),
            mean_active_return_bps: metric(
                "mean_active_return_bps_per_bar",
                "mean_active_return_bps_per_trading_day",
            ),
            tracking_error_bps: metric(
                "tracking_error_bps_per_bar",
                "tracking_error_bps_per_trading_day",
            ),
            information_ratio: metric(
                "information_ratio_per_bar",
                "information_ratio_per_trading_day",
            ),
        });
        analyses.push(analysis);
    }
    Ok(analyses)
}

pub(crate) fn write_csv(
    analyses: &[Value],
    entries: &[ComparisonEntry],
    output: &Path,
) -> Result<(), String> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建基准比较 CSV 目录失败 {}: {error}", parent.display()))?;
    let mut csv = String::from(
        "run_id,benchmark_instrument,benchmark_fingerprint,price_basis,alignment,period_basis,start_ts,end_ts,portfolio_equity_start_raw,portfolio_equity_end_raw,benchmark_close_start_raw,benchmark_close_end_raw,portfolio_return_bps,benchmark_return_bps,active_return_bps\n",
    );
    for (analysis, entry) in analyses.iter().zip(entries) {
        let instrument = text(analysis, "/benchmark/instrument")?;
        let fingerprint = text(analysis, "/benchmark/fingerprint")?;
        let price_basis = text(analysis, "/benchmark/price_basis")?;
        let alignment = text(analysis, "/alignment")?;
        let period_basis = text(analysis, "/period_basis")?;
        for period in analysis
            .get("series")
            .and_then(Value::as_array)
            .ok_or("基准分析缺少 series")?
        {
            let fields = [
                Value::String(entry.run_id.clone()),
                Value::String(instrument.clone()),
                Value::String(fingerprint.clone()),
                Value::String(price_basis.clone()),
                Value::String(alignment.clone()),
                Value::String(period_basis.clone()),
                required(period, "start_ts")?,
                required(period, "end_ts")?,
                required(period, "portfolio_equity_start_raw")?,
                required(period, "portfolio_equity_end_raw")?,
                required(period, "benchmark_close_start_raw")?,
                required(period, "benchmark_close_end_raw")?,
                required(period, "portfolio_return_bps")?,
                required(period, "benchmark_return_bps")?,
                required(period, "active_return_bps")?,
            ];
            csv.push_str(&fields.iter().map(csv_cell).collect::<Vec<_>>().join(","));
            csv.push('\n');
        }
    }
    std::fs::write(output, csv)
        .map_err(|error| format!("写入基准比较 CSV 失败 {}: {error}", output.display()))
}

fn text(value: &Value, pointer: &str) -> Result<String, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("基准分析缺少 {pointer}"))
}

fn required(value: &Value, key: &str) -> Result<Value, String> {
    value
        .get(key)
        .cloned()
        .ok_or_else(|| format!("基准序列缺少 {key}"))
}

fn csv_cell(value: &Value) -> String {
    let raw = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    if raw.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", raw.replace('"', "\"\""))
    } else {
        raw
    }
}
