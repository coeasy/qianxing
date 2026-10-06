//! 回测报告命令：摘要复核、人读/机读投影与可选 HTML 导出。

use crate::*;

pub(crate) fn run_report(path: &Path, as_json: bool, html: bool) -> Result<(), String> {
    run_report_with_output(path, as_json, html, None)
}

pub(crate) fn run_report_with_output(
    path: &Path,
    as_json: bool,
    html: bool,
    output: Option<&Path>,
) -> Result<(), String> {
    if output.is_some() && !html {
        return Err("--output 只能与 --html 一起使用".to_string());
    }
    let summary_path = resolve_backtest_summary_path(path)?;
    let payload = std::fs::read_to_string(&summary_path)
        .map_err(|error| format!("读取回测摘要失败 {}: {error}", summary_path.display()))?;
    let summary: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| format!("回测摘要 JSON 无效 {}: {error}", summary_path.display()))?;
    // 摘要声明的输入要重读、重算并对齐；旧 schema 未声明时必须如实显示未经核对（V11 Q66/Q1b）。
    let declared_input = recompute_declared_backtest_input(&summary)?;
    let artifacts = if html {
        Some(write_report_html(&summary_path, &summary, output)?)
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
            "summary": summary
        });
        if let Some(files) = &artifacts {
            report["generated_artifacts"] = serde_json::json!({
                "html": files.html.display().to_string(),
                "equity_svg": files.equity_svg.display().to_string(),
                "fills_svg": files.fills_svg.display().to_string(),
                "monthly_svg": files.monthly_svg.display().to_string(),
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
    println!("[Report] summary={}", summary_path.display());
    let input_verified = match &declared_input {
        Some(input) => format!(
            "{} input_kind={} input_id={} input_fingerprint={}",
            input.path, input.kind, input.dataset_id, input.fingerprint
        ),
        None => "not_declared（该摘要没有 input 块，输入身份未经核对）".to_string(),
    };
    for line in report_readout_lines(&summary, &input_verified) {
        println!("{line}");
    }
    Ok(())
}
