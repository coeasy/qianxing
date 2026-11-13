//! Detailed presentation of already recorded facts; no financial metric calculation.
use super::escape_html;
use crate::report_readout::{summary_number, summary_text, READOUT_ABSENT};
use crate::report_svg::money;
use serde_json::Value;
use std::fmt::Write;

pub(super) fn currency(summary: &Value) -> Option<&str> {
    [
        "/metrics_exact/turnover/currency",
        "/account/currency",
        "/settlement_currency",
    ]
    .into_iter()
    .find_map(|path| {
        summary
            .pointer(path)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
    })
}

pub(super) fn amount(summary: &Value, exact: &str, raw: &str) -> String {
    let value = summary_text(summary, exact).or_else(|| summary_number(summary, raw).map(money));
    value.map_or_else(
        || READOUT_ABSENT.into(),
        |value| {
            format!(
                "{} {}",
                escape_html(&value),
                escape_html(currency(summary).unwrap_or("币种未提供"))
            )
        },
    )
}

pub(super) fn fees(summary: &Value) -> String {
    if let Some(fees) = summary
        .pointer("/metrics_exact/fees_by_currency")
        .and_then(Value::as_object)
        .filter(|fees| !fees.is_empty())
    {
        return fees
            .iter()
            .map(|(currency, value)| {
                format!(
                    "{} {}",
                    escape_html(&scalar(Some(value))),
                    escape_html(currency)
                )
            })
            .collect::<Vec<_>>()
            .join(" / ");
    }
    amount(summary, "/metrics_exact/fees", "/metrics/fees_raw")
}

fn scalar(value: Option<&Value>) -> String {
    value.filter(|value| !value.is_null()).map_or_else(
        || READOUT_ABSENT.into(),
        |value| {
            value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned)
        },
    )
}

fn fact_row(label: &str, value: &str) -> String {
    format!(
        "<tr><th>{}</th><td>{}</td></tr>",
        escape_html(label),
        escape_html(value)
    )
}

pub(super) fn overview(summary: &Value) -> String {
    let mut html = String::from(
        "<section class=\"report-detail\" id=\"report-overview\"><h2>回测范围与运行信息</h2><div class=\"detail-grid\"><table><tbody>",
    );
    for (label, pointer) in [
        ("策略", "/strategy_id"),
        ("交易标的", "/instrument"),
        ("输入记录数", "/bars"),
        ("采样单位", "/sample_unit"),
        ("撮合内核", "/matching_kernel"),
    ] {
        html.push_str(&fact_row(label, &scalar(summary.pointer(pointer))));
    }
    html.push_str(&fact_row(
        "结算币种",
        currency(summary).unwrap_or(READOUT_ABSENT),
    ));
    html.push_str("</tbody></table><table><tbody>");
    for (label, pointer) in [
        ("开始时间（Unix ms）", "/clock_start"),
        ("结束时间（Unix ms）", "/clock_end"),
    ] {
        let raw = scalar(summary.pointer(pointer));
        let _ = write!(
            html,
            "<tr><th>{label}</th><td><span data-timestamp=\"{}\">{}</span><br><code>{}</code></td></tr>",
            escape_html(&raw),
            escape_html(&raw),
            escape_html(&raw)
        );
    }
    for (label, pointer) in [
        ("结果身份", "/result_hash"),
        ("回放事件数", "/replay/events"),
        ("回放账本条目", "/replay/ledger_entries"),
        ("输入指纹", "/input/fingerprint"),
    ] {
        html.push_str(&fact_row(label, &scalar(summary.pointer(pointer))));
    }
    html.push_str("</tbody></table></div><p class=\"foot\">可读时间统一显示为 Asia/Shanghai，保留原始毫秒时间戳。示例数据或短样本不能用于推断策略盈利能力；未提供的事实显示为缺失。</p></section>");
    html
}

