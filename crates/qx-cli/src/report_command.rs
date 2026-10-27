//! 回测报告命令：摘要复核、人读/机读投影、可选 HTML 导出与可选运行证据包。

use crate::report_output_paths::{report_artifact_paths, same_output_target};
use crate::*;
use std::path::{Path, PathBuf};

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

/// The standalone report accepts the v5-v7 summary family and complete qx-app RunSummary v1.
/// Every v1 summary is immediately sent through the shared qx-app verifier by the caller.
pub(crate) fn require_current_summary_schema(summary: &serde_json::Value) -> Result<(), String> {
    let migration = "迁移方式：使用原 runtime 与 BarFrame 重新运行 `qx-cli backtest <runtime.json> [<bar-frame.json>]`";
    let schema = summary_schema_version(summary)
        .ok_or_else(|| format!("回测摘要缺少有效 schema_version；{migration}"))?;
    let complete_app_run_summary = schema == 1
        && report_readout::summary_text(summary, "/run_id").is_some()
        && report_readout::summary_text(summary, "/instrument").is_some()
        && report_readout::summary_text(summary, "/result_hash").is_some()
        && report_readout::summary_text(summary, "/data_fingerprint").is_some()
        && report_readout::summary_text(summary, "/run_manifest").is_some()
        && report_readout::summary_text(summary, "/run_record").is_some()
        && report_readout::summary_number(summary, "/fills").is_some()
        && report_readout::summary_number(summary, "/equity_points").is_some()
        && report_readout::summary_number(summary, "/return_bps").is_some()
        && report_readout::summary_number(summary, "/max_drawdown_bps").is_some()
        && report_readout::summary_block(summary, "input").is_some()
        && report_readout::summary_block(summary, "replay").is_some()
        && report_readout::summary_block(summary, "artifact_locations").is_some();
    if !(5..=7).contains(&schema) && !complete_app_run_summary {
        return Err(format!(
            "回测摘要 schema_version={schema} 已拒绝：仅接受 v5/v6/v7，或字段完整且可由 qx-app 共享核验的 RunSummary v1；旧摘要不具备可验证的输入身份与重放证据。{migration}，再对新生成的 v7 summary 执行 report。"
        ));
    }
    Ok(())
}

const MAX_STRATEGY_DIAGNOSTICS_BYTES: u64 = 20 * 1024 * 1024;

/// The sole outlet for rendering a research HTML report, shared by both native and app backtests.
fn write_research_report_html(
    summary_path: &Path,
    summary: &serde_json::Value,
    output: Option<&Path>,
) -> Result<crate::report_html::ReportArtifacts, String> {
    crate::report_html::write_report_html(summary_path, summary, output)
}

/// Attach bounded app-backtest strategy diagnostics to the existing interactive research report.
pub(crate) fn write_app_backtest_diagnostic_html(
    summary_path: &Path,
    summary: &serde_json::Value,
    spec: &qx_app::BacktestSpec,
    diagnostics: &serde_json::Value,
) -> Result<(), String> {
    let mut display_summary = summary.clone();
    display_summary["metrics"] = serde_json::json!({
        "return_bps": summary["return_bps"],
        "max_drawdown_bps": summary["max_drawdown_bps"],
        "final_equity_raw": summary["final_equity_raw"],
        "fees_raw": summary["fees_raw"],
        "turnover_raw": summary["turnover_raw"],
    });
    display_summary["account"] = serde_json::json!({
        "initial_cash_raw": spec.initial_cash_raw,
        "currency": spec.settlement_currency,
    });
    display_summary["rejected_orders"] =
        serde_json::Value::from(report_readout::summary_rejected_orders(summary)?);
    display_summary["matching_kernel"] =
        serde_json::Value::String("qx-xingban::BacktestEngine(bar)".into());
    display_summary["risk_rules"] = serde_json::json!({
        "rule_set_version": summary["risk_rule_set_version"],
        "source": "verified RunManifest",
    });
    display_summary["execution_costs"] = serde_json::json!({ "source": summary["fee_model"] });
    display_summary["assumptions"] = serde_json::json!([
        "本地确定性 Bar 回测",
        "策略诊断使用固定初始资金和空持仓上下文",
        "未执行供应商 PIT 可见性验证",
    ]);
    display_summary["strategy_bias_diagnostics"] = diagnostics.clone();
    write_research_report_html(summary_path, &display_summary, None)?;
    Ok(())
}

