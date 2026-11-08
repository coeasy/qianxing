//! Offline research report for shared-account multi-market Bar runs.

use crate::report_output_paths::same_output_target;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

mod diagnostics;
mod projection;
mod publication;
use projection::{build_report, market_ids, resolve_path};
mod render;
pub(crate) use diagnostics::generate_builtin_bar_diagnostics;
#[cfg(test)]
use diagnostics::parse_market_bar_comparison_diagnostics;
use diagnostics::{
    bind_market_bar_comparison_diagnostics, bind_market_bar_diagnostics_value,
    read_market_bar_diagnostics,
};

const MAX_MULTI_MARKET_EVENTS: u64 = 5_000_000;
const MAX_MULTI_MARKET_EVENT_PAGE: usize = 300;

fn read_artifact(path: &Path) -> Result<Option<Value>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes =
        std::fs::read(path).map_err(|e| format!("读取报告输入失败 {}: {e}", path.display()))?;
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(None);
    };
    let version = value.get("schema_version").and_then(Value::as_u64);
    let has_event_source = match version {
        Some(1) => value.get("event_log").and_then(Value::as_array).is_some(),
        Some(2) => {
            value
                .get("event_trace_path")
                .and_then(Value::as_str)
                .is_some()
                && value.get("event_count").and_then(Value::as_u64).is_some()
                && value.get("event_log").is_none()
        }
        _ => false,
    };
    if matches!(version, Some(1 | 2))
        && value.pointer("/outcome/shared_result_hash").is_some()
        && value.pointer("/outcome/pending_orders").is_none()
        && value.get("market_data").and_then(Value::as_array).is_some()
        && value.pointer("/spec/markets").is_some()
        && has_event_source
    {
        Ok(Some(value))
    } else {
        Ok(None)
    }
}

