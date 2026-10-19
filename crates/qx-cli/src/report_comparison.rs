//! 同输入的多轮回测横向报告；只读取已复核 summary，不重算任何指标。

use crate::{escape_html, report_readout::summary_text, report_readout::READOUT_ABSENT};
pub(crate) use qx_app::report_analysis::ComparisonEntry;
use qx_spec::RunEvidenceFidelity;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub(crate) fn comparison_entry(
    summary_path: &Path,
    summary: &Value,
) -> Result<ComparisonEntry, String> {
    let manifest_path = summary_text(summary, "/run_manifest")
        .ok_or_else(|| format!("对比摘要缺少 run_manifest: {}", summary_path.display()))?;
    let manifest_payload = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("读取对比 RunManifest 失败 {manifest_path}: {error}"))?;
    let manifest = qx_core::RunManifest::from_json(&manifest_payload)?;
    qx_app::report_analysis::comparison_entry_from_summary(summary_path, summary, manifest.run_id)
}

pub(crate) fn rank_comparison(entries: &mut [ComparisonEntry]) {
    qx_app::report_analysis::rank_comparison_by_return(entries);
}

pub(crate) fn write_comparison_html(
    base_summary: &Path,
    instrument: &str,
    data_fingerprint: &str,
    entries: &[ComparisonEntry],
    output: Option<&Path>,
) -> Result<PathBuf, String> {
    let stem = base_summary
        .file_name()
        .and_then(|value| value.to_str())
        .and_then(|value| value.strip_suffix(".summary.json"))
        .ok_or_else(|| format!("比较基准不是 *.summary.json: {}", base_summary.display()))?;
    let output_path = output.map_or_else(
        || base_summary.with_file_name(format!("{stem}.comparison.html")),
        Path::to_path_buf,
    );
    if !output_path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("html") || value.eq_ignore_ascii_case("htm")
        })
    {
        return Err(format!(
            "比较报告输出路径必须以 .html 或 .htm 结尾: {}",
            output_path.display()
        ));
    }
    let parent = output_path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建比较报告目录失败 {}: {error}", parent.display()))?;
    let mut html = String::from(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>牵星回测对照报告</title><style>",
    );
    html.push_str(STYLE);
    html.push_str("</style></head><body><main><h1>牵星回测横向对照</h1><p class=\"sub\">标的 <b>");
    html.push_str(&escape_html(instrument));
    html.push_str("</b> · 数据指纹 <code>");
    html.push_str(&escape_html(data_fingerprint));
    html.push_str("</code></p><p class=\"note\">排名按收益率降序、最大回撤升序、运行摘要名升序；每个指标直接读取对应 v5/v6/v7 summary。可选基准指标按同一份 benchmark BarFrame 与每轮权益曲线逐时间戳精确对齐，基准指纹、价格口径、对齐与周期口径在表中列明。可信度分类列展示字段来源类别和 JSON Pointer，不代表数据或模型已独立认证。缺失指标显示 ");
    html.push_str(READOUT_ABSENT);
    html.push_str("，不按零参与比较。</p><div class=\"scroll\"><table><thead><tr><th>原始收益排名</th><th>主动收益排名</th><th>运行</th><th>策略</th><th>来源分类</th><th>信号参数</th><th>模型假设</th><th>撮合模型</th><th>风控规则</th><th>费用来源</th><th>收益率</th><th>最大回撤</th><th>基准标的</th><th>基准指纹</th><th>价格口径</th><th>对齐口径</th><th>期间口径</th><th>基准收益</th><th>主动收益</th><th>平均主动收益</th><th>跟踪误差</th><th>信息比率</th><th>夏普</th><th>索提诺</th><th>卡玛</th><th>胜率</th><th>收益因子</th><th>手续费</th><th>换手</th><th>期末权益</th><th>成交数</th><th>结果哈希</th></tr></thead><tbody>");
    for entry in entries {
        html.push_str("<tr><td>");
        html.push_str(&entry.rank.to_string());
        html.push_str("</td><td>");
        html.push_str(
            &entry
                .benchmark_rank
                .map_or_else(|| READOUT_ABSENT.into(), |rank| rank.to_string()),
        );
        html.push_str("</td><td><code>");
        html.push_str(&escape_html(&entry.run_id));
        html.push_str("</code></td><td>");
        html.push_str(&escaped_option(entry.strategy_id.as_deref()));
        html.push_str("</td><td>");
        html.push_str(&format_fidelity(&entry.fidelity));
        html.push_str("</td><td>");
        html.push_str(&format_document(entry.signal.as_ref()));
        html.push_str("</td><td>");
        html.push_str(&format_document(entry.assumptions.as_ref()));
        html.push_str("</td><td>");
        html.push_str(&format_document(entry.model_descriptors.as_ref()));
        html.push_str("</td><td>");
        html.push_str(&format_risk_rule(entry));
        html.push_str("</td><td>");
        html.push_str(&escaped_option(entry.execution_cost_source.as_deref()));
        html.push_str("</td><td class=\"positive\">");
        html.push_str(&format_bps(entry.return_bps));
        html.push_str("</td><td>");
        html.push_str(&format_bps(entry.max_drawdown_bps));
        html.push_str("</td><td>");
        html.push_str(&escaped_option(
            entry
                .benchmark
                .as_ref()
                .map(|value| value.instrument.as_str()),
        ));
        html.push_str("</td><td><code>");
        html.push_str(&escaped_option(
            entry
                .benchmark
                .as_ref()
                .map(|value| value.fingerprint.as_str()),
        ));
        html.push_str("</code></td><td>");
        html.push_str(&escaped_option(
            entry
                .benchmark
                .as_ref()
                .map(|value| value.price_basis.as_str()),
        ));
        html.push_str("</td><td>");
        html.push_str(&escaped_option(
            entry
                .benchmark
                .as_ref()
                .map(|value| value.alignment.as_str()),
        ));
        html.push_str("</td><td>");
        html.push_str(&escaped_option(
            entry
                .benchmark
                .as_ref()
                .map(|value| value.period_basis.as_str()),
        ));
        html.push_str("</td><td>");
        html.push_str(&format_optional_bps(
            entry
                .benchmark
                .as_ref()
                .and_then(|value| value.benchmark_return_bps),
        ));
        html.push_str("</td><td>");
        html.push_str(&format_optional_bps(
            entry
                .benchmark
                .as_ref()
                .and_then(|value| value.active_return_bps),
        ));
        html.push_str("</td><td>");
        html.push_str(&format_optional_bps(
            entry
                .benchmark
                .as_ref()
                .and_then(|value| value.mean_active_return_bps),
        ));
        html.push_str("</td><td>");
        html.push_str(&format_optional_bps(
            entry
                .benchmark
                .as_ref()
                .and_then(|value| value.tracking_error_bps),
        ));
        html.push_str("</td><td>");
        html.push_str(&format_ratio(
            entry
                .benchmark
                .as_ref()
                .and_then(|value| value.information_ratio),
        ));
        html.push_str("</td><td>");
        html.push_str(&format_ratio(entry.sharpe));
        html.push_str("</td><td>");
        html.push_str(&format_ratio(entry.sortino));
        html.push_str("</td><td>");
        html.push_str(&format_ratio(entry.calmar));
        html.push_str("</td><td>");
        html.push_str(&format_ratio(entry.win_rate));
        html.push_str("</td><td>");
        html.push_str(&format_ratio(entry.profit_factor));
        html.push_str("</td><td>");
        html.push_str(&format_money(entry.fees_raw));
        html.push_str("</td><td>");
        html.push_str(&format_money(entry.turnover_raw));
        html.push_str("</td><td>");
        html.push_str(&format_money(entry.final_equity_raw));
        html.push_str("</td><td>");
        html.push_str(&format_integer(entry.fills));
        html.push_str("</td><td><code>");
        html.push_str(&escaped_option(entry.result_hash.as_deref()));
        html.push_str("</code></td></tr>");
    }
    html.push_str("</tbody></table></div><footer>牵星 Qianxing · 离线报告 · 不连接真实交易所 · 历史回测结果不构成投资建议</footer></main></body></html>\n");
    std::fs::write(&output_path, html)
        .map_err(|error| format!("写入比较报告失败 {}: {error}", output_path.display()))?;
    Ok(output_path)
}

