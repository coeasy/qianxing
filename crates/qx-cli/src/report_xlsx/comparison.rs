use super::*;

pub(crate) fn write_comparison_xlsx(
    instrument: &str,
    data_fingerprint: &str,
    entries: &[crate::report_comparison::ComparisonEntry],
    output: &Path,
) -> Result<PathBuf, String> {
    validate_xlsx_path(output)?;
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
    for entry in entries {
        let input = Path::new(&entry.summary_path)
            .canonicalize()
            .map_err(|error| format!("解析对照摘要路径失败 {}: {error}", entry.summary_path))?;
        if input == output_absolute {
            return Err(format!("XLSX 输出不能覆盖对照摘要: {}", entry.summary_path));
        }
    }
    let mut overview = vec![vec![t("Field"), t("Value")]];
    overview.push(vec![t("Instrument"), t(instrument)]);
    overview.push(vec![t("Data fingerprint"), t(data_fingerprint)]);
    overview.push(vec![
        t("Comparison kind"),
        t(if entries.iter().any(|entry| entry.benchmark.is_some()) {
            "same_input_backtest_runs_vs_benchmark"
        } else {
            "same_input_backtest_runs"
        }),
    ]);
    overview.push(vec![
        t("Ranking"),
        t("return_bps_desc,max_drawdown_bps_asc,run_id_asc"),
    ]);
    let mut runs = vec![vec![
        t("rank"),
        t("benchmark_rank"),
        t("run_id"),
        t("strategy_id"),
        t("return_bps"),
        t("max_drawdown_bps"),
        t("fees_raw"),
        t("turnover_raw"),
        t("final_equity_raw"),
        t("fills"),
        t("sharpe"),
        t("sortino"),
        t("calmar"),
        t("win_rate"),
        t("profit_factor"),
        t("benchmark_instrument"),
        t("benchmark_fingerprint"),
        t("benchmark_price_basis"),
        t("benchmark_alignment"),
        t("benchmark_return_bps"),
        t("active_return_bps"),
        t("mean_active_return_bps"),
        t("tracking_error_bps"),
        t("information_ratio"),
        t("benchmark_period_basis"),
        t("result_hash"),
        t("summary_path"),
    ]];
    for entry in entries {
        runs.push(vec![
            n(entry.rank.to_string()),
            optional_integer(entry.benchmark_rank.map(|rank| rank as i128)),
            t(&entry.run_id),
            optional_text(entry.strategy_id.as_deref()),
            n(entry.return_bps.to_string()),
            n(entry.max_drawdown_bps.to_string()),
            optional_integer(entry.fees_raw),
            optional_integer(entry.turnover_raw),
            optional_integer(entry.final_equity_raw),
            optional_integer(entry.fills),
            optional_float(entry.sharpe),
            optional_float(entry.sortino),
            optional_float(entry.calmar),
            optional_float(entry.win_rate),
            optional_float(entry.profit_factor),
            optional_text(
                entry
                    .benchmark
                    .as_ref()
                    .map(|value| value.instrument.as_str()),
            ),
            optional_text(
                entry
                    .benchmark
                    .as_ref()
                    .map(|value| value.fingerprint.as_str()),
            ),
            optional_text(
                entry
                    .benchmark
                    .as_ref()
                    .map(|value| value.price_basis.as_str()),
            ),
            optional_text(
                entry
                    .benchmark
                    .as_ref()
                    .map(|value| value.alignment.as_str()),
            ),
            optional_float(
                entry
                    .benchmark
                    .as_ref()
                    .and_then(|value| value.benchmark_return_bps),
            ),
            optional_float(
                entry
                    .benchmark
                    .as_ref()
                    .and_then(|value| value.active_return_bps),
            ),
            optional_float(
                entry
                    .benchmark
                    .as_ref()
                    .and_then(|value| value.mean_active_return_bps),
            ),
            optional_float(
                entry
                    .benchmark
                    .as_ref()
                    .and_then(|value| value.tracking_error_bps),
            ),
            optional_float(
                entry
                    .benchmark
                    .as_ref()
                    .and_then(|value| value.information_ratio),
            ),
            optional_text(
                entry
                    .benchmark
                    .as_ref()
                    .map(|value| value.period_basis.as_str()),
            ),
            optional_text(entry.result_hash.as_deref()),
            t(&entry.summary_path),
        ]);
    }
    write_workbook(output, &[("Overview", overview), ("Ranking", runs)])?;
    Ok(output.to_path_buf())
}
