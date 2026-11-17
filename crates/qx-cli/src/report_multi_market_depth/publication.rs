//! Staged output generation and publication for multi-market depth reports.

use crate::report_multi_market_depth_render::{
    render_comparison_html, render_html, strval, workbook_rows,
};
use crate::report_output_paths::same_output_target;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn benchmark_runs_from_report(report: &Value) -> Vec<(String, Value)> {
    if let Some(runs) = report.get("ranking").and_then(Value::as_array) {
        runs.iter()
            .filter_map(|run| {
                run.get("benchmark_analysis")
                    .filter(|analysis| !analysis.is_null())
                    .cloned()
                    .map(|analysis| {
                        (
                            run.get("run_id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            analysis,
                        )
                    })
            })
            .collect()
    } else {
        report
            .get("benchmark_analysis")
            .filter(|analysis| !analysis.is_null())
            .cloned()
            .map(|analysis| {
                vec![(
                    report
                        .get("run_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    analysis,
                )]
            })
            .unwrap_or_default()
    }
}

pub(super) struct PublishedArtifacts {
    pub(super) html_path: Option<PathBuf>,
    pub(super) xlsx_path: Option<PathBuf>,
    pub(super) benchmark_csv_path: Option<PathBuf>,
    pub(super) evidence_path: Option<PathBuf>,
}

pub(super) fn publish_report(
    report: &Value,
    comparison: bool,
    path: &Path,
    source_paths: &[PathBuf],
    html: bool,
    output: Option<&Path>,
    xlsx: Option<&Path>,
    benchmark_csv: Option<&Path>,
    evidence: bool,
    strategy_diagnostics: Option<&Path>,
    diagnostics_payload: Option<&Value>,
) -> Result<PublishedArtifacts, String> {
    let mut output_bundle = crate::report_publish::ReportOutputBundle::default();
    let html_path = if html {
        let target = output.map(Path::to_path_buf).unwrap_or_else(|| {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("multi-market-depth.json");
            path.with_file_name(name.replace(
                ".multi-market-depth.json",
                if comparison {
                    ".multi-market-depth.comparison.html"
                } else {
                    ".multi-market-depth.report.html"
                },
            ))
        });
        if !target
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                value.eq_ignore_ascii_case("html") || value.eq_ignore_ascii_case("htm")
            })
        {
            return Err(format!(
                "HTML 报告输出路径必须以 .html 或 .htm 结尾: {}",
                target.display()
            ));
        }
        if source_paths
            .iter()
            .any(|source| same_output_target(&target, source))
        {
            return Err("HTML 输出不能覆盖多市场深度输入产物".into());
        }
        let rendered = if comparison {
            render_comparison_html(report)
        } else {
            render_html(report)
        };
        let rendered = if comparison {
            let mut sections = String::new();
            for run in report["ranking"].as_array().into_iter().flatten() {
                if let Some(benchmark) = crate::report_benchmark::benchmark_html_section(
                    &json!({"benchmark_analysis": run["benchmark_analysis"]}),
                ) {
                    sections.push_str(&format!(
                        "<section><h2>运行 {}</h2>{benchmark}</section>",
                        crate::report_html::escape_html(run["run_id"].as_str().unwrap_or(""))
                    ));
                }
            }
            rendered.replace("</body>", &format!("{sections}</body>"))
        } else if let Some(benchmark) = crate::report_benchmark::benchmark_html_section(report) {
            rendered.replace("</body>", &format!("{benchmark}</body>"))
        } else {
            rendered
        };
        output_bundle.add_bytes(&target, rendered.into_bytes());
        Some(target)
    } else {
        None
    };
    let xlsx_path = if let Some(target) = xlsx {
        if source_paths
            .iter()
            .any(|source| same_output_target(target, source))
        {
            return Err("XLSX 输出不能覆盖多市场深度输入产物".into());
        }
        let staged = output_bundle.stage_path(target)?;
        if comparison {
            let overview = vec![
                vec!["Field".into(), "Value".into()],
                vec![
                    "Data fingerprint".into(),
                    strval(&report["data_fingerprint"]),
                ],
                vec![
                    "Comparison kind".into(),
                    "same_multi_market_depth_input".into(),
                ],
            ];
            let mut ranking = vec![vec![
                "rank".into(),
                "run_id".into(),
                "return_bps".into(),
                "max_drawdown_bps".into(),
                "successful_fills".into(),
                "result_hash".into(),
                "summary_path".into(),
            ]];
            for row in report["ranking"].as_array().into_iter().flatten() {
                ranking.push(
                    [
                        "rank",
                        "run_id",
                        "return_bps",
                        "max_drawdown_bps",
                        "successful_fills",
                        "result_hash",
                        "summary_path",
                    ]
                    .into_iter()
                    .map(|key| strval(&row[key]))
                    .collect(),
                );
            }
            crate::report_xlsx::write_multi_market_depth_comparison_xlsx(
                &staged,
                overview,
                ranking,
                report.get("runs"),
                Some(&benchmark_runs_from_report(report)),
            )?;
            output_bundle.add_staged(target, &staged)?;
            Some(target.to_path_buf())
        } else {
            let (overview, quality) = workbook_rows(report);
            crate::report_xlsx::write_multi_market_depth_xlsx(
                &staged,
                overview,
                quality,
                report.get("market_strategy_diagnostics"),
                Some(&benchmark_runs_from_report(report)),
            )?;
            output_bundle.add_staged(target, &staged)?;
            Some(target.to_path_buf())
        }
    } else {
        None
    };
    let benchmark_csv_path = if let Some(target) = benchmark_csv {
        if source_paths
            .iter()
            .any(|source| same_output_target(target, source))
        {
            return Err("基准 CSV 输出不能覆盖多市场深度输入或基准产物".into());
        }
        let staged = output_bundle.stage_path(target)?;
        if comparison {
            let runs = benchmark_runs_from_report(report);
            crate::report_benchmark::write_benchmark_runs_csv(&runs, &staged)?;
        } else {
            let analysis = report
                .get("benchmark_analysis")
                .ok_or("多市场深度报告缺少 benchmark_analysis")?;
            crate::report_benchmark::write_benchmark_csv(analysis, &staged)?;
        }
        output_bundle.add_staged(target, &staged)?;
        Some(target.to_path_buf())
    } else {
        None
    };
    let evidence_path = if evidence {
        let diagnostics = strategy_diagnostics.zip(diagnostics_payload);
        Some(
            crate::run_evidence_multi_market::write_multi_market_depth_run_evidence(
                path,
                diagnostics,
            )?,
        )
    } else {
        None
    };
    output_bundle.commit()?;
    Ok(PublishedArtifacts {
        html_path,
        xlsx_path,
        benchmark_csv_path,
        evidence_path,
    })
}