fn escaped_option(value: Option<&str>) -> String {
    value.map_or_else(|| READOUT_ABSENT.into(), escape_html)
}

fn format_bps(value: i128) -> String {
    let magnitude = value.unsigned_abs();
    let sign = if value < 0 { "-" } else { "+" };
    format!("{sign}{}.{:02}%", magnitude / 100, magnitude % 100)
}

fn format_ratio(value: Option<f64>) -> String {
    value.map_or_else(|| READOUT_ABSENT.into(), |value| format!("{value:.3}"))
}

fn format_optional_bps(value: Option<f64>) -> String {
    value.map_or_else(|| READOUT_ABSENT.into(), |value| format!("{value:+.2} bps"))
}

fn format_money(value: Option<i128>) -> String {
    value.map_or_else(|| READOUT_ABSENT.into(), crate::report_svg::money)
}

fn format_integer(value: Option<i128>) -> String {
    value.map_or_else(|| READOUT_ABSENT.into(), |value| value.to_string())
}

fn format_document(value: Option<&Value>) -> String {
    value.map_or_else(
        || READOUT_ABSENT.into(),
        |value| {
            format!(
                "<details><summary>查看</summary><code>{}</code></details>",
                escape_html(&value.to_string())
            )
        },
    )
}

fn format_fidelity(fidelity: &RunEvidenceFidelity) -> String {
    let groups = [
        ("观测", &fidelity.observed),
        ("配置", &fidelity.configured),
        ("估计", &fidelity.estimated),
        ("计算", &fidelity.computed),
        ("缺席", &fidelity.absent),
    ];
    let summary = groups
        .iter()
        .map(|(label, paths)| format!("{label} {} 项", paths.len()))
        .collect::<Vec<_>>()
        .join(" · ");
    let mut details = String::from("<details><summary>");
    details.push_str(&escape_html(&format!(
        "{} 工件 · {summary}",
        fidelity.source_artifact
    )));
    details.push_str("</summary>");
    for (label, paths) in groups {
        details.push_str("<p><b>");
        details.push_str(&escape_html(label));
        details.push_str("</b>: <code>");
        details.push_str(&escape_html(&paths.join(" · ")));
        details.push_str("</code></p>");
    }
    details.push_str("</details>");
    details
}

fn format_risk_rule(entry: &ComparisonEntry) -> String {
    match (
        entry.risk_rule_set_version.as_deref(),
        entry.risk_source.as_deref(),
    ) {
        (Some(version), Some(source)) => escape_html(&format!("{version} ({source})")),
        (Some(version), None) => escape_html(version),
        (None, Some(source)) => escape_html(source),
        (None, None) => READOUT_ABSENT.into(),
    }
}

const STYLE: &str = "body{margin:0;background:#f5f7fa;color:#202124;font:14px system-ui,'Microsoft YaHei',sans-serif}main{max-width:1500px;margin:32px auto;padding:24px;background:white;border-radius:12px;box-shadow:0 3px 18px #0001}h1{margin:0 0 8px}.sub,.note,footer{color:#5f6368}.scroll{overflow-x:auto}table{border-collapse:collapse;width:100%;white-space:nowrap;margin-top:20px}th,td{padding:10px 12px;border-bottom:1px solid #e5e7eb;text-align:right}th{background:#f3f5f8;position:sticky;top:0}th:nth-child(3),th:nth-child(4),td:nth-child(3),td:nth-child(4){text-align:left}.positive{color:#c0392b}code{font-family:ui-monospace,Consolas,monospace}footer{margin-top:22px}";
