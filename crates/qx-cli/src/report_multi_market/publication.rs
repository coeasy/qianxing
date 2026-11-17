//! Staged HTML, XLSX, CSV, and Evidence output for multi-market Bar reports.

use super::diagnostics::{comparison_diagnostics_for_xlsx, render_comparison_diagnostics};
use super::render::render_html;
use crate::report_output_paths::same_output_target;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub(super) fn publish_report(
    path: &Path,
    report: &Value,
    comparison: bool,
    evidence: bool,
    html: bool,
    output: Option<&Path>,
    benchmark_csv: Option<&Path>,
    xlsx: Option<&Path>,
    strategy_diagnostics: Option<&Path>,
    diagnostics_payload: Option<&Value>,
    protected_sources: &[PathBuf],
) -> Result<serde_json::Map<String, Value>, String> {
    let mut generated = serde_json::Map::new();
    let mut output_bundle = crate::report_publish::ReportOutputBundle::default();
    if evidence {
        let diagnostics = strategy_diagnostics.zip(diagnostics_payload);
        let bundle_path =
            crate::run_evidence_multi_market::write_multi_market_run_evidence(path, diagnostics)?;
        generated.insert("evidence".into(), json!(bundle_path));
    }
    if html {
        let target = output.map(Path::to_path_buf).unwrap_or_else(|| {
            let name = path
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("multi-market.json");
            path.with_file_name(name.replace(
                ".multi-market.json",
                if comparison {
                    ".multi-market.comparison.html"
                } else {
                    ".multi-market.report.html"
                },
            ))
        });
        if !target
            .extension()
            .and_then(|v| v.to_str())
            .is_some_and(|v| v.eq_ignore_ascii_case("html") || v.eq_ignore_ascii_case("htm"))
        {
            return Err(format!(
                "HTML 输出路径必须以 .html 或 .htm 结尾: {}",
                target.display()
            ));
        }
        if protected_sources
            .iter()
            .any(|source| same_output_target(&target, source))
        {
            return Err("HTML 输出不能覆盖任何输入运行产物".into());
        }
        if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建报告目录失败: {e}"))?;
        }
        let rendered = render_html(report, comparison)?;
        let rendered = if comparison {
            let mut sections = String::new();
            for run in report["runs"].as_array().into_iter().flatten() {
                if let Some(benchmark) = crate::report_benchmark::benchmark_html_section(
                    &json!({"benchmark_analysis": run["benchmark_analysis"]}),
                ) {
                    sections.push_str(&format!(
                        "<section><h2>运行 {}</h2>{benchmark}</section>",
                        crate::report_html::escape_html(run["run_id"].as_str().unwrap_or(""))
                    ));
                }
            }
            rendered.replace("</main>", &format!("{sections}</main>"))
        } else if let Some(benchmark) = crate::report_benchmark::benchmark_html_section(report) {
            rendered.replace("</main>", &format!("{benchmark}</main>"))
        } else {
            rendered
        };
        let rendered = if comparison {
            rendered.replace(
                "</main>",
                &format!("{}</main>", render_comparison_diagnostics(report)),
            )
        } else {
            rendered
        };
        output_bundle.add_bytes(&target, rendered.into_bytes());
        generated.insert("html".into(), json!(target));
    }
    if let Some(target) = benchmark_csv {
        if protected_sources
            .iter()
            .any(|source| same_output_target(target, source))
        {
            return Err("基准 CSV 输出不能覆盖任何输入运行或基准产物".into());
        }
        let staged = output_bundle.stage_path(target)?;
        if comparison {
            let runs = report["runs"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|run| {
                    Ok((
                        run["run_id"]
                            .as_str()
                            .ok_or("多市场比较运行缺少 run_id")?
                            .to_owned(),
                        run.get("benchmark_analysis")
                            .cloned()
                            .ok_or("多市场比较运行缺少 benchmark_analysis")?,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            crate::report_benchmark::write_benchmark_runs_csv(&runs, &staged)?;
        } else {
            let analysis = report
                .get("benchmark_analysis")
                .ok_or("多市场报告缺少 benchmark_analysis")?;
            crate::report_benchmark::write_benchmark_csv(analysis, &staged)?;
        }
        output_bundle.add_staged(target, &staged)?;
        generated.insert("benchmark_csv".into(), json!(target));
    }
    if let Some(target) = xlsx {
        if protected_sources
            .iter()
            .any(|source| same_output_target(target, source))
        {
            return Err("XLSX 输出不能覆盖任何输入运行产物".into());
        }
        let (mut overview, markets) = if comparison {
            (
                vec![
                    vec!["Comparison".into(), "same_multi_market_bar_input".into()],
                    vec![
                        "Data fingerprint".into(),
                        report["data_fingerprint"].as_str().unwrap_or("").into(),
                    ],
                ],
                report["runs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|r| {
                        vec![
                            r["run_id"].to_string(),
                            r["return_bps"].to_string(),
                            r["max_drawdown_bps"].to_string(),
                            r["successful_fills"].to_string(),
                            r["result_hash"].to_string(),
                        ]
                    })
                    .collect(),
            )
        } else {
            (
                vec![
                    vec!["Field".into(), "Value".into()],
                    vec!["run_id".into(), report["run_id"].to_string()],
                    vec!["return_bps".into(), report["return_bps"].to_string()],
                    vec![
                        "max_drawdown_bps".into(),
                        report["max_drawdown_bps"].to_string(),
                    ],
                ],
                report["markets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|m| {
                        vec![
                            m["instrument"].to_string(),
                            m["row_count"].to_string(),
                            m["fills"].to_string(),
                            m["content_hash"].to_string(),
                            m["quality_status"].to_string(),
                        ]
                    })
                    .collect(),
            )
        };
        let metrics_definition = if comparison {
            report["runs"]
                .as_array()
                .and_then(|runs| runs.first())
                .map(|run| &run["metrics_definition"])
        } else {
            Some(&report["metrics_definition"])
        };
        if let Some(definition) = metrics_definition {
            for key in [
                "schema_version",
                "risk_ratio_period_basis",
                "benchmark_period_basis",
                "fees_exact_basis",
                "fees_raw_basis",
            ] {
                overview.push(vec![
                    format!("metrics_definition.{key}"),
                    definition[key]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| definition[key].to_string()),
                ]);
            }
        }
        let metrics_exact = if comparison {
            report["runs"]
                .as_array()
                .and_then(|runs| runs.first())
                .map(|run| &run["metrics_exact"])
        } else {
            Some(&report["metrics_exact"])
        };
        if let Some(fees) = metrics_exact.and_then(|metrics| metrics.get("fees_by_currency")) {
            overview.push(vec![
                "metrics_exact.fees_by_currency".into(),
                fees.to_string(),
            ]);
        }
        let mut m: Vec<Vec<String>> = markets;
        if comparison {
            m.insert(
                0,
                vec![
                    "run_id".into(),
                    "return_bps".into(),
                    "max_drawdown_bps".into(),
                    "fills".into(),
                    "result_hash".into(),
                ],
            );
        } else {
            m.insert(
                0,
                vec![
                    "instrument".into(),
                    "bar_rows".into(),
                    "fills".into(),
                    "content_hash".into(),
                    "quality".into(),
                ],
            );
        }
        let diagnostics_for_xlsx = if comparison {
            comparison_diagnostics_for_xlsx(report)
        } else {
            report.get("market_strategy_diagnostics").cloned()
        };
        let benchmark_runs = if comparison {
            report["runs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|run| {
                    run.get("benchmark_analysis").cloned().map(|analysis| {
                        (
                            run["run_id"].as_str().unwrap_or_default().to_owned(),
                            analysis,
                        )
                    })
                })
                .collect::<Vec<_>>()
        } else {
            report
                .get("benchmark_analysis")
                .cloned()
                .map(|analysis| {
                    vec![(
                        report["run_id"].as_str().unwrap_or_default().to_owned(),
                        analysis,
                    )]
                })
                .unwrap_or_default()
        };
        let staged = output_bundle.stage_path(target)?;
        crate::report_xlsx::write_multi_market_depth_xlsx(
            &staged,
            overview,
            m,
            diagnostics_for_xlsx.as_ref(),
            Some(&benchmark_runs),
        )?;
        output_bundle.add_staged(target, &staged)?;
        generated.insert("xlsx".into(), json!(target));
    }
    output_bundle.commit()?;
    Ok(generated)
}
