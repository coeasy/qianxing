//! Bounded strategy correctness diagnostics projection for the standalone HTML report.

use super::escape_html;
use std::fmt::Write as _;

fn diagnostic_fields(value: Option<&serde_json::Value>) -> String {
    let values = value
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    if values.is_empty() {
        "—".into()
    } else {
        values
    }
}

pub(super) fn strategy_bias_panel(summary: &serde_json::Value) -> String {
    let Some(report) = summary.get("strategy_bias_diagnostics") else {
        return String::new();
    };
    let status = |pointer: &str| {
        report
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("未提供")
    };
    let lookahead = status("/lookahead/status");
    let startup = status("/startup/status");
    let visibility = status("/data_visibility/status");
    let adjustment = status("/data_visibility/adjustment_chain/status");
    let adjustment_basis = status("/data_visibility/adjustment_chain/price_adjustment_basis");
    let fingerprint = status("/data_fingerprint");
    let input_unit = if report.get("input_events").is_some() {
        "事件"
    } else {
        "Bar"
    };
    let mut rows: Vec<(String, String, String, String)> = Vec::new();
    if let Some(checks) = report
        .pointer("/lookahead/checks")
        .and_then(serde_json::Value::as_array)
    {
        for check in checks
            .iter()
            .filter(|row| {
                row.get("future_perturbation_changed_decision")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                    || row
                        .get("intermediate_state_changed")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
            })
            .take(100)
        {
            rows.push((
                "未来数据扰动".into(),
                check
                    .get("decision_as_of")
                    .map_or("—".into(), ToString::to_string),
                check
                    .get("perturbation_profile")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("—")
                    .into(),
                format!(
                    "决策: {}；中间态: {}",
                    diagnostic_fields(check.get("changed_decision_fields")),
                    diagnostic_fields(check.get("intermediate_state_changed_fields")),
                ),
            ));
        }
    }
    if let Some(windows) = report
        .pointer("/startup/windows")
        .and_then(serde_json::Value::as_array)
    {
        for window in windows
            .iter()
            .filter(|row| {
                row.get("changed_decision_fields")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|fields| !fields.is_empty())
                    || row
                        .get("intermediate_state_changed_fields")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|fields| !fields.is_empty())
            })
            .take(100)
        {
            rows.push((
                "预热窗口".into(),
                report.get("as_of").map_or("—".into(), ToString::to_string),
                format!(
                    "请求 {} {input_unit}，可见 {} {input_unit}",
                    window
                        .get("requested_events")
                        .or_else(|| window.get("requested_bars"))
                        .map_or("—".into(), ToString::to_string),
                    window
                        .get("available_events")
                        .or_else(|| window.get("available_bars"))
                        .map_or("—".into(), ToString::to_string),
                ),
                format!(
                    "决策: {}；中间态: {}",
                    diagnostic_fields(window.get("changed_decision_fields")),
                    diagnostic_fields(window.get("intermediate_state_changed_fields")),
                ),
            ));
        }
    }
    if let Some(trajectory) = report.pointer("/startup/trajectory") {
        rows.push((
            "预热历史轨迹覆盖".into(),
            report.get("as_of").map_or("—".into(), ToString::to_string),
            format!(
                "{} / {} 个可比决策点；全量 {}；参考历史不足排除 {}；已抽样: {}；worker 重放: {}",
                trajectory
                    .get("decision_points_checked")
                    .map_or("0".into(), ToString::to_string),
                trajectory
                    .get("decision_points_eligible")
                    .or_else(|| trajectory.get("decision_points_available"))
                    .map_or("0".into(), ToString::to_string),
                trajectory
                    .get("decision_points_available")
                    .map_or("0".into(), ToString::to_string),
                trajectory
                    .get("decision_points_excluded_insufficient_reference_history")
                    .map_or("0".into(), ToString::to_string),
                trajectory
                    .get("decision_points_sampled")
                    .map_or("未知".into(), ToString::to_string),
                trajectory
                    .get("worker_replays")
                    .map_or("0".into(), ToString::to_string),
            ),
            trajectory
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("未计算")
                .into(),
        ));
        if let Some(divergence) = trajectory.get("first_divergence") {
            rows.push((
                "预热历史轨迹首次分歧".into(),
                divergence
                    .get("decision_at")
                    .or_else(|| divergence.get("decision_timestamp"))
                    .map_or("—".into(), ToString::to_string),
                format!(
                    "请求 {} {input_unit}，对照 {} {input_unit}",
                    divergence
                        .get("requested_events")
                        .or_else(|| divergence.get("requested_bars"))
                        .map_or("—".into(), ToString::to_string),
                    divergence
                        .get("reference_window_events")
                        .or_else(|| divergence.get("reference_window_bars"))
                        .map_or("—".into(), ToString::to_string),
                ),
                format!(
                    "决策: {}；中间态: {}",
                    diagnostic_fields(
                        divergence
                            .get("changed_decision_fields")
                            .or_else(|| divergence.get("changed_fields")),
                    ),
                    diagnostic_fields(
                        divergence
                            .get("intermediate_state_changed_fields")
                            .or_else(|| divergence.get("changed_intermediate_fields")),
                    ),
                ),
            ));
        }
    }
    if let Some(examples) = report
        .pointer("/data_visibility/examples")
        .and_then(serde_json::Value::as_object)
    {
        for key in [
            "future_value_unavailable",
            "revised_value_after_decision",
            "availability_unknown",
        ] {
            if let Some(items) = examples.get(key).and_then(serde_json::Value::as_array) {
                for item in items.iter().take(25) {
                    rows.push((
                        key.into(),
                        item.get("decision_at")
                            .map_or("—".into(), ToString::to_string),
                        format!(
                            "{} · {}",
                            item.get("entity_id")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("—"),
                            item.get("field")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("—"),
                        ),
                        format!(
                            "事实 {}；生效 {}；版本可用时间 {}",
                            item.get("fact_id")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("—"),
                            item.get("effective_at")
                                .map_or("—".into(), ToString::to_string),
                            item.get("available_at")
                                .map_or("—".into(), ToString::to_string),
                        ),
                    ));
                }
            }
        }
    }
    if let Some(examples) = report
        .pointer("/data_visibility/adjustment_chain/examples")
        .and_then(serde_json::Value::as_array)
    {
        for item in examples.iter().take(25) {
            rows.push((
                "公司行为与价格修订时间关联（非因果证明）".into(),
                item.get("price_revision_available_at")
                    .map_or("—".into(), ToString::to_string),
                format!(
                    "{} · {} ↔ {}",
                    item.get("price_entity_id").and_then(serde_json::Value::as_str).unwrap_or("—"),
                    item.get("price_field").and_then(serde_json::Value::as_str).unwrap_or("—"),
                    item.get("action_field").and_then(serde_json::Value::as_str).unwrap_or("—"),
                ),
                format!(
                    "口径 {}；动作生效 {}；影响 {} 个决策点（动作前 {} 个）；可能后复权前视: {}；归因: 仅时间相关性",
                    adjustment_basis,
                    item.get("action_effective_at").map_or("—".into(), ToString::to_string),
                    item.get("affected_decision_points").map_or("0".into(), ToString::to_string),
                    item.get("pre_action_decision_points").map_or("0".into(), ToString::to_string),
                    item.get("potential_back_adjustment_lookahead")
                        .map_or("未知".into(), ToString::to_string),
                ),
            ));
        }
    }
    let mut html = String::new();
    html.push_str("<section class=\"strategy-diagnostics\" aria-label=\"策略正确性诊断\"><h2>策略正确性诊断</h2>");
    html.push_str("<p class=\"chart-note\">诊断策略与回测输入身份已核对；无变化仅表示本次检查未发现变化，不证明不存在所有偏差。</p>");
    let _ = write!(
        html,
        "<div class=\"diagnostic-status\"><span>未来数据 <b>{}</b></span><span>预热 <b>{}</b></span><span>PIT <b>{}</b></span><span>复权链 <b>{}</b>（口径 {}）</span></div><p class=\"chart-note\">数据指纹 <code>{}</code></p>",
        escape_html(lookahead),
        escape_html(startup),
        escape_html(visibility),
        escape_html(adjustment),
        escape_html(adjustment_basis),
        escape_html(fingerprint),
    );
    let _ = write!(
        html,
        "<details><summary>查看诊断发现（展示 {} 项；未来数据最多 100 项、预热最多 100 项、每类 PIT 与复权关联最多 25 项）</summary>",
        rows.len()
    );
    if rows.is_empty() {
        html.push_str("<p>本次检查没有产生可列出的变化项。</p>");
    } else {
        html.push_str("<label>筛选时点、类型或字段 <input id=\"diagnostic-filter\" type=\"search\"></label><div class=\"diagnostic-scroll\"><table id=\"diagnostic-table\"><thead><tr><th>类型</th><th>决策时点</th><th>对象</th><th>变化</th></tr></thead><tbody>");
        for (kind, when, subject, detail) in rows {
            let searchable = format!("{kind} {when} {subject} {detail}");
            let _ = write!(
                html,
                "<tr data-search=\"{}\"><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(&searchable),
                escape_html(&kind),
                escape_html(&when),
                escape_html(&subject),
                escape_html(&detail),
            );
        }
        html.push_str("</tbody></table></div><script>const diagnosticFilter=document.getElementById('diagnostic-filter');diagnosticFilter.addEventListener('input',()=>{const query=diagnosticFilter.value.trim().toLocaleLowerCase();for(const row of document.querySelectorAll('#diagnostic-table tbody tr'))row.hidden=!row.dataset.search.toLocaleLowerCase().includes(query);});</script>");
    }
    html.push_str("</details></section>");
    html
}
