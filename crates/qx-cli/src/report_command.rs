//! 回测报告命令：摘要复核、人读/机读投影、可选 HTML 导出与可选运行证据包。

mod diagnostics;
mod verified_summary;
mod workflow;

use self::diagnostics::read_bound_strategy_diagnostics;
pub(crate) use diagnostics::{
    validate_strategy_diagnostics_identity, write_app_backtest_diagnostic_html,
};

pub(crate) use self::verified_summary::require_current_summary_schema;
use self::verified_summary::{read_verified_bar_summary, read_verified_depth_summary};
pub(crate) use self::workflow::run_report_with_benchmark;

use crate::*;
use std::path::{Path, PathBuf};

/// The sole outlet for rendering a research HTML report, shared by both native and app backtests.
pub(super) fn write_research_report_html(
    summary_path: &Path,
    summary: &serde_json::Value,
    output: Option<&Path>,
) -> Result<crate::report_html::ReportArtifacts, String> {
    crate::report_html::write_report_html(summary_path, summary, output)
}

pub(crate) struct ReportOptions<'a> {
    pub(crate) as_json: bool,
    pub(crate) html: bool,
    pub(crate) evidence: bool,
    pub(crate) output: Option<&'a Path>,
    pub(crate) against: &'a [PathBuf],
    pub(crate) benchmark_bars: Option<&'a Path>,
    pub(crate) benchmark_calendar: Option<&'a Path>,
    pub(crate) benchmark_csv: Option<&'a Path>,
    pub(crate) strategy_diagnostics: Option<&'a Path>,
    pub(crate) xlsx: Option<&'a Path>,
}

pub(crate) fn run_report(path: &Path, as_json: bool, html: bool) -> Result<(), String> {
    run_report_with_output(path, as_json, html, false, None, &[])
}

pub(crate) fn run_report_with_output(
    path: &Path,
    as_json: bool,
    html: bool,
    evidence: bool,
    output: Option<&Path>,
    against: &[PathBuf],
) -> Result<(), String> {
    run_report_with_benchmark(
        path,
        ReportOptions {
            as_json,
            html,
            evidence,
            output,
            against,
            benchmark_bars: None,
            benchmark_calendar: None,
            benchmark_csv: None,
            strategy_diagnostics: None,
            xlsx: None,
        },
    )
}

pub(crate) fn run_unified_report_entry(
    arguments: &[String],
    default: impl FnOnce() -> PathBuf,
) -> Result<(), String> {
    let (
        path,
        json,
        html,
        evidence,
        output,
        against,
        benchmark_bars,
        benchmark_calendar,
        benchmark_csv,
        xlsx,
        strategy_diagnostics,
    ) = run_report_entry_arguments(arguments, default)?;
    run_report_with_benchmark(
        &path,
        ReportOptions {
            as_json: json,
            html,
            evidence,
            output: output.as_deref(),
            against: &against,
            benchmark_bars: benchmark_bars.as_deref(),
            benchmark_calendar: benchmark_calendar.as_deref(),
            benchmark_csv: benchmark_csv.as_deref(),
            strategy_diagnostics: strategy_diagnostics.as_deref(),
            xlsx: xlsx.as_deref(),
        },
    )
}

pub(crate) fn dispatch_report(args: super::report_args::ReportArgs) {
    let path = args.path.unwrap_or_else(default_runtime_path);
    if let Err(error) = run_report_with_benchmark(
        &path,
        ReportOptions {
            as_json: args.json,
            html: args.html,
            evidence: args.evidence,
            output: args.out.as_deref(),
            against: &args.against,
            benchmark_bars: args.benchmark_bars.as_deref(),
            benchmark_calendar: args.benchmark_calendar.as_deref(),
            benchmark_csv: args.benchmark_csv.as_deref(),
            strategy_diagnostics: args.strategy_diagnostics.as_deref(),
            xlsx: args.xlsx.as_deref(),
        },
    ) {
        eprintln!("[qx-cli · CLI] 报告查看失败: {error}");
        std::process::exit(2);
    }
}