fn read_bound_strategy_diagnostics(
    path: &Path,
    summary: &serde_json::Value,
    verified_data_fingerprint: Option<&str>,
) -> Result<serde_json::Value, String> {
    let size = std::fs::metadata(path)
        .map_err(|error| format!("读取策略诊断文件属性失败 {}: {error}", path.display()))?
        .len();
    if size > MAX_STRATEGY_DIAGNOSTICS_BYTES {
        return Err(format!(
            "策略诊断文件超过 {} MiB 上限: {}",
            MAX_STRATEGY_DIAGNOSTICS_BYTES / (1024 * 1024),
            path.display()
        ));
    }
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取策略诊断文件失败 {}: {error}", path.display()))?;
    let report: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| format!("策略诊断 JSON 无效 {}: {error}", path.display()))?;
    let summary_strategy = report_readout::summary_text(summary, "/strategy_id")
        .ok_or("回测摘要缺少可绑定的 strategy_id")?;
    let summary_instrument = report_readout::summary_text(summary, "/instrument")
        .ok_or("回测摘要缺少可绑定的 instrument")?;
    let fingerprint =
        verified_data_fingerprint.ok_or("回测摘要缺少可复核的数据身份，不能绑定策略诊断")?;
    validate_strategy_diagnostics_identity(
        &report,
        &summary_strategy,
        &summary_instrument,
        fingerprint,
    )?;
    Ok(report)
}

pub(crate) fn validate_strategy_diagnostics_identity(
    report: &serde_json::Value,
    strategy_id: &str,
    instrument: &str,
    fingerprint: &str,
) -> Result<(), String> {
    if report
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
        || report.get("kind").and_then(serde_json::Value::as_str)
            != Some("qianxing-strategy-bias-diagnostics")
    {
        return Err("策略诊断报告须为 qianxing-strategy-bias-diagnostics v1".into());
    }
    if report
        .get("strategy_id")
        .and_then(serde_json::Value::as_str)
        != Some(strategy_id)
    {
        return Err("策略诊断 strategy_id 与回测策略槽不一致".into());
    }
    if report.get("instrument").and_then(serde_json::Value::as_str) != Some(instrument) {
        return Err("策略诊断 instrument 与回测标的不一致".into());
    }
    if report
        .get("data_fingerprint")
        .and_then(serde_json::Value::as_str)
        != Some(fingerprint)
    {
        return Err("策略诊断 data_fingerprint 与复核后的回测输入不一致".into());
    }
    if report
        .get("as_of")
        .and_then(serde_json::Value::as_u64)
        .is_none()
    {
        return Err("策略诊断缺少有效 as_of".into());
    }
    for (pointer, allowed) in [
        (
            "/lookahead/status",
            &[
                "potential_lookahead_detected",
                "no_change_detected",
                "insufficient_history",
                "insufficient_work_budget",
                "causal_event_contract",
            ][..],
        ),
        (
            "/startup/status",
            &[
                "startup_sensitive",
                "stable_across_requested_windows",
                "insufficient_reference_history",
                "insufficient_history",
            ][..],
        ),
    ] {
        let status = report.pointer(pointer).and_then(serde_json::Value::as_str);
        if !status.is_some_and(|value| allowed.contains(&value)) {
            return Err(format!("策略诊断报告缺少有效 {pointer}"));
        }
    }
    if report.get("data_visibility").is_some() {
        let status = report
            .pointer("/data_visibility/status")
            .and_then(serde_json::Value::as_str);
        if !status.is_some_and(|value| {
            [
                "potential_pit_bias_detected",
                "visibility_unknown",
                "no_pit_bias_detected",
            ]
            .contains(&value)
        }) {
            return Err("策略诊断报告缺少有效 /data_visibility/status".into());
        }
    }
    Ok(())
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
                report_readout::summary_number(&summary, "/metrics/return_bps").ok_or_else(
                    || "回测摘要缺少有效 metrics.return_bps，不能计算超额收益".to_string(),
                )?,
            )
            .map_err(|_| "回测摘要 metrics.return_bps 超出 i64 范围".to_string())?;
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
    if let (Some(analysis), Some(csv_path)) = (&benchmark, benchmark_csv) {
        crate::report_benchmark::write_benchmark_csv(analysis, csv_path)?;
    }
    if let Some(xlsx_path) = xlsx {
        crate::report_xlsx::write_report_xlsx(
            &summary_path,
            &summary,
            benchmark.as_ref(),
            strategy_bias.as_ref(),
            xlsx_path,
        )?;
    }
    let artifacts = if html {
        Some(write_research_report_html(
            &summary_path,
            &display_summary,
            output,
        )?)
    } else {
        None
    };
    // 运行证据包排在复核链**之后**：上面那一步已经把每份产物的 SHA-256 重算过一遍，所以证据包里
    // `artifact_digests_verified=true` 是核对的结果，而不是一句声明（T1-1 / 退出门 G1 第一条）。
    let evidence_path = if evidence {
        Some(run_evidence::write_run_evidence(
            &summary_path,
            &summary,
            benchmark.as_ref(),
            strategy_diagnostics.zip(strategy_bias.as_ref()),
        )?)
    } else {
        None
    };
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

