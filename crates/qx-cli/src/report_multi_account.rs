//! Account-isolated research report for persisted multi-account Bar results.

use crate::escape_html;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const MAX_REPORT_FILL_ROWS_PER_ACCOUNT: usize = 5_000;
const MAX_REPORT_FILL_ROWS_TOTAL: usize = 50_000;
const MAX_REPORT_EVENT_ROWS_PER_ACCOUNT: usize = 5_000;
const MAX_REPORT_EVENT_ROWS_TOTAL: usize = 50_000;

// Keep CLI report options explicit; the caller owns their parsing and validation.
#[allow(clippy::too_many_arguments)]
pub(crate) fn maybe_run(
    path: &Path,
    as_json: bool,
    html: bool,
    evidence: bool,
    output: Option<&Path>,
    against: &[PathBuf],
    xlsx: Option<&Path>,
    has_benchmark: bool,
    strategy_diagnostics: Option<&Path>,
) -> Result<bool, String> {
    let payload = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(false),
    };
    let Ok(value) = serde_json::from_slice::<Value>(&payload) else {
        return Ok(false);
    };
    if value.get("batch_id").and_then(Value::as_str).is_none()
        || value.get("runs").and_then(Value::as_array).is_none()
        || value.get("summary").and_then(Value::as_object).is_none()
    {
        return Ok(false);
    }
    if !against.is_empty() || has_benchmark {
        return Err("多账户报告暂不支持 --against 或基准参数；请对账户运行分别生成对照报告".into());
    }
    if output.is_some() && !html {
        return Err("--output 只能与 --html 一起使用".into());
    }
    if value.get("result_kind").and_then(Value::as_str) == Some("multi_account_depth")
        || value
            .get("runs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|run| {
                run.pointer("/outcome/tier")
                    .and_then(Value::as_str)
                    .is_some()
            })
    {
        return depth::run_depth_report(
            path,
            &value,
            as_json,
            html,
            evidence,
            output,
            against,
            xlsx,
            has_benchmark,
            strategy_diagnostics,
        );
    }
    bar::run_bar_report(
        path,
        value,
        as_json,
        html,
        evidence,
        output,
        xlsx,
        strategy_diagnostics,
    )
}

#[derive(Clone, Copy)]
struct AccountReportArtifactPaths<'a> {
    run_manifest: &'a str,
    summary: &'a str,
    equity: &'a str,
    fills: &'a str,
    event_trace: Option<&'a str>,
    account_id: Option<&'a str>,
}

