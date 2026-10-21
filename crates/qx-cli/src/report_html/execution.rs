use super::escape_html;
use std::fmt::Write;

pub(super) fn execution_attempts_section(summary: &serde_json::Value) -> String {
    let Some(attempts) = summary
        .get("execution_attempts")
        .and_then(serde_json::Value::as_array)
    else {
        return String::new();
    };
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for attempt in attempts {
        if let Some(reason) = attempt.get("reason").and_then(serde_json::Value::as_str) {
            *counts.entry(reason.to_string()).or_default() += 1;
        }
    }
    let mut html = format!(
        "<section><h2>逐单执行记录</h2><p>执行评估 {} 次</p>",
        attempts.len()
    );
    if !counts.is_empty() {
        html.push_str("<table><thead><tr><th>原因</th><th>次数</th></tr></thead><tbody>");
        for (reason, count) in counts {
            let _ = write!(
                html,
                "<tr><td>{}</td><td>{count}</td></tr>",
                escape_html(&reason)
            );
        }
        html.push_str("</tbody></table>");
    }
    html.push_str("<details><summary>查看逐笔委托记录</summary><div class=\"table-wrap\"><table><thead><tr><th>时间</th><th>订单</th><th>标的</th><th>方向</th><th>请求量(raw)</th><th>成交量(raw)</th><th>剩余量(raw)</th><th>成交价(raw)</th><th>盘口显示量(raw)</th><th>队列前量(raw)</th><th>队列后量(raw)</th><th>估算成交量(raw)</th><th>盘口减少量(raw)</th><th>用于前队列(raw)</th><th>原因</th></tr></thead><tbody>");
    for attempt in attempts {
        let field = |name: &str| {
            attempt.get(name).map_or_else(
                || "—".to_string(),
                |value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| value.to_string())
                },
            )
        };
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            escape_html(&field("ts")),
            escape_html(&field("order_id")),
            escape_html(&field("instrument")),
            escape_html(&field("side")),
            escape_html(&field("requested_qty_raw")),
            escape_html(&field("filled_qty_raw")),
            escape_html(&field("remaining_qty_raw")),
            escape_html(&field("price_raw")),
            escape_html(&field("displayed_qty_raw")),
            escape_html(&field("queue_ahead_before_raw")),
            escape_html(&field("queue_ahead_after_raw")),
            escape_html(&field("estimated_volume_raw")),
            escape_html(&field("displayed_reduction_raw")),
            escape_html(&field("queue_consumed_raw")),
            escape_html(&field("reason"))
        );
    }
    html.push_str("</tbody></table></div></details></section>");
    html
}
