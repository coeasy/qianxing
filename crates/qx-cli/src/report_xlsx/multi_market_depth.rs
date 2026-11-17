//! Multi-market depth and comparison workbook projection and publication.

use super::*;

pub(crate) fn write_multi_market_depth_xlsx(
    output: &Path,
    overview: Vec<Vec<String>>,
    quality: Vec<Vec<String>>,
    strategy_diagnostics: Option<&Value>,
    benchmark_runs: Option<&[(String, Value)]>,
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
    if let Some(benchmark_runs) = benchmark_runs.filter(|runs| !runs.is_empty()) {
        let mut rows = vec![[
            "run_id",
            "benchmark_instrument",
            "benchmark_fingerprint",
            "price_basis",
            "alignment",
            "period_basis",
            "start_ts",
            "end_ts",
            "portfolio_equity_start_raw",
            "portfolio_equity_end_raw",
            "benchmark_close_start_raw",
            "benchmark_close_end_raw",
            "portfolio_return_bps",
            "benchmark_return_bps",
            "active_return_bps",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()];
        for (run_id, analysis) in benchmark_runs {
            let series = analysis
                .get("series")
                .and_then(Value::as_array)
                .ok_or("基准分析缺少逐期序列，不能写入多市场 XLSX")?;
            let metadata = [
                analysis.pointer("/benchmark/instrument"),
                analysis.pointer("/benchmark/fingerprint"),
                analysis.pointer("/benchmark/price_basis"),
                analysis.get("alignment"),
                analysis.get("period_basis"),
            ];
            let metadata = metadata
                .into_iter()
                .map(|value| {
                    value
                        .and_then(Value::as_str)
                        .ok_or("基准分析缺少 XLSX 元数据")
                        .map(str::to_owned)
                })
                .collect::<Result<Vec<_>, _>>()?;
            for period in series {
                let mut row = vec![run_id.clone()];
                row.extend(metadata.iter().cloned());
                for key in [
                    "start_ts",
                    "end_ts",
                    "portfolio_equity_start_raw",
                    "portfolio_equity_end_raw",
                    "benchmark_close_start_raw",
                    "benchmark_close_end_raw",
                    "portfolio_return_bps",
                    "benchmark_return_bps",
                    "active_return_bps",
                ] {
                    row.push(
                        period
                            .get(key)
                            .filter(|value| !value.is_null())
                            .ok_or_else(|| format!("基准逐期序列缺少 {key}"))?
                            .to_string(),
                    );
                }
                rows.push(row);
            }
        }
        sheets.push(("Benchmark", convert(rows)));
    }
    if let Some(diagnostics) = strategy_diagnostics {
        sheets.extend(strategy_diagnostics_sheets(diagnostics)?);
    }
    write_workbook(output, &sheets)?;
    Ok(output.to_path_buf())
}

pub(crate) fn write_multi_market_depth_comparison_xlsx(
    output: &Path,
    overview: Vec<Vec<String>>,
    ranking: Vec<Vec<String>>,
    diagnostic_runs: Option<&Value>,
    benchmark_runs: Option<&[(String, Value)]>,
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
    if let Some(benchmark_runs) = benchmark_runs.filter(|runs| !runs.is_empty()) {
        let mut rows = vec![[
            "run_id",
            "benchmark_instrument",
            "benchmark_fingerprint",
            "price_basis",
            "alignment",
            "period_basis",
            "start_ts",
            "end_ts",
            "portfolio_equity_start_raw",
            "portfolio_equity_end_raw",
            "benchmark_close_start_raw",
            "benchmark_close_end_raw",
            "portfolio_return_bps",
            "benchmark_return_bps",
            "active_return_bps",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()];
        for (run_id, analysis) in benchmark_runs {
            let series = analysis
                .get("series")
                .and_then(Value::as_array)
                .ok_or("基准分析缺少逐期序列，不能写入多市场比较 XLSX")?;
            let metadata = [
                analysis.pointer("/benchmark/instrument"),
                analysis.pointer("/benchmark/fingerprint"),
                analysis.pointer("/benchmark/price_basis"),
                analysis.get("alignment"),
                analysis.get("period_basis"),
            ];
            let metadata = metadata
                .into_iter()
                .map(|value| {
                    value
                        .and_then(Value::as_str)
                        .ok_or("基准分析缺少 XLSX 元数据")
                        .map(str::to_owned)
                })
                .collect::<Result<Vec<_>, _>>()?;
            for period in series {
                let mut row = vec![run_id.clone()];
                row.extend(metadata.iter().cloned());
                for key in [
                    "start_ts",
                    "end_ts",
                    "portfolio_equity_start_raw",
                    "portfolio_equity_end_raw",
                    "benchmark_close_start_raw",
                    "benchmark_close_end_raw",
                    "portfolio_return_bps",
                    "benchmark_return_bps",
                    "active_return_bps",
                ] {
                    row.push(
                        period
                            .get(key)
                            .filter(|value| !value.is_null())
                            .ok_or_else(|| format!("基准逐期序列缺少 {key}"))?
                            .to_string(),
                    );
                }
                rows.push(row);
            }
        }
        sheets.push(("Benchmark", convert(rows)));
    }
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
