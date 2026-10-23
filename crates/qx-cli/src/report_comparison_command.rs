use crate::report_output_paths::{report_artifact_paths, same_output_target};
use crate::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub(crate) fn run_comparison_report(
    base_path: &Path,
    against: &[PathBuf],
    options: crate::report_command::ReportOptions<'_>,
) -> Result<(), String> {
    let crate::report_command::ReportOptions {
        as_json,
        html,
        output,
        xlsx,
        benchmark_bars,
        benchmark_calendar,
        benchmark_csv,
        ..
    } = options;
    let base_summary_path = resolve_backtest_summary_path(base_path)?;
    let mut paths = vec![base_summary_path.clone()];
    for path in against {
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".summary.json"))
        {
            return Err(format!(
                "--against 只接受显式 *.summary.json 文件: {}",
                path.display()
            ));
        }
        paths.push(resolve_backtest_summary_path(path)?);
    }
    if paths.len() < 2 {
        return Err("横向对照至少需要两份回测摘要".into());
    }
    let mut entries = Vec::with_capacity(paths.len());
    let mut equity_series = Vec::with_capacity(paths.len());
    let mut seen_paths = BTreeSet::new();
    let mut seen_run_ids = BTreeSet::new();
    let mut identity: Option<qx_app::report_analysis::ComparisonInputIdentity> = None;
    for path in &paths {
        let canonical_path = path
            .canonicalize()
            .map_err(|error| format!("规范化对照摘要路径失败 {}: {error}", path.display()))?;
        if !seen_paths.insert(canonical_path) {
            return Err(format!("横向对照不能重复传入同一摘要: {}", path.display()));
        }
        let payload = std::fs::read_to_string(path)
            .map_err(|error| format!("读取对照摘要失败 {}: {error}", path.display()))?;
        let current_summary: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|error| format!("对照摘要 JSON 无效 {}: {error}", path.display()))?;
        require_current_summary_schema(&current_summary)?;
        let input = qx_app::artifact_store::recompute_declared_backtest_input(&current_summary)?
            .ok_or_else(|| format!("对照摘要缺少可复核的输入身份: {}", path.display()))?;
        let instrument = summary_text(&current_summary, "/instrument")
            .ok_or_else(|| format!("对照摘要缺少 instrument: {}", path.display()))?;
        let current = qx_app::report_analysis::validate_comparison_input_identity(
            identity.as_ref(),
            &instrument,
            &input,
        )?;
        identity.get_or_insert(current);
        let entry = comparison_entry(path, &current_summary)?;
        let artifacts = report_artifact_paths(path)?;
        let points = if artifacts.equity_csv.is_file() {
            let series = crate::report_html::read_equity_csv(&artifacts.equity_csv)?;
            downsample_equity(&series, 2_000)?
        } else {
            Vec::new()
        };
        equity_series.push(serde_json::json!({
            "run_id": &entry.run_id,
            "points": points.iter().map(|(ts, value)| serde_json::json!({
                "ts": ts,
                "equity_raw": value.to_string(),
            })).collect::<Vec<_>>(),
            "available": points.first().is_some_and(|(_, value)| *value > 0),
        }));
        if !seen_run_ids.insert(entry.run_id.clone()) {
            return Err(format!(
                "横向对照 RunManifest.run_id 重复: {}",
                entry.run_id
            ));
        }
        entries.push(entry);
    }
    let identity = identity.ok_or_else(|| "横向对照输入身份未建立".to_string())?;
    let instrument = identity.instrument;
    let fingerprint = identity.fingerprint;
    rank_comparison(&mut entries);
    let analyses = benchmark_bars
        .map(|benchmark| {
            crate::report_comparison_benchmark::bind(&mut entries, benchmark, benchmark_calendar)
        })
        .transpose()?;
    if analyses.is_some() {
        crate::report_comparison_benchmark::rank_by_active_return(&mut entries);
    }
    let html_target = if html {
        Some(output.map_or_else(
            || {
                let stem = base_summary_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(".summary.json"))
                    .unwrap_or("comparison");
                base_summary_path.with_file_name(format!("{stem}.comparison.html"))
            },
            Path::to_path_buf,
        ))
    } else {
        None
    };
    let mut destinations: Vec<(&str, &Path)> = Vec::new();
    if let Some(path) = html_target.as_deref() {
        destinations.push(("HTML", path));
    }
    if let Some(path) = xlsx {
        destinations.push(("XLSX", path));
    }
    if let Some(path) = benchmark_csv {
        destinations.push(("基准 CSV", path));
    }
    for (index, (name, path)) in destinations.iter().enumerate() {
        for (other_name, other) in destinations.iter().skip(index + 1) {
            if same_output_target(path, other) {
                return Err(format!(
                    "横向对照 {name} 与 {other_name} 必须使用不同输出路径"
                ));
            }
        }
        for summary_path in &paths {
            if same_output_target(path, summary_path) {
                return Err(format!(
                    "报告产物 {name} 不能覆盖对照摘要: {}",
                    summary_path.display()
                ));
            }
            let artifact = report_artifact_paths(summary_path)?;
            for source in [&artifact.equity_csv, &artifact.fills_csv] {
                if same_output_target(path, source) {
                    return Err(format!(
                        "报告产物 {name} 不能覆盖来源文件: {}",
                        source.display()
                    ));
                }
            }
        }
        for (source_name, source) in [
            ("benchmark BarFrame", benchmark_bars),
            ("benchmark calendar", benchmark_calendar),
        ] {
            if source.is_some_and(|source| same_output_target(path, source)) {
                return Err(format!("报告产物 {name} 不能覆盖来源 {source_name}"));
            }
        }
    }
    if let (Some(analyses), Some(path)) = (&analyses, benchmark_csv) {
        crate::report_comparison_benchmark::write_csv(analyses, &entries, path)?;
    }
    let html_path = if html {
        Some(write_comparison_html(
            &base_summary_path,
            &instrument,
            &fingerprint,
            &entries,
            &equity_series,
            output,
        )?)
    } else {
        None
    };
    let xlsx_path = xlsx
        .map(|path| {
            crate::report_xlsx::write_comparison_xlsx(&instrument, &fingerprint, &entries, path)
        })
        .transpose()?;
    if as_json {
        let mut report = serde_json::json!({
            "schema_version": 1,
            "comparison_kind": if benchmark_bars.is_some() { "same_input_backtest_runs_vs_benchmark" } else { "same_input_backtest_runs" },
            "instrument": instrument,
            "data_fingerprint": fingerprint,
            "ranking": "return_bps_desc,max_drawdown_bps_asc,run_id_asc",
            "runs": entries,
        });
        if benchmark_bars.is_some() {
            report["benchmark_ranking"] = serde_json::Value::String(
                "active_return_bps_desc,max_drawdown_bps_asc,run_id_asc".into(),
            );
        }
        if let Some(path) = html_path {
            report["generated_artifacts"] = serde_json::json!({ "html": path });
        }
        if let Some(path) = &xlsx_path {
            report["generated_artifacts"]["xlsx"] =
                serde_json::Value::String(path.display().to_string());
        }
        if let Some(path) = benchmark_csv {
            report["generated_artifacts"]["benchmark_csv"] =
                serde_json::Value::String(path.display().to_string());
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("编码横向对照 JSON 失败: {error}"))?
        );
    } else if let Some(path) = html_path {
        println!("[Report] comparison_html={}", path.display());
        println!("[Report] instrument={instrument} data_fingerprint={fingerprint}");
        println!("[Report] compared_runs={}", entries.len());
    } else {
        println!("[Report Comparison] instrument={instrument} data_fingerprint={fingerprint}");
        for entry in entries {
            println!(
                "[Report Comparison] rank={} run={} strategy={} return_bps={} max_drawdown_bps={} sharpe={}",
                entry.rank,
                entry.run_id,
                entry.strategy_id.as_deref().unwrap_or(READOUT_ABSENT),
                entry.return_bps,
                entry.max_drawdown_bps,
                entry
                    .sharpe
                    .map_or_else(|| READOUT_ABSENT.into(), |value| value.to_string())
            );
        }
    }
    if !as_json {
        if let Some(path) = xlsx_path {
            println!("[Report] comparison_xlsx={}", path.display());
        }
    }
    Ok(())
}

fn downsample_equity(points: &[(i64, i128)], limit: usize) -> Result<Vec<(i64, i128)>, String> {
    if points.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err("回测权益时间戳必须严格递增，无法生成可对齐对比曲线".into());
    }
    if points.len() <= limit || limit < 2 {
        return Ok(points.to_vec());
    }
    Ok((0..limit)
        .map(|sample| {
            let index = sample * (points.len() - 1) / (limit - 1);
            points[index]
        })
        .collect())
}