fn validate_account_report_output_target<'a>(
    target: &Path,
    source: &Path,
    strategy_diagnostics: Option<&Path>,
    evidence: bool,
    protect_account_diagnostic_snapshots: bool,
    input_label: &str,
    runs: impl IntoIterator<Item = AccountReportArtifactPaths<'a>>,
) -> Result<(), String> {
    if crate::report_output_paths::same_output_target(target, source) {
        return Err(format!("报告不能覆盖{input_label}"));
    }
    if strategy_diagnostics.is_some_and(|diagnostics| {
        crate::report_output_paths::same_output_target(target, diagnostics)
    }) {
        return Err("报告不能覆盖策略诊断输入".into());
    }
    for run in runs {
        for artifact in [run.run_manifest, run.summary, run.equity, run.fills]
            .into_iter()
            .chain(run.event_trace)
        {
            if crate::report_output_paths::same_output_target(target, Path::new(artifact)) {
                return Err(format!("报告不能覆盖账户运行工件: {artifact}"));
            }
        }
        if evidence {
            let summary_path = Path::new(run.summary);
            let stem = evidence_summary_stem(summary_path)?;
            for suffix in [".evidence.json", ".strategy-diagnostics.evidence.json"] {
                let evidence_path = summary_path.with_file_name(format!("{stem}{suffix}"));
                if crate::report_output_paths::same_output_target(target, &evidence_path) {
                    return Err(format!(
                        "报告不能覆盖账户运行 EvidenceBundle: {}",
                        evidence_path.display()
                    ));
                }
            }
            if protect_account_diagnostic_snapshots {
                if let Some(account_id) = run.account_id {
                    let diagnostic_path =
                        account_diagnostic_snapshot_path(summary_path, account_id)?;
                    if crate::report_output_paths::same_output_target(target, &diagnostic_path) {
                        return Err(format!(
                            "报告不能覆盖账户策略诊断证据附件: {}",
                            diagnostic_path.display()
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_depth_report_output_target(
    target: &Path,
    source: &Path,
    strategy_diagnostics: Option<&Path>,
    result: &qx_app::MultiAccountDepthBacktestResult,
    evidence: bool,
) -> Result<(), String> {
    let runs = result
        .runs
        .iter()
        .filter_map(|run| run.outcome.as_ref())
        .map(|outcome| AccountReportArtifactPaths {
            run_manifest: &outcome.artifacts.run_manifest,
            summary: &outcome.artifacts.summary,
            equity: &outcome.artifacts.equity,
            fills: &outcome.artifacts.fills,
            event_trace: outcome.artifacts.event_trace.as_deref(),
            account_id: outcome.account_id.as_deref(),
        });
    validate_account_report_output_target(
        target,
        source,
        strategy_diagnostics,
        evidence,
        true,
        "多账户深度结果输入",
        runs,
    )
}

fn validate_report_output_target(
    target: &Path,
    source: &Path,
    strategy_diagnostics: Option<&Path>,
    result: &qx_app::MultiAccountBacktestResult,
    evidence: bool,
) -> Result<(), String> {
    let runs = result
        .runs
        .iter()
        .filter_map(|run| run.outcome.as_ref())
        .map(|outcome| AccountReportArtifactPaths {
            run_manifest: &outcome.artifacts.run_manifest,
            summary: &outcome.artifacts.summary,
            equity: &outcome.artifacts.equity,
            fills: &outcome.artifacts.fills,
            event_trace: outcome.artifacts.event_trace.as_deref(),
            account_id: outcome.account_id.as_deref(),
        });
    validate_account_report_output_target(
        target,
        source,
        strategy_diagnostics,
        evidence,
        strategy_diagnostics.is_some(),
        "多账户结果输入",
        runs,
    )
}

mod bar;
mod depth;
mod details;
use details::{
    account_diagnostic_snapshot_path, evidence_summary_stem, read_account_fills,
    read_depth_account_fills, read_multi_account_diagnostics, write_account_diagnostic_snapshot,
};

mod render;
use render::{batch_row_budget, downsample_equity, render_html};
#[cfg(test)]
use render::{bounded_html_report, MAX_HTML_DETAIL_ROWS_PER_ACCOUNT};

#[cfg(test)]
mod tests {
    use super::{
        batch_row_budget, bounded_html_report, read_account_fills, MAX_HTML_DETAIL_ROWS_PER_ACCOUNT,
    };
    use serde_json::json;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn html_detail_projection_is_bounded_and_drops_links_to_hidden_events() {
        let mut fills = (0..300)
            .map(|index| {
                json!({
                    "timestamp_ms": index,
                    "event_id": format!("event-{index}"),
                    "event_seq": index
                })
            })
            .collect::<Vec<_>>();
        fills[299]["event_id"] = json!("event-0");
        fills[299]["event_seq"] = json!(0);
        let events = (0..300)
            .map(|index| json!({"event_id": format!("event-{index}"), "seq": index}))
            .collect::<Vec<_>>();
        let report = json!({
            "accounts": [{
                "fill_rows_total": 450,
                "event_rows_total": 9_000,
                "fill_rows": fills,
                "event_rows": events
            }]
        });

        let html_report = bounded_html_report(&report);
        let account = &html_report["accounts"][0];
        assert_eq!(
            account["fill_rows"].as_array().unwrap().len(),
            MAX_HTML_DETAIL_ROWS_PER_ACCOUNT
        );
        assert_eq!(
            account["event_rows"].as_array().unwrap().len(),
            MAX_HTML_DETAIL_ROWS_PER_ACCOUNT
        );
        assert_eq!(account["fill_rows"][0]["timestamp_ms"], 50);
        assert_eq!(account["fill_rows"][0]["event_id"], "event-50");
        assert!(account["fill_rows"][249].get("event_id").is_none());
        assert_eq!(account["fill_rows_total"], 450);
        assert_eq!(account["event_rows_total"], 9_000);
        assert_eq!(
            report["accounts"][0]["fill_rows"].as_array().unwrap().len(),
            300
        );
    }

    #[test]
    fn report_fill_reader_keeps_only_the_recent_bounded_window_and_checks_total() {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::current_dir()
            .unwrap()
            .join("target/report-fill-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!(
            "qx-report-fill-{}-{}.csv",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "ts,order_id,qty_raw,price_raw,fee_raw,strategy_id").unwrap();
        for index in 0..10 {
            writeln!(file, "{index},{index},1,2,0,strategy").unwrap();
        }
        drop(file);
        let path_text = path.to_str().unwrap();
        let (rows, total) = read_account_fills(path_text, 10, 3).unwrap();
        assert_eq!(total, 10);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["timestamp_ms"], 7);
        assert_eq!(rows[2]["timestamp_ms"], 9);
        assert!(read_account_fills(path_text, 9, 3).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn report_row_budgets_are_fair_and_never_exceed_the_batch_limit() {
        for accounts in [2, 3, 11, 32] {
            let limits = (0..accounts)
                .map(|index| batch_row_budget(50_000, accounts, index, 5_000))
                .collect::<Vec<_>>();
            assert!(limits.windows(2).all(|pair| pair[0] >= pair[1]));
            assert!(limits.iter().all(|limit| *limit <= 5_000));
            assert!(limits.iter().sum::<usize>() <= 50_000);
        }
        assert_eq!(
            (0..32)
                .map(|index| batch_row_budget(50_000, 32, index, 5_000))
                .sum::<usize>(),
            50_000
        );
    }
}
