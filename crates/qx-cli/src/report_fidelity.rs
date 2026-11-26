//! 报告中把可核验记录、模型假设和回测计算结果分开展示。

use crate::report_html::escape_html;
use crate::report_readout::{summary_number, summary_text, READOUT_ABSENT};
use serde_json::Value;
use std::fmt::Write as _;

pub(crate) fn fidelity_panel(summary: &Value) -> String {
    let mut html = String::from("<section class=\"fidelity\"><h2>数据与模型口径</h2>");
    html.push_str("<div class=\"fidelity-grid\">");
    html.push_str(&table(
        "可核验的输入与重放记录",
        &[
            ("数据集", text(summary, "/input/dataset_id")),
            ("数据集版本", text(summary, "/input/dataset_version")),
            ("输入指纹", text(summary, "/input/fingerprint")),
            ("输入 Bar 数", number(summary, "/bars")),
            ("重放事件数", number(summary, "/replay/events")),
            ("重放日志摘要", text(summary, "/replay/log_digest")),
        ],
    ));
    html.push_str(&table(
        "模型选择与估计假设",
        &[
            ("撮合内核", text(summary, "/matching_kernel")),
            ("成交模型", fill_model(summary)),
            ("盘口执行模型", execution_model(summary)),
            ("执行成本来源", text(summary, "/execution_costs/source")),
            ("模型描述子", string_list(summary, "model_descriptors")),
            ("显式假设", string_list(summary, "assumptions")),
        ],
    ));
    html.push_str("</div>");
    html.push_str(
        "<p class=\"fidelity-note\">回测计算结果来自上述数据与模型，属于历史模拟结果；
                 成交数值不是实盘成交观测。</p>",
    );
    html.push_str(&table(
        "回测计算结果（非实盘观测）",
        &[
            ("收益率（bps）", number(summary, "/metrics/return_bps")),
            (
                "最大回撤（bps）",
                number(summary, "/metrics/max_drawdown_bps"),
            ),
            ("手续费（定点原值）", number(summary, "/metrics/fees_raw")),
            (
                "期末权益（定点原值）",
                number(summary, "/metrics/final_equity_raw"),
            ),
            ("模拟成交数", number(summary, "/fills")),
            ("拒单数", number(summary, "/rejected_orders")),
        ],
    ));
    html.push_str(&rejection_details_panel(summary));
    html.push_str("</section>");
    html
}

fn rejection_details_panel(summary: &Value) -> String {
    let Some(readout) = summary.get("rejection_detail_readout") else {
        return "<p class=\"fidelity-note\">该旧版摘要未保存逐笔拒单快照。</p>".to_string();
    };
    let details = readout
        .get("details")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let recorded = readout
        .get("recorded_count")
        .and_then(Value::as_u64)
        .unwrap_or(details.len() as u64);
    let available = readout
        .get("available_count")
        .and_then(Value::as_u64)
        .unwrap_or(recorded);
    let omitted = readout
        .get("omitted_count")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let unavailable = readout
        .get("unavailable_count")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let mut html = format!(
        "<details class=\"rejection-details\"><summary>逐笔拒单明细（保存 {recorded}/{available}，缺少快照 {unavailable}）</summary>"
    );
    if details.is_empty() {
        html.push_str("<p>本次没有可展示的逐笔拒单快照。</p>");
    } else {
        html.push_str("<div class=\"table-wrap\"><table><thead><tr><th>订单</th><th>时间</th><th>标的</th><th>方向</th><th>委托数量</th><th>参考/限价</th><th>拒绝原因</th></tr></thead><tbody>");
        for detail in details {
            let order_id = detail
                .get("client_order_id")
                .map_or_else(|| "—".to_string(), Value::to_string);
            let timestamp = detail.get("ts").and_then(Value::as_u64);
            let timestamp_cell = timestamp.map_or_else(
                || "—".to_string(),
                |ts| format!("<time data-timestamp=\"{ts}\">{ts}</time>"),
            );
            let instrument = detail
                .get("instrument")
                .and_then(Value::as_str)
                .unwrap_or("—");
            let side = detail.get("side").and_then(Value::as_str).unwrap_or("—");
            let quantity = detail
                .get("quantity_exact")
                .and_then(Value::as_str)
                .unwrap_or("—");
            let reference_price = detail
                .get("reference_price_exact")
                .and_then(Value::as_str)
                .or_else(|| detail.get("limit_price_exact").and_then(Value::as_str))
                .unwrap_or("—");
            let reason = detail.get("reason").and_then(Value::as_str).unwrap_or("—");
            let _ = write!(
                html,
                "<tr><td>{}</td><td>{timestamp_cell}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(&order_id),
                escape_html(instrument),
                escape_html(side),
                escape_html(quantity),
                escape_html(reference_price),
                escape_html(reason),
            );
        }
        html.push_str("</tbody></table></div>");
    }
    if omitted > 0 {
        let _ = write!(
            html,
            "<p class=\"fidelity-note\">为控制报告大小，另有 {omitted} 条已记录快照未展开。</p>"
        );
    }
    html.push_str("</details>");
    html
}

