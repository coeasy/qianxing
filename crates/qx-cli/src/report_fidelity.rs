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
    html.push_str("</section>");
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