// Keep CLI report options explicit; the caller owns their parsing and validation.
#[allow(clippy::too_many_arguments)]
pub(crate) fn maybe_run(
    path: &Path,
    as_json: bool,
    html: bool,
    evidence: bool,
    output: Option<&Path>,
    against: &[PathBuf],
    benchmark_bars: Option<&Path>,
    benchmark_calendar: Option<&Path>,
    benchmark_csv: Option<&Path>,
    xlsx: Option<&Path>,
    strategy_diagnostics: Option<&Path>,
) -> Result<bool, String> {
    let Some(artifact) = read_artifact(path)? else {
        return Ok(false);
    };
    if evidence && !against.is_empty() {
        return Err("多市场 --against 横向对照不生成单次运行 EvidenceBundle".into());
    }
    if strategy_diagnostics.is_some() && !html && !as_json && xlsx.is_none() {
        return Err("--strategy-diagnostics 必须与 --html、--json 或 --xlsx 一起使用".into());
    }
    if output.is_some() && !html {
        return Err("--output 只能与 --html 一起使用".into());
    }
    if output.is_some_and(|h| xlsx.is_some_and(|x| same_output_target(h, x))) {
        return Err("HTML 与 XLSX 报告必须使用不同输出路径".into());
    }
    if benchmark_csv.is_some_and(|csv| {
        output.is_some_and(|html| same_output_target(csv, html))
            || xlsx.is_some_and(|workbook| same_output_target(csv, workbook))
    }) {
        return Err("HTML、XLSX 和基准 CSV 必须使用不同输出路径".into());
    }
    let mut reports = vec![build_report(path, &artifact)?];
    let mut seen = std::collections::BTreeSet::from([path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())]);
    for candidate in against {
        let canonical = candidate
            .canonicalize()
            .map_err(|e| format!("解析对照路径失败 {}: {e}", candidate.display()))?;
        if !seen.insert(canonical) {
            return Err(format!("多市场对照不能重复输入: {}", candidate.display()));
        }
        let Some(value) = read_artifact(candidate)? else {
            return Err(format!(
                "--against 只接受多市场 Bar 运行产物: {}",
                candidate.display()
            ));
        };
        reports.push(build_report(candidate, &value)?);
    }
    let comparison = reports.len() > 1;
    if comparison {
        let fingerprint = reports[0]["data_fingerprint"].clone();
        let instruments = market_ids(&reports[0]);
        if reports
            .iter()
            .any(|r| r["data_fingerprint"] != fingerprint || market_ids(r) != instruments)
        {
            return Err("多市场 Bar 对照要求市场顺序和数据指纹完全相同".into());
        }
        reports.sort_by(|a, b| {
            b["return_bps"]
                .as_i64()
                .cmp(&a["return_bps"].as_i64())
                .then_with(|| {
                    a["max_drawdown_bps"]
                        .as_u64()
                        .cmp(&b["max_drawdown_bps"].as_u64())
                })
                .then_with(|| a["run_id"].as_str().cmp(&b["run_id"].as_str()))
        });
        if let Some(diagnostics_path) = strategy_diagnostics {
            bind_market_bar_comparison_diagnostics(diagnostics_path, &mut reports)?;
        }
    }
    if let Some(benchmark_path) = benchmark_bars {
        for report in &mut reports {
            let portfolio =
                crate::report_benchmark::portfolio_from_report_equity(&report["equity"])?;
            let window = (
                portfolio.first().ok_or("多市场权益序列为空")?.0,
                portfolio.last().ok_or("多市场权益序列为空")?.0,
            );
            let return_bps = report["return_bps"]
                .as_i64()
                .ok_or("多市场报告 return_bps 非法，不能计算基准超额收益")?;
            report["benchmark_analysis"] =
                crate::report_benchmark::benchmark_analysis_for_portfolio(
                    &portfolio,
                    benchmark_path,
                    return_bps,
                    benchmark_calendar,
                    window,
                )?;
        }
    }
    let mut protected_sources = reports
        .iter()
        .flat_map(|report| {
            let summary = report["summary_path"]
                .as_str()
                .map(PathBuf::from)
                .into_iter();
            let inputs = report["input_artifacts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(PathBuf::from);
            summary.chain(inputs)
        })
        .collect::<Vec<_>>();
    protected_sources.extend(strategy_diagnostics.map(Path::to_path_buf));
    protected_sources.extend(benchmark_bars.map(Path::to_path_buf));
    protected_sources.extend(benchmark_calendar.map(Path::to_path_buf));
    let mut report = if comparison {
        json!({"comparison_kind":"same_multi_market_bar_input","data_fingerprint":reports[0]["data_fingerprint"],
            "markets":market_ids(&reports[0]),"market_sources":reports[0]["markets"],"runs":reports})
    } else {
        reports.remove(0)
    };
    let mut diagnostics_payload = None;
    if !comparison {
        if let Some(diagnostics_path) = strategy_diagnostics {
            let payload = read_market_bar_diagnostics(diagnostics_path)?;
            report["market_strategy_diagnostics"] =
                bind_market_bar_diagnostics_value(&payload, &artifact, &report)?;
            diagnostics_payload = Some(payload);
        }
    }
    let generated = publication::publish_report(
        path,
        &report,
        comparison,
        evidence,
        html,
        output,
        benchmark_csv,
        xlsx,
        strategy_diagnostics,
        diagnostics_payload.as_ref(),
        &protected_sources,
    )?;
    if as_json {
        let mut result = json!({"schema_version":1,"report_kind":if comparison {"multi_market_bar_comparison"} else {"multi_market_bar"},"report":report});
        if !generated.is_empty() {
            result["generated_artifacts"] = Value::Object(generated.clone());
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?
        );
    } else if comparison {
        println!(
            "[Report] comparison_fingerprint={} compared_runs={}",
            report["data_fingerprint"],
            report["runs"].as_array().map_or(0, Vec::len)
        );
    } else {
        println!(
            "[Report] run_id={} return_bps={} max_drawdown_bps={} markets={}",
            report["run_id"],
            report["return_bps"],
            report["max_drawdown_bps"],
            report["markets"].as_array().map_or(0, Vec::len)
        );
    }
    if !as_json {
        for (kind, path) in generated {
            println!("[Report] {kind}={}", path.as_str().unwrap_or(""));
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::diagnostics::render_comparison_diagnostics;
    use super::parse_market_bar_comparison_diagnostics;
    use super::render::bound_html_series;
    use serde_json::{json, Value};

    #[test]
    fn html_projection_is_bounded_and_keeps_equity_endpoints_and_recent_fills() {
        let equity = (0..10_001)
            .map(|value| json!([value, value]))
            .collect::<Vec<_>>();
        let fills = (0..3_001).map(Value::from).collect::<Vec<_>>();
        let events = (0..3_001).map(Value::from).collect::<Vec<_>>();
        let mut report = json!({"equity":equity,"fills":fills,"events":events});
        bound_html_series(&mut report);
        let equity = report["equity"].as_array().unwrap();
        let fills = report["fills"].as_array().unwrap();
        let events = report["events"].as_array().unwrap();
        assert!(equity.len() <= 2_000);
        assert_eq!(equity.first().unwrap()[0], 0);
        assert_eq!(equity.last().unwrap()[0], 10_000);
        assert_eq!(fills.len(), 2_000);
        assert_eq!(fills.first(), Some(&json!(1_001)));
        assert_eq!(fills.last(), Some(&json!(3_000)));
        assert_eq!(events.len(), 2_000);
        assert_eq!(events.first(), Some(&json!(1_001)));
        assert_eq!(events.last(), Some(&json!(3_000)));
    }

    #[test]
    fn multi_market_bar_comparison_diagnostics_require_unique_run_ids() {
        let payload = json!({
            "schema_version": 1,
            "kind": "qianxing-multi-market-bar-diagnostic-comparison",
            "runs": [
                {"run_id":"run-a","diagnostics":[]},
                {"run_id":"run-b","diagnostics":[]}
            ]
        });
        let parsed = parse_market_bar_comparison_diagnostics(&payload).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains_key("run-a"));

        let duplicate = json!({
            "schema_version": 1,
            "kind": "qianxing-multi-market-bar-diagnostic-comparison",
            "runs": [
                {"run_id":"run-a","diagnostics":[]},
                {"run_id":"run-a","diagnostics":[]}
            ]
        });
        assert!(parse_market_bar_comparison_diagnostics(&duplicate)
            .unwrap_err()
            .contains("重复 run_id"));
    }

    #[test]
    fn multi_market_bar_comparison_diagnostics_render_run_identity_and_escape_evidence() {
        let report = json!({
            "runs": [{
                "run_id":"run-a",
                "market_strategy_diagnostics":[{
                    "instrument":"BTCUSDT.BINANCE",
                    "strategy_id":"<strategy>",
                    "report":{
                        "input_kind":"bar",
                        "input_bars":12,
                        "as_of":99,
                        "lookahead":{"status":"no_change_detected"},
                        "startup":{"status":"startup_sensitive"},
                        "data_visibility":{"status":"visibility_unknown"},
                        "evidence":"<img src=x>"
                    }
                }]
            }]
        });
        let html = render_comparison_diagnostics(&report);
        assert!(html.contains("run-a"));
        assert!(html.contains("逐运行策略诊断对照"));
        assert!(html.contains("&lt;strategy&gt;"));
        assert!(html.contains("&lt;img src=x&gt;"));
        assert!(!html.contains("<img src=x>"));
    }
}
