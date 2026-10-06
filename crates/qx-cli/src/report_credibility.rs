//! 结果可信度面板（易用性 P1 §7 P1.4）：把"这份结果能不能信"拆成六格。
//!
//! 这一层刻意只做**读**：六格每一个都来自摘要已有的字段（input / replay / matching_kernel /
//! execution_costs / risk_rules / metrics），不引入第二个指标算式来源——与报告卡只印
//! `summary.json` 已有格子的纪律一致。颜色走独立的「可信 / 待核 / 缺席」三色（蓝 / 琥珀 / 灰），
//! 不碰收益的涨红跌绿语义，避免把"已验证"误读成"涨"。

use super::*;
use std::fmt::Write as _;

/// 渲染结果可信度面板；纯函数，不经文件系统，与 [`crate::report_html::render_report_html`]
/// 共用同一份摘要读法。
pub(crate) fn credibility_panel(summary: &serde_json::Value) -> String {
    let input = summary_block(summary, "input");
    let fingerprint = summary_text(summary, "/input/fingerprint");
    let dataset = summary_text(summary, "/input/dataset_id");
    let dataset_version = summary_text(summary, "/input/dataset_version");

    let replay = summary_block(summary, "replay");
    let replay_events = summary_number(summary, "/replay/events");
    let replay_ledger = summary_number(summary, "/replay/ledger_entries");
    let run_ledger = summary_number(summary, "/replay/run_ledger_entries");
    let replay_ok = replay.is_some()
        && summary_text(summary, "/replay/log_digest").is_some()
        && replay_events.is_some_and(|count| count > 0)
        && replay_ledger == run_ledger
        && replay_ledger.is_some();

    let kernel = summary_text(summary, "/matching_kernel");
    let cost = summary_text(summary, "/execution_costs/source");
    let risk_version = summary_text(summary, "/risk_rules/rule_set_version");
    let risk_source = summary_text(summary, "/risk_rules/source");

    let metric_fields = [
        ("return_bps", "/metrics/return_bps"),
        ("max_drawdown_bps", "/metrics/max_drawdown_bps"),
        ("fees_raw", "/metrics/fees_raw"),
        ("turnover_raw", "/metrics/turnover_raw"),
        ("final_equity_raw", "/metrics/final_equity_raw"),
    ];
    let uncomputed: Vec<&str> = metric_fields
        .iter()
        .filter(|(_, pointer)| summary_number(summary, pointer).is_none())
        .map(|(name, _)| *name)
        .collect();

    let rows: [(&str, &str, String); 7] = [
        (
            "输入身份",
            if input.is_some() && fingerprint.is_some() {
                "ok"
            } else {
                "absent"
            },
            match (&dataset, &dataset_version, &fingerprint) {
                (Some(d), Some(v), Some(f)) => format!("数据集 {d}@{v} · 指纹 {f}"),
                _ => "摘要未声明 input 块或指纹".to_string(),
            },
        ),
        (
            "重放校验",
            if replay_ok {
                "ok"
            } else if replay.is_some() {
                "warn"
            } else {
                "absent"
            },
            match (replay_events, replay_ledger, run_ledger) {
                (Some(e), Some(l), Some(r)) => format!("事件 {e} · 账簿 {l}/{r} · 日志摘要已落盘"),
                _ => "摘要未声明 replay 块".to_string(),
            },
        ),
        (
            "撮合档位",
            if kernel.is_some() { "ok" } else { "absent" },
            kernel.unwrap_or_else(|| "摘要未声明撮合内核".to_string()),
        ),
        (
            "成本模型",
            if cost.is_some() { "ok" } else { "absent" },
            cost.unwrap_or_else(|| "摘要未声明成本口径来源".to_string()),
        ),
        (
            "风控规则",
            if risk_version.is_some() {
                "ok"
            } else {
                "absent"
            },
            match (&risk_version, &risk_source) {
                (Some(v), Some(s)) => format!("规则集 {v}（{s}）"),
                _ => "摘要未声明风控规则版本".to_string(),
            },
        ),
        (
            "数据质量",
            if dataset.is_some() && fingerprint.is_some() {
                "ok"
            } else {
                "absent"
            },
            if dataset.is_some() && fingerprint.is_some() {
                format!(
                    "数据集 {} 已登记，输入指纹落盘（档位门禁已在回测前拦截）",
                    dataset.as_deref().unwrap()
                )
            } else {
                "摘要未登记数据集身份".to_string()
            },
        ),
        (
            "未计算字段",
            if uncomputed.is_empty() { "ok" } else { "warn" },
            if uncomputed.is_empty() {
                "收益率/回撤/费用/换手/期末权益均已计算".to_string()
            } else {
                format!(
                    "以下指标未计算：{}（按仓库纪律报 absent 而非 0）",
                    uncomputed.join(", ")
                )
            },
        ),
    ];

    let mut body = String::from("<table><tbody>");
    for (label, status, detail) in &rows {
        let zh = status_zh(status);
        let _ = write!(
            body,
            "<tr><th>{label}</th><td class=\"cstat {status}\">{zh}</td>\
             <td class=\"cdet\">{detail}</td></tr>",
            label = label,
            status = status,
            zh = zh,
            detail = escape_html(detail)
        );
    }
    body.push_str("</tbody></table>");
    format!("<section class=\"cred\"><h2>结果可信度</h2>{body}</section>")
}

/// 可信度三态的中文标签；与 `.cstat.ok/warn/absent` 的配色一一对应。
fn status_zh(status: &str) -> &'static str {
    match status {
        "ok" => "可信",
        "warn" => "待核",
        _ => "缺席",
    }
}