fn read_verified_depth_summary(
    summary_path: &Path,
    summary: &serde_json::Value,
) -> Result<qx_app::ResearchRunReportV1, String> {
    let locations = report_readout::summary_block(summary, "artifact_locations")
        .and_then(serde_json::Value::as_object)
        .ok_or("深度摘要缺少 artifact_locations，不能构造共享报告身份")?;
    let declared_summary = locations
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .ok_or("深度摘要缺少 summary 工件定位")?;
    let reader = qx_app::artifact_store::FileArtifactReader;
    if !qx_app::artifact_store::ArtifactReader::equivalent_references(
        &reader,
        &summary_path.to_string_lossy(),
        declared_summary,
    )
    .map_err(|error| error.to_string())?
    {
        return Err("所选深度摘要路径与 artifact_locations 不一致".into());
    }
    let required_path = |name: &str| -> Result<String, String> {
        locations
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("深度摘要缺少 {name} 工件定位"))
    };
    let run_manifest = required_path("run_manifest")?;
    let event_trace = report_readout::summary_text(summary, "/event_trace").or_else(|| {
        let path = Path::new(&run_manifest);
        let name = path.file_name()?.to_str()?.strip_suffix(".run.json")?;
        Some(
            path.with_file_name(format!("{name}.events.jsonl"))
                .to_string_lossy()
                .into_owned(),
        )
    });
    let outcome = qx_app::DepthBacktestOutcome {
        run_id: required_summary_text(summary, "run_id")?,
        manifest_run_id: report_readout::summary_text(summary, "/manifest_run_id")
            .filter(|value| !value.trim().is_empty()),
        instrument: required_summary_text(summary, "instrument")?,
        tier: required_summary_text(summary, "tier")?,
        result_hash: required_summary_text(summary, "result_hash")?,
        data_fingerprint: required_summary_text(summary, "data_fingerprint")?,
        fills: required_summary_u64(summary, "fills")?,
        equity_points: required_summary_u64(summary, "equity_points")?,
        snapshots: required_summary_u64(summary, "bars")?,
        return_bps: i32::try_from(
            report_readout::summary_number(summary, "/return_bps")
                .ok_or("深度摘要缺少有效 return_bps")?,
        )
        .map_err(|_| "深度摘要 return_bps 超出 i32 范围")?,
        max_drawdown_bps: u32::try_from(required_summary_u64(summary, "max_drawdown_bps")?)
            .map_err(|_| "深度摘要 max_drawdown_bps 超出 u32 范围")?,
        artifacts: qx_app::BacktestArtifacts {
            run_manifest,
            summary: summary_path.to_string_lossy().into_owned(),
            equity: required_path("equity")?,
            fills: required_path("fills")?,
            event_trace,
        },
        manifest_digest: String::new(),
        account_id: report_readout::summary_text(summary, "/account_id"),
        display_summary: summary.clone(),
    };
    let context =
        qx_app::RunContext::new(qx_app::CallerCapability::Research, outcome.run_id.clone());
    let report = qx_app::read_depth_run_report_v1_with_reader(&outcome, &context, &reader)
        .map_err(|error| error.to_string())?;
    if !report.verification.verified {
        return Err(format!(
            "深度研究报告复核失败: {}",
            report.verification.mismatches.join("；")
        ));
    }
    if report.summary != *summary {
        return Err("深度摘要在共享报告复核期间发生变化".into());
    }
    Ok(report)
}