pub(super) fn configuration(summary: &Value) -> String {
    let mut html = String::from(
        "<section class=\"report-detail\" id=\"report-configuration\"><h2>策略参数与精确账户数据</h2><div class=\"detail-grid\"><article><h3>已记录的策略配置</h3><table><tbody>",
    );
    if let Some(signal) = summary.get("signal").and_then(Value::as_object) {
        for (name, value) in signal {
            html.push_str(&fact_row(name, &scalar(Some(value))));
        }
    } else {
        html.push_str(&fact_row(
            "策略参数",
            "摘要未提供；不从当前配置文件反推历史参数",
        ));
    }
    html.push_str("</tbody></table></article><article><h3>引擎记录的精确值</h3><table><tbody>");
    for (label, pointer) in [
        ("初始权益", "/metrics_exact/equity/initial"),
        ("期末权益", "/metrics_exact/equity/final"),
        ("权益变化金额", "/metrics_exact/equity/return"),
        ("收益（bps）", "/metrics_exact/equity/return_bps"),
        ("最大回撤金额", "/metrics_exact/equity/max_drawdown"),
        ("成交额", "/metrics_exact/turnover/amount"),
        ("结算币种", "/metrics_exact/turnover/currency"),
    ] {
        html.push_str(&fact_row(label, &scalar(summary.pointer(pointer))));
    }
    if let Some(fees) = summary
        .pointer("/metrics_exact/fees_by_currency")
        .and_then(Value::as_object)
    {
        for (currency, value) in fees {
            html.push_str(&fact_row(
                &format!("手续费 / {currency}"),
                &scalar(Some(value)),
            ));
        }
    }
    html.push_str("</tbody></table></article></div><p class=\"foot\">精确值直接取自已验证摘要；缺失时不在页面重新估算。declared_unused 表示该信号未使用的配置项。raw 表示内核兼容投影，不等同于高精度源值。</p></section>");
    html
}

pub(super) fn equity_table(equity: &[(i64, i128, Option<i128>)]) -> String {
    const LIMIT: usize = 500;
    let start = equity.len().saturating_sub(LIMIT);
    let mut html = format!(
        "<section class=\"report-detail\" id=\"report-equity\"><h2>权益与持仓逐点明细</h2><p class=\"foot\">展示末尾 {} / {} 个已验证 CSV 记录。金额展示为 raw 的 1e-9 投影，完整精度与原始记录以引擎产物为准。</p><div class=\"table-wrap\"><table><thead><tr><th>记录序号</th><th>时间（Asia/Shanghai）</th><th>时间戳 ms</th><th>权益投影</th><th>权益 raw</th><th>持仓 raw</th></tr></thead><tbody>",
        equity.len() - start,
        equity.len()
    );
    for (index, (ts, amount, position)) in equity.iter().enumerate().skip(start) {
        let _ = write!(
            html,
            "<tr><td>{index}</td><td data-timestamp=\"{ts}\">{ts}</td><td>{ts}</td><td>{}</td><td><code>{amount}</code></td><td><code>{}</code></td></tr>",
            money(*amount),
            position.map_or_else(|| READOUT_ABSENT.to_owned(), |value| value.to_string())
        );
    }
    html.push_str("</tbody></table></div></section>");
    html
}

pub(super) fn monthly_table(monthly: &[(i32, u32, i64)]) -> String {
    let mut html = String::from(
        "<section class=\"report-detail\"><h2>月度收益明细</h2><p class=\"foot\">与月度图使用同一份引擎报告序列；不重新定义收益或年化口径。</p><table><thead><tr><th>年月</th><th>收益 bps</th><th>收益 %</th></tr></thead><tbody>",
    );
    for (year, month, bps) in monthly {
        let _ = write!(
            html,
            "<tr><td>{year}-{month:02}</td><td>{bps}</td><td>{:+.2}%</td></tr>",
            *bps as f64 / 100.0
        );
    }
    html.push_str("</tbody></table></section>");
    html
}

pub(super) fn summary_download(summary: &Value) -> String {
    let pretty = serde_json::to_string_pretty(summary).unwrap_or_default();
    let payload = serde_json::to_string(summary)
        .unwrap_or_else(|_| "null".into())
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    format!(
        r#"<section class="report-detail" id="report-source"><h2>完整摘要与下载</h2><button id="download-summary" type="button">下载已验证摘要 JSON</button><details><summary>展开全部摘要字段</summary><pre>{}</pre></details><script type="application/json" id="verified-summary">{payload}</script></section><script>(function(){{const formatter=new Intl.DateTimeFormat('zh-CN',{{timeZone:'Asia/Shanghai',year:'numeric',month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit',second:'2-digit',hour12:false}});for(const node of document.querySelectorAll('[data-timestamp]')){{const ms=Number(node.dataset.timestamp);if(Number.isFinite(ms)&&Number.isFinite(new Date(ms).getTime()))node.textContent=formatter.format(new Date(ms));}}document.getElementById('download-summary').addEventListener('click',()=>{{const text=document.getElementById('verified-summary').textContent,url=URL.createObjectURL(new Blob([text],{{type:'application/json;charset=utf-8'}})),a=document.createElement('a');a.href=url;a.download='verified-backtest-summary.json';a.click();setTimeout(()=>URL.revokeObjectURL(url),1000);}});}})();</script>"#,
        escape_html(&pretty)
    )
}