fn text(summary: &Value, pointer: &str) -> String {
    summary_text(summary, pointer).unwrap_or_else(|| READOUT_ABSENT.to_string())
}

fn number(summary: &Value, pointer: &str) -> String {
    summary_number(summary, pointer)
        .map_or_else(|| READOUT_ABSENT.to_string(), |value| value.to_string())
}

fn fill_model(summary: &Value) -> String {
    match summary.get("fill_model") {
        Some(model) => format!(
            "{}@{}（来源：{}；参数：{}；假设：{}）",
            model
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(READOUT_ABSENT),
            model
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or(READOUT_ABSENT),
            model
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or(READOUT_ABSENT),
            model
                .get("parameters")
                .and_then(Value::as_str)
                .unwrap_or(READOUT_ABSENT),
            model
                .get("assumption")
                .and_then(Value::as_str)
                .unwrap_or(READOUT_ABSENT)
        ),
        None => READOUT_ABSENT.to_string(),
    }
}

fn execution_model(summary: &Value) -> String {
    let Some(model) = summary.get("execution_model") else {
        return READOUT_ABSENT.to_string();
    };
    let queue = model.get("queue_position");
    let limits = queue
        .and_then(|value| value.get("limitations"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("、")
        })
        .unwrap_or_default();
    format!(
        "{}@{} fee={}bps latency={} snapshots impact={}bps; queue={}@{} initial={}bps advance={}bps; {}{}",
        model
            .get("kernel")
            .and_then(Value::as_str)
            .unwrap_or(READOUT_ABSENT),
        model
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or(READOUT_ABSENT),
        model.get("fee_bps").unwrap_or(&Value::Null),
        model.get("latency_snapshots").unwrap_or(&Value::Null),
        model.get("market_impact_bps").unwrap_or(&Value::Null),
        queue
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(READOUT_ABSENT),
        queue
            .and_then(|value| value.get("version"))
            .and_then(Value::as_str)
            .unwrap_or(READOUT_ABSENT),
        queue
            .and_then(|value| value.get("initial_queue_bps"))
            .unwrap_or(&Value::Null),
        queue
            .and_then(|value| value.get("volume_advance_bps"))
            .unwrap_or(&Value::Null),
        queue
            .and_then(|value| value.get("assumption"))
            .and_then(Value::as_str)
            .unwrap_or(READOUT_ABSENT),
        if limits.is_empty() {
            String::new()
        } else {
            format!("；限制：{limits}")
        },
    )
}

fn string_list(summary: &Value, key: &str) -> String {
    let Some(values) = summary.get(key).and_then(Value::as_array) else {
        return READOUT_ABSENT.to_string();
    };
    if values.is_empty() {
        return READOUT_ABSENT.to_string();
    }
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string())
        })
        .collect::<Vec<_>>()
        .join("；")
}

fn table(title: &str, rows: &[(&str, String)]) -> String {
    let mut html = format!("<article><h3>{}</h3><table><tbody>", escape_html(title));
    for (label, value) in rows {
        let _ = write!(
            html,
            "<tr><th>{}</th><td>{}</td></tr>",
            escape_html(label),
            escape_html(value)
        );
    }
    html.push_str("</tbody></table></article>");
    html
}

#[cfg(test)]
mod tests {
    use super::fidelity_panel;

    #[test]
    fn fidelity_panel_renders_escaped_rejection_details_and_legacy_note() {
        let summary = serde_json::json!({
            "rejected_orders": 1,
            "rejection_detail_readout": {
                "recorded_count": 1,
                "available_count": 1,
                "omitted_count": 0,
                "unavailable_count": 0,
                "details": [{
                    "client_order_id": 7,
                    "ts": 1_000,
                    "instrument": "BTC/USDT.BINANCE",
                    "side": "buy",
                    "quantity_exact": "0.001",
                    "reference_price_exact": "100",
                    "reason": "<script>alert(1)</script>"
                }]
            }
        });
        let html = fidelity_panel(&summary);
        assert!(html.contains("逐笔拒单明细（保存 1/1，缺少快照 0）"));
        assert!(html.contains("<time data-timestamp=\"1000\">1000</time>"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert(1)</script>"));

        let legacy = fidelity_panel(&serde_json::json!({ "rejected_orders": 0 }));
        assert!(legacy.contains("该旧版摘要未保存逐笔拒单快照"));
    }
}