fn read_verified_bar_summary(
    summary_path: &Path,
    summary: &serde_json::Value,
) -> Result<qx_app::ResearchRunReportV1, String> {
    let locations = report_readout::summary_block(summary, "artifact_locations")
        .and_then(serde_json::Value::as_object)
        .ok_or("Bar 摘要缺少 artifact_locations，不能构造共享报告身份")?;
    let declared_summary = locations
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .ok_or("Bar 摘要缺少 summary 工件定位")?;
    let reader = qx_app::artifact_store::FileArtifactReader;
    if !qx_app::artifact_store::ArtifactReader::equivalent_references(
        &reader,
        &summary_path.to_string_lossy(),
        declared_summary,
    )
    .map_err(|error| error.to_string())?
    {
        return Err("所选 Bar 摘要路径与 artifact_locations 不一致".into());
    }
    let required_path = |name: &str| -> Result<String, String> {
        locations
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("Bar 摘要缺少 {name} 工件定位"))
    };
    let outcome = qx_app::BacktestOutcome {
        run_id: required_summary_text(summary, "run_id")?,
        instrument: required_summary_text(summary, "instrument")?,
        result_hash: required_summary_text(summary, "result_hash")?,
        data_fingerprint: required_summary_text(summary, "data_fingerprint")?,
        fills: required_summary_u64(summary, "fills")?,
        equity_points: required_summary_u64(summary, "equity_points")?,
        return_bps: i32::try_from(
            report_readout::summary_number(summary, "/return_bps")
                .ok_or("Bar 摘要缺少有效 return_bps")?,
        )
        .map_err(|_| "Bar 摘要 return_bps 超出 i32 范围")?,
        max_drawdown_bps: u32::try_from(required_summary_u64(summary, "max_drawdown_bps")?)
            .map_err(|_| "Bar 摘要 max_drawdown_bps 超出 u32 范围")?,
        artifacts: qx_app::BacktestArtifacts {
            run_manifest: required_path("run_manifest")?,
            summary: summary_path.to_string_lossy().into_owned(),
            equity: required_path("equity")?,
            fills: required_path("fills")?,
            event_trace: report_readout::summary_text(summary, "/event_trace"),
        },
        account_id: report_readout::summary_text(summary, "/account_id"),
    };
    let context =
        qx_app::RunContext::new(qx_app::CallerCapability::Research, outcome.run_id.clone());
    let report = qx_app::read_run_report_v1_with_reader(&outcome, &context, &reader)
        .map_err(|error| error.to_string())?;
    if !report.verification.verified {
        return Err(format!(
            "Bar 研究报告复核失败: {}",
            report.verification.mismatches.join("；")
        ));
    }
    if report.summary != *summary {
        return Err("Bar 摘要在共享报告复核期间发生变化".into());
    }
    Ok(report)
}

fn required_summary_text(summary: &serde_json::Value, key: &str) -> Result<String, String> {
    report_readout::summary_text(summary, &format!("/{key}"))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("深度摘要缺少有效 {key}"))
}

fn required_summary_u64(summary: &serde_json::Value, key: &str) -> Result<u64, String> {
    let value = report_readout::summary_number(summary, &format!("/{key}"))
        .ok_or_else(|| format!("深度摘要缺少有效 {key}"))?;
    u64::try_from(value).map_err(|_| format!("深度摘要 {key} 超出 u64 范围"))
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
