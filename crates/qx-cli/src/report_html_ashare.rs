//! HTML presentation for the versioned A-share analysis block.

use crate::report_html::escape_html;
use std::fmt::Write as _;

pub(crate) fn ashare_analysis_html_section(summary: &serde_json::Value) -> Option<String> {
    let analysis = summary.get("ashare_analysis")?;
    let mut html = String::from("<section class=\"prov\"><h2>A 股公司行为分析</h2>");
    if let Some(snapshots) = analysis
        .get("issuer_capital_snapshots")
        .and_then(serde_json::Value::as_array)
    {
        html.push_str("<h3>发行人股本快照</h3><table><thead><tr><th>标的</th><th>生效时间</th><th>总股本 raw</th><th>流通股本 raw</th><th>来源</th></tr></thead><tbody>");
        for snapshot in snapshots {
            let field = |name: &str| scalar_text(snapshot.get(name));
            let _ = write!(
                html,
                "<tr><td>{}</td><td>{}</td><td><code>{}</code></td><td><code>{}</code></td><td>{}</td></tr>",
                escape_html(&field("instrument")),
                escape_html(&field("effective_ts")),
                escape_html(&field("total_shares_raw")),
                escape_html(&field("free_float_shares_raw")),
                escape_html(&field("source")),
            );
        }
        html.push_str("</tbody></table>");
    }
    if let Some(snapshot) = analysis.get("issuer_capital_at_end") {
        html.push_str(&format!(
            "<p>回测结束时有效股本：<b>{}</b>，总股本 raw=<code>{}</code>，生效时间={}</p>",
            escape_html(&scalar_text(snapshot.get("instrument"))),
            escape_html(&scalar_text(snapshot.get("total_shares_raw"))),
            escape_html(&scalar_text(snapshot.get("effective_ts"))),
        ));
    }
    if let Some(entitlements) = analysis
        .get("entitlements")
        .and_then(serde_json::Value::as_array)
    {
        html.push_str("<h3>权益余额（raw）</h3><table><thead><tr><th>标的</th><th>币种</th><th>事实类型</th><th>配股权利</th><th>现金分红</th><th>可转债利息</th></tr></thead><tbody>");
        for entitlement in entitlements {
            let kinds = entitlement
                .get("action_kinds")
                .and_then(serde_json::Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<Vec<_>>()
                        .join("、")
                })
                .unwrap_or_default();
            let _ = write!(
                html,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td><code>{}</code></td><td><code>{}</code></td><td><code>{}</code></td></tr>",
                escape_html(&scalar_text(entitlement.get("instrument"))),
                escape_html(&scalar_text(entitlement.get("currency"))),
                escape_html(&kinds),
                escape_html(&scalar_text(entitlement.get("rights_entitlement_raw"))),
                escape_html(&scalar_text(
                    entitlement.get("cash_dividend_entitlement_raw")
                )),
                escape_html(&scalar_text(
                    entitlement.get("convertible_bond_interest_entitlement_raw")
                )),
            );
        }
        html.push_str("</tbody></table>");
    }
    html.push_str("</section>");
    Some(html)
}

fn scalar_text(value: Option<&serde_json::Value>) -> String {
    value.filter(|value| !value.is_null()).map_or_else(
        || "—".to_string(),
        |value| {
            value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string)
        },
    )
}
