//! Report-family routing, verified single-run projection, and output commit workflow.

use super::*;
use crate::report_output_paths::{report_artifact_paths, same_output_target};

pub(crate) fn run_report_with_benchmark(
    path: &Path,
    options: ReportOptions<'_>,
) -> Result<(), String> {
    let ReportOptions {
        as_json,
        html,
        evidence,
        output,
        against,
        benchmark_bars,
        benchmark_calendar,
        benchmark_csv,
        strategy_diagnostics,
        xlsx,
    } = options;
    if output.is_some() && !html {
        return Err("--output 只能与 --html 一起使用".to_string());
    }
    if benchmark_csv.is_some() && benchmark_bars.is_none() {
        return Err("--benchmark-csv 必须与 --benchmark-bars 一起使用".into());
    }
    if benchmark_calendar.is_some() && benchmark_bars.is_none() {
        return Err("--benchmark-calendar 必须与 --benchmark-bars 一起使用".into());
    }
    if strategy_diagnostics.is_some() && !html && !as_json && xlsx.is_none() {
        return Err("--strategy-diagnostics 必须与 --html、--json 或 --xlsx 一起使用".into());
    }
    if crate::report_multi_market::maybe_run(
        path,
        as_json,
        html,
        evidence,
        output,
        against,
        benchmark_bars,
        benchmark_calendar,
        benchmark_csv,
        xlsx,
        strategy_diagnostics,
    )? {
        return Ok(());
    }
    if benchmark_bars.is_some() || benchmark_csv.is_some() || benchmark_calendar.is_some() {
        if crate::report_multi_market_depth::maybe_run(
            path,
            as_json,
            html,
            evidence,
            output,
            against,
            benchmark_bars,
            benchmark_calendar,
            benchmark_csv,
            xlsx,
            strategy_diagnostics,
        )? {
            return Ok(());
        }
    } else if crate::report_multi_market_depth::maybe_run(
        path,
        as_json,
        html,
        evidence,
        output,
        against,
        benchmark_bars,
        benchmark_calendar,
        benchmark_csv,
        xlsx,
        strategy_diagnostics,
    )? {
        return Ok(());
    }
    if crate::report_multi_account::maybe_run(
        path,
        as_json,
        html,
        evidence,
        output,
        against,
        xlsx,
        benchmark_bars.is_some() || benchmark_calendar.is_some() || benchmark_csv.is_some(),
        strategy_diagnostics,
    )? {
        return Ok(());
    }
    if !against.is_empty() {
        if evidence {
            return Err("横向对照报告不接受 --evidence；请对每轮单独生成运行证据包".into());
        }
        if strategy_diagnostics.is_some() {
            return Err("--strategy-diagnostics 只可绑定单次回测摘要，不能用于此类横向对照".into());
        }
        return crate::report_comparison_command::run_comparison_report(
            path,
            against,
            ReportOptions {
                as_json,
                html,
                evidence,
                output,
                against,
                benchmark_bars,
                benchmark_calendar,
                benchmark_csv,
                strategy_diagnostics,
                xlsx,
            },
        );
    }
    let summary_path = resolve_backtest_summary_path(path)?;
    let artifact_paths = report_artifact_paths(&summary_path)?;
    let default_html = if html && output.is_none() {
        Some(artifact_paths.html.clone())
    } else {
        None
    };
    let mut destinations = Vec::new();
    if html {
        let html_path = output
            .or(default_html.as_deref())
            .ok_or_else(|| "请求 HTML 报告但未能确定输出路径".to_string())?;
        destinations.push(("HTML", html_path));
    }
    if let Some(path) = benchmark_csv {
        destinations.push(("基准 CSV", path));
    }
    if let Some(path) = xlsx {
        destinations.push(("XLSX", path));
    }
    for (index, (left_name, left)) in destinations.iter().enumerate() {
        for (right_name, right) in destinations.iter().skip(index + 1) {
            if same_output_target(left, right) {
                return Err(format!("{left_name} 与 {right_name} 必须使用不同输出路径"));
            }
        }
        for (source_name, source) in [
            ("summary", summary_path.as_path()),
            ("equity.csv", artifact_paths.equity_csv.as_path()),
            ("fills.csv", artifact_paths.fills_csv.as_path()),
        ]
        .into_iter()
        .chain(benchmark_bars.map(|path| ("benchmark BarFrame", path)))
        .chain(benchmark_calendar.map(|path| ("benchmark calendar", path)))
        .chain(strategy_diagnostics.map(|path| ("strategy diagnostics", path)))
        {
            if same_output_target(left, source) {
                return Err(format!(
                    "报告产物 {left_name} 不能覆盖来源 {source_name}: {}",
                    source.display()
                ));
            }
        }
    }
    let payload = std::fs::read_to_string(&summary_path)
        .map_err(|error| format!("读取回测摘要失败 {}: {error}", summary_path.display()))?;
    let mut summary: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| format!("回测摘要 JSON 无效 {}: {error}", summary_path.display()))?;
    require_current_summary_schema(&summary)?;
    let report_verification = if report_readout::summary_text(&summary, "/tier").is_some() {
        let depth_report = read_verified_depth_summary(&summary_path, &summary)?;
        summary = depth_report.summary.clone();
        Some(("depth", depth_report.verification))
    } else if report_readout::summary_text(&summary, "/run_id").is_some() {
        let bar_report = read_verified_bar_summary(&summary_path, &summary)?;
        summary = bar_report.summary.clone();
        Some(("bar", bar_report.verification))
    } else {
        None
    };
    // 摘要声明的输入要重读、重算并对齐；旧 schema 未声明时必须如实显示未经核对（V11 Q66/Q1b）。
    let declared_input = qx_app::artifact_store::recompute_declared_backtest_input(&summary)?;
    let strategy_bias = strategy_diagnostics
        .map(|diagnostics_path| {
            read_bound_strategy_diagnostics(
                diagnostics_path,
                &summary,
                declared_input
                    .as_ref()
                    .map(|input| input.fingerprint.as_str()),
            )
        })
        .transpose()?;
    let benchmark = match benchmark_bars {
        Some(benchmark_path) => {
            let return_bps = i64::try_from(
                report_readout::summary_number(&summary, "/metrics/return_bps")
                    .or_else(|| report_readout::summary_number(&summary, "/return_bps"))
                    .ok_or_else(|| "回测摘要缺少有效 return_bps，不能计算超额收益".to_string())?,
            )
            .map_err(|_| "回测摘要 return_bps 超出 i64 范围".to_string())?;
            Some(crate::report_benchmark::benchmark_analysis(
                &summary_path,
                benchmark_path,
                return_bps,
                benchmark_calendar,
            )?)
        }
        None => None,
    };
    let mut display_summary = summary.clone();
    if let Some(benchmark) = &benchmark {
        display_summary["benchmark_analysis"] = benchmark.clone();
    }
    if let Some(diagnostics) = &strategy_bias {
        display_summary["strategy_bias_diagnostics"] = diagnostics.clone();
    }
    let mut output_bundle = report_publish::ReportOutputBundle::default();
    if let (Some(analysis), Some(csv_path)) = (&benchmark, benchmark_csv) {
        let staged = output_bundle.stage_path(csv_path)?;
        crate::report_benchmark::write_benchmark_csv(analysis, &staged)?;
        output_bundle.add_staged(csv_path, &staged)?;
    }
    if let Some(xlsx_path) = xlsx {
        let staged = output_bundle.stage_path(xlsx_path)?;
        crate::report_xlsx::write_report_xlsx(
            &summary_path,
            &summary,
            benchmark.as_ref(),
            strategy_bias.as_ref(),
            &staged,
        )?;
        output_bundle.add_staged(xlsx_path, &staged)?;
    }
    let artifacts = if html {
        let (artifacts, html_bundle) = crate::report_html::prepare_report_html_bundle(
            &summary_path,
            &display_summary,
            output,
        )?;
        output_bundle.extend(html_bundle);
        Some(artifacts)
    } else {
        None
    };
    // 运行证据包排在复核链**之后**：上面那一步已经把每份产物的 SHA-256 重算过一遍，所以证据包里
    // `artifact_digests_verified=true` 是核对的结果，而不是一句声明（T1-1 / 退出门 G1 第一条）。
    // 它与报告文件一起提交，避免报告已更新而 EvidenceBundle 仍缺失。
    let evidence_path = if evidence {
        let (path, payload) = run_evidence::prepare_run_evidence(
            &summary_path,
            &summary,
            benchmark.as_ref(),
            strategy_diagnostics.zip(strategy_bias.as_ref()),
        )?;
        output_bundle.add_bytes(&path, payload);
        Some(path)
    } else {
        None
    };
    output_bundle.commit()?;
    if as_json {
        let mut report = serde_json::json!({
            "schema_version": 1,
            "runtime_version": build_identity::RUNTIME_VERSION,
            "summary_path": summary_path.display().to_string(),
            "input_check": match &declared_input {
                Some(input) => serde_json::json!({
                    "verdict": "verified",
                    "declared_and_recomputed_match": true,
                    "kind": input.kind,
                    "path": input.path,
                    "dataset_id": input.dataset_id,
                    "dataset_version": input.dataset_version,
                    "fingerprint": input.fingerprint,
                }),
                None => serde_json::json!({
                    "verdict": "not_declared",
                    "declared_and_recomputed_match": false,
                }),
            },
            "fidelity": qx_app::artifact_store::run_evidence_fidelity_classification(&summary),
            "summary": summary
        });
        if let Some((run_kind, verification)) = report_verification.as_ref() {
            report["run_kind"] = serde_json::Value::String((*run_kind).into());
            report["verification"] = serde_json::to_value(verification)
                .map_err(|error| format!("编码 {run_kind} 报告复核结果失败: {error}"))?;
        }
        if let Some(benchmark) = benchmark {
            report["benchmark_analysis"] = benchmark;
        }
        if let Some(diagnostics) = strategy_bias {
            report["strategy_bias_diagnostics"] = diagnostics;
        }
        if let Some(files) = &artifacts {
            report["generated_artifacts"] = serde_json::json!({
                "html": files.html.display().to_string(),
                "equity_svg": files.equity_svg.display().to_string(),
                "fills_svg": files.fills_svg.display().to_string(),
                "monthly_svg": files.monthly_svg.display().to_string(),
            });
        }
        if let Some(path) = benchmark_csv {
            report["generated_artifacts"]["benchmark_csv"] =
                serde_json::Value::String(path.display().to_string());
        }
        if let Some(path) = xlsx {
            report["generated_artifacts"]["xlsx"] =
                serde_json::Value::String(path.display().to_string());
        }
        if let Some(path) = &evidence_path {
            report["run_evidence"] = serde_json::json!({
                "path": path.display().to_string(),
                "schema_version": qx_spec::EVIDENCE_BUNDLE_SCHEMA_VERSION,
            });
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("编码回测报告 JSON 失败: {error}"))?
        );
        return Ok(());
    }
    if let Some(files) = artifacts {
        println!("[Report] html={}", files.html.display());
        println!("[Report] equity_svg={}", files.equity_svg.display());
        println!("[Report] fills_svg={}", files.fills_svg.display());
        println!("[Report] monthly_svg={}", files.monthly_svg.display());
    }
    if let Some(path) = &evidence_path {
        println!("[Report] evidence={}", path.display());
    }
    if let Some(path) = benchmark_csv {
        println!("[Report] benchmark_csv={}", path.display());
    }
    if let Some(path) = xlsx {
        println!("[Report] xlsx={}", path.display());
    }
    println!("[Report] summary={}", summary_path.display());
    if let Some((run_kind, verification)) = report_verification.as_ref() {
        println!(
            "[Report] {run_kind}_verification={} checks={} run_id={}",
            verification.verified,
            verification.checks.len(),
            verification.run_id
        );
    }
    let input_verified = match &declared_input {
        Some(input) => format!(
            "{} input_kind={} input_id={} input_fingerprint={}",
            input.path, input.kind, input.dataset_id, input.fingerprint
        ),
        None => "not_declared（该摘要没有 input 块，输入身份未经核对）".to_string(),
    };
    for line in report_readout_lines(&summary, &input_verified) {
        println!("[Report]{line}");
    }
    if let Some(benchmark) = benchmark {
        let metrics = &benchmark["metrics"];
        let daily = benchmark["period_basis"].as_str() == Some("per_trading_day_no_annualization");
        let (label, key) = if daily {
            (
                "tracking_error_bps_per_trading_day",
                "tracking_error_bps_per_trading_day",
            )
        } else {
            ("tracking_error_bps_per_bar", "tracking_error_bps_per_bar")
        };
        println!(
            "[Report · Benchmark] instrument={} return_bps={} active_return_bps={} {}={}",
            benchmark["benchmark"]["instrument"]
                .as_str()
                .unwrap_or("unknown"),
            metrics["benchmark_return_bps"].as_i64().unwrap_or_default(),
            metrics["active_return_bps"].as_i64().unwrap_or_default(),
            label,
            metrics[key]
                .as_f64()
                .map(|value| format!("{value:.4}"))
                .unwrap_or_else(|| "null".into()),
        );
    }
    Ok(())
}
