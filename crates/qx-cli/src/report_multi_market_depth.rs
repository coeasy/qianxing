//! Quality and portfolio readout for persisted multi-market depth runs.

pub(super) use crate::report_output_paths::same_output_target;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

mod diagnostics;
mod projection;
mod publication;
use diagnostics::{bind_depth_comparison_diagnostics, bind_market_depth_diagnostics};
#[cfg(test)]
use diagnostics::{bind_market_depth_diagnostics_value, parse_depth_comparison_diagnostics};
pub(crate) use diagnostics::{
    generate_builtin_depth_diagnostics, verify_strategy_diagnostics_for_evidence,
};
use projection::{build_report, market_instruments};

fn read_artifact(path: &Path) -> Result<Option<Value>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let payload = std::fs::read(path)
        .map_err(|error| format!("读取报告输入失败 {}: {error}", path.display()))?;
    let Ok(value) = serde_json::from_slice::<Value>(&payload) else {
        return Ok(None);
    };
    if value.pointer("/outcome/shared_result_hash").is_some()
        && value.pointer("/outcome/pending_orders").is_some()
        && value.get("market_data").and_then(Value::as_array).is_some()
        && value.pointer("/spec/markets").is_some()
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
        return Err("多市场深度 EvidenceBundle 只支持单次运行，不能与 --against 一起使用".into());
    }
    if strategy_diagnostics.is_some() && !html && !as_json && xlsx.is_none() {
        return Err("--strategy-diagnostics 必须与 --html、--json 或 --xlsx 一起使用".into());
    }
    if output.is_some() && !html {
        return Err("--output 只能与 --html 一起使用".into());
    }
    if output.is_some_and(|html_path| {
        xlsx.is_some_and(|xlsx_path| same_output_target(html_path, xlsx_path))
    }) {
        return Err("HTML 与 XLSX 报告必须使用不同输出路径".into());
    }
    if benchmark_csv.is_some_and(|csv| {
        output.is_some_and(|html| same_output_target(csv, html))
            || xlsx.is_some_and(|workbook| same_output_target(csv, workbook))
    }) {
        return Err("HTML、XLSX 和基准 CSV 必须使用不同输出路径".into());
    }
    let include_verified_benchmark_equity = benchmark_bars.is_some();
    let mut reports = vec![build_report(
        path,
        &artifact,
        include_verified_benchmark_equity,
    )?];
    let mut seen = std::collections::BTreeSet::from([path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())]);
    for candidate in against {
        let canonical = candidate
            .canonicalize()
            .map_err(|error| format!("解析对照运行路径失败 {}: {error}", candidate.display()))?;
        if !seen.insert(canonical) {
            return Err(format!(
                "多市场深度对照不能重复输入: {}",
                candidate.display()
            ));
        }
        let Some(value) = read_artifact(candidate)? else {
            return Err(format!(
                "--against 只接受多市场深度运行产物: {}",
                candidate.display()
            ));
        };
        reports.push(build_report(
            candidate,
            &value,
            include_verified_benchmark_equity,
        )?);
    }
    let mut source_paths = reports
        .iter()
        .filter_map(|report| report["summary_path"].as_str().map(PathBuf::from))
        .collect::<Vec<_>>();
    source_paths.extend(strategy_diagnostics.map(Path::to_path_buf));
    let comparison = reports.len() > 1;
    if comparison {
        let fingerprint = reports[0]["data_fingerprint"].clone();
        let instruments = market_instruments(&reports[0])?;
        for report in &reports {
            if report["data_fingerprint"] != fingerprint
                || market_instruments(report)? != instruments
            {
                return Err("多市场深度对照要求市场顺序和数据指纹完全相同".into());
            }
        }
        reports.sort_by(|left, right| {
            right["return_bps"]
                .as_i64()
                .unwrap_or_default()
                .cmp(&left["return_bps"].as_i64().unwrap_or_default())
                .then_with(|| {
                    left["max_drawdown_bps"]
                        .as_u64()
                        .cmp(&right["max_drawdown_bps"].as_u64())
                })
                .then_with(|| left["run_id"].as_str().cmp(&right["run_id"].as_str()))
        });
    }
    if let Some(benchmark_path) = benchmark_bars {
        for item in &mut reports {
            let portfolio = crate::report_benchmark::portfolio_from_report_equity(
                &item["verified_benchmark_equity"],
            )?;
            let window = (
                portfolio.first().ok_or("多市场深度权益序列为空")?.0,
                portfolio.last().ok_or("多市场深度权益序列为空")?.0,
            );
            let return_bps = item["return_bps"]
                .as_i64()
                .ok_or("多市场深度报告 return_bps 非法，不能计算基准超额收益")?;
            item["benchmark_analysis"] = crate::report_benchmark::benchmark_analysis_for_portfolio(
                &portfolio,
                benchmark_path,
                return_bps,
                benchmark_calendar,
                window,
            )?;
            item.as_object_mut()
                .ok_or("多市场深度报告 projection 必须是对象")?
                .remove("verified_benchmark_equity");
        }
    }
    source_paths.extend(benchmark_bars.map(Path::to_path_buf));
    source_paths.extend(benchmark_calendar.map(Path::to_path_buf));
    if comparison {
        if let Some(diagnostics_path) = strategy_diagnostics {
            bind_depth_comparison_diagnostics(diagnostics_path, &mut reports)?;
        }
    }
    let mut report = if comparison {
        json!({"comparison_kind":"same_multi_market_depth_input","data_fingerprint":reports[0]["data_fingerprint"],"markets":market_instruments(&reports[0])?,"ranking":reports.iter().enumerate().map(|(index, report)|json!({"rank":index+1,"run_id":report["run_id"],"return_bps":report["return_bps"],"max_drawdown_bps":report["max_drawdown_bps"],"successful_fills":report["successful_fills"],"result_hash":report["result_hash"],"summary_path":report["summary_path"],"benchmark_analysis":report.get("benchmark_analysis").cloned().unwrap_or(Value::Null)})).collect::<Vec<_>>(),"runs":reports.iter().map(|report|json!({"run_id":report["run_id"],"benchmark_analysis":report.get("benchmark_analysis").cloned().unwrap_or(Value::Null),"market_strategy_diagnostics":report.get("market_strategy_diagnostics").cloned().unwrap_or(Value::Null)})).collect::<Vec<_>>()})
    } else {
        reports.remove(0)
    };
    let mut diagnostics_payload = None;
    if !comparison {
        if let Some(diagnostics_path) = strategy_diagnostics {
            let bound = bind_market_depth_diagnostics(diagnostics_path, &artifact, &report)?;
            diagnostics_payload = Some(Value::Array(
                bound
                    .as_array()
                    .ok_or("多市场深度诊断结果必须是数组")?
                    .iter()
                    .map(|item| item["report"].clone())
                    .collect(),
            ));
            report["market_strategy_diagnostics"] = bound;
        }
    }
    let published = publication::publish_report(
        &report,
        comparison,
        path,
        &source_paths,
        html,
        output,
        xlsx,
        benchmark_csv,
        evidence,
        strategy_diagnostics,
        diagnostics_payload.as_ref(),
    )?;
    let html_path = published.html_path;
    let xlsx_path = published.xlsx_path;
    let benchmark_csv_path = published.benchmark_csv_path;
    let evidence_path = published.evidence_path;
    if as_json {
        let mut value = json!({"schema_version":1,"report_kind":if comparison {"multi_market_depth_comparison"} else {"multi_market_depth"},"report":report});
        if let Some(path) = html_path {
            value["generated_artifacts"]["html"] = json!(path);
        }
        if let Some(path) = xlsx_path {
            value["generated_artifacts"]["xlsx"] = json!(path);
        }
        if let Some(path) = benchmark_csv_path {
            value["generated_artifacts"]["benchmark_csv"] = json!(path);
        }
        if let Some(path) = evidence_path {
            value["generated_artifacts"]["evidence"] = json!(path);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?
        );
    } else {
        if let Some(path) = html_path {
            println!("[Report] html={}", path.display());
        }
        if let Some(path) = xlsx_path {
            println!("[Report] xlsx={}", path.display());
        }
        if let Some(path) = evidence_path {
            println!("[Report] evidence={}", path.display());
        }
        if let Some(path) = benchmark_csv_path {
            println!("[Report] benchmark_csv={}", path.display());
        }
        if comparison {
            println!(
                "[Report] comparison_fingerprint={} compared_runs={}",
                report["data_fingerprint"],
                report["ranking"].as_array().map_or(0, Vec::len)
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
    }
    Ok(true)
}

#[cfg(test)]
mod diagnostics_tests {
    use super::{
        bind_depth_comparison_diagnostics, bind_market_depth_diagnostics_value,
        parse_depth_comparison_diagnostics,
    };
    use serde_json::{json, Value};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn inputs() -> (Value, Value, Value) {
        let artifact = json!({
            "spec": {"markets": [{
                "instrument": "BTCUSDT.BINANCE",
                "data_tier": "l2",
                "strategy": {"strategy_id": "sma-btc"}
            }]},
            "strategy_identities": []
        });
        let report = json!({
            "markets": [{
                "instrument": "BTCUSDT.BINANCE",
                "data_fingerprint": "depth:l2:binance:abcd",
                "snapshot_count": 3,
                "source_end_ts": 300
            }]
        });
        let diagnostics = json!([{
            "schema_version": 1,
            "kind": "qianxing-strategy-bias-diagnostics",
            "strategy_id": "sma-btc",
            "instrument": "BTCUSDT.BINANCE",
            "data_fingerprint": "depth:l2:binance:abcd",
            "input_kind": "depth_midpoint_bar",
            "input_events": 3,
            "as_of": 300,
            "lookahead": {"status": "no_change_detected"},
            "startup": {"status": "stable_across_requested_windows"}
        }]);
        (artifact, report, diagnostics)
    }

    #[test]
    fn depth_diagnostic_attachment_binds_market_strategy_and_event_coverage() {
        let (artifact, report, diagnostics) = inputs();
        let bound = bind_market_depth_diagnostics_value(&diagnostics, &artifact, &report)
            .expect("matching diagnostic attachment");
        assert_eq!(bound[0]["identity_bound"], true);
        assert_eq!(bound[0]["report"]["input_events"], 3);

        let mut rendered_report = report;
        rendered_report["run_id"] = json!("run-1");
        rendered_report["result_hash"] = json!("result-1");
        rendered_report["markets_total"] = json!(1);
        rendered_report["successful_fills"] = json!(0);
        rendered_report["pending_orders"] = json!(0);
        rendered_report["equity_points"] = json!(1);
        rendered_report["final_equity_raw"] = json!(100);
        rendered_report["return_bps"] = json!(0);
        rendered_report["max_drawdown_bps"] = json!(0);
        rendered_report["market_strategy_diagnostics"] = bound;
        let html = crate::report_multi_market_depth_render::render_html(&rendered_report);
        assert!(html.contains("diagnostic-filter"));
        assert!(html.contains("BTCUSDT.BINANCE"));
        assert!(html.contains("stable_across_requested_windows"));
    }

    #[test]
    fn depth_diagnostic_attachment_rejects_wrong_coverage_or_strategy_slot() {
        let (artifact, report, mut diagnostics) = inputs();
        diagnostics[0]["input_events"] = json!(2);
        assert!(
            bind_market_depth_diagnostics_value(&diagnostics, &artifact, &report)
                .unwrap_err()
                .contains("事件数")
        );

        let (_, report, mut diagnostics) = inputs();
        diagnostics[0]["strategy_id"] = json!("other-strategy");
        assert!(
            bind_market_depth_diagnostics_value(&diagnostics, &artifact, &report)
                .unwrap_err()
                .contains("策略槽")
        );

        let (_, report, mut diagnostics) = inputs();
        diagnostics[0]["input_kind"] = json!("order_book");
        assert!(
            bind_market_depth_diagnostics_value(&diagnostics, &artifact, &report)
                .unwrap_err()
                .contains("输入形态")
        );
    }

    #[test]
    fn depth_comparison_diagnostics_are_bound_by_exact_run_identity() {
        let (mut artifact, mut report, diagnostics) = inputs();
        artifact["spec"]["run_id"] = json!("run-alpha");
        artifact["outcome"] = json!({"shared_result_hash":"hash-alpha","pending_orders":[]});
        artifact["market_data"] = json!([]);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after unix epoch")
            .as_nanos();
        let summary_path = std::env::temp_dir().join(format!(
            "qx-depth-comparison-{nonce}.multi-market-depth.json"
        ));
        std::fs::write(
            &summary_path,
            serde_json::to_vec(&artifact).expect("serialize summary fixture"),
        )
        .expect("write summary fixture");
        report["run_id"] = json!("run-alpha");
        report["summary_path"] = json!(summary_path.to_string_lossy());
        let mut reports = vec![report];
        let payload = json!({
            "schema_version": 1,
            "kind": "qianxing-multi-market-depth-diagnostic-comparison",
            "runs": [{"run_id":"run-alpha","diagnostics":diagnostics}]
        });
        let parsed = parse_depth_comparison_diagnostics(&payload).expect("parse comparison input");
        assert_eq!(parsed.len(), 1);
        let diagnostics_path = summary_path.with_extension("diagnostics.json");
        std::fs::write(
            &diagnostics_path,
            serde_json::to_vec(&payload).expect("serialize comparison diagnostics"),
        )
        .expect("write comparison diagnostics");
        bind_depth_comparison_diagnostics(&diagnostics_path, &mut reports)
            .expect("bind per-run diagnostic to reopened summary");
        assert_eq!(
            reports[0]["market_strategy_diagnostics"][0]["identity_bound"],
            true
        );
        assert_eq!(
            reports[0]["market_strategy_diagnostics"][0]["report"]["input_events"],
            3
        );

        let html = crate::report_multi_market_depth_render::render_comparison_html(&json!({
            "data_fingerprint":"same-input",
            "ranking":[{"rank":1,"run_id":"run-alpha","return_bps":25,"max_drawdown_bps":4,"successful_fills":2,"result_hash":"hash-alpha"}],
            "runs":[{"run_id":"run-alpha","market_strategy_diagnostics":reports[0]["market_strategy_diagnostics"]}]
        }));
        assert!(html.contains("comparison-diagnostic-filter"));
        assert!(html.contains("run-alpha"));
        assert!(html.contains("stable_across_requested_windows"));
        let _ = std::fs::remove_file(summary_path);
        let _ = std::fs::remove_file(diagnostics_path);
    }

    #[test]
    fn depth_comparison_diagnostics_reject_duplicate_or_missing_run_ids() {
        let mut duplicate = json!({
            "schema_version":1,
            "kind":"qianxing-multi-market-depth-diagnostic-comparison",
            "runs":[
                {"run_id":"run-alpha","diagnostics":[]},
                {"run_id":"run-alpha","diagnostics":[]}
            ]
        });
        assert!(parse_depth_comparison_diagnostics(&duplicate)
            .unwrap_err()
            .contains("重复 run_id"));
        duplicate["runs"] = json!([{"run_id":"run-alpha","diagnostics":[]}]);
        assert_eq!(
            parse_depth_comparison_diagnostics(&duplicate)
                .unwrap()
                .len(),
            1
        );
        duplicate["kind"] = json!("unknown");
        assert!(parse_depth_comparison_diagnostics(&duplicate).is_err());
    }
}
