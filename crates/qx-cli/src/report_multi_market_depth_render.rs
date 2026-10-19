use serde_json::Value;

pub(crate) fn workbook_rows(report: &Value) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
    let overview: Vec<Vec<String>> = [
        "run_id",
        "account_id",
        "result_hash",
        "data_fingerprint",
        "markets_total",
        "successful_fills",
        "pending_orders",
        "equity_points",
        "final_equity_raw",
        "return_bps",
        "max_drawdown_bps",
    ]
    .into_iter()
    .map(|key| {
        vec![
            key.to_string(),
            report.get(key).map_or("—".into(), |value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string())
            }),
        ]
    })
    .collect();
    let mut quality = vec![vec![
        "instrument".into(),
        "tier".into(),
        "source".into(),
        "snapshots".into(),
        "fills".into(),
        "manifest_fingerprint".into(),
        "interval_ms".into(),
        "missing_intervals".into(),
        "duplicate_rows".into(),
        "out_of_order_rows".into(),
        "quality_status".into(),
    ]];
    for market in report["markets"].as_array().into_iter().flatten() {
        let q = &market["dataset_quality"];
        let r = &q["quality_report"];
        quality.push(vec![
            strval(&market["instrument"]),
            strval(&market["data_tier"]),
            strval(&market["source"]),
            strval(&market["snapshot_count"]),
            strval(&market["fills"]),
            strval(&q["manifest_fingerprint"]),
            strval(&r["interval_ms"]),
            strval(&r["missing_intervals"]),
            strval(&r["duplicate_rows"]),
            strval(&r["out_of_order_rows"]),
            if q.is_null() {
                "not_provided".into()
            } else {
                strval(&q["status"])
            },
        ]);
    }
    (
        std::iter::once(vec!["Field".into(), "Value".into()])
            .chain(overview)
            .collect(),
        quality,
    )
}

pub(crate) fn strval(value: &Value) -> String {
    if value.is_null() {
        "—".into()
    } else if let Some(text) = value.as_str() {
        text.to_string()
    } else {
        value.to_string()
    }
}

pub(crate) fn render_html(report: &Value) -> String {
    let mut html = String::from(
        "<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>多市场深度回测报告</title><style>body{font:15px system-ui,sans-serif;max-width:1200px;margin:2rem auto;padding:0 1rem;color:#172033}h1{margin-bottom:.3rem}.muted{color:#64748b}.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:10px}.card,section{border:1px solid #dbe3ee;border-radius:10px;padding:14px;margin:12px 0}.card b{display:block;font-size:1.2rem}table{border-collapse:collapse;width:100%;font-size:.9rem}th,td{text-align:left;border-bottom:1px solid #e2e8f0;padding:9px;overflow-wrap:anywhere}code{overflow-wrap:anywhere}</style><body><h1>多市场深度回测报告</h1>",
    );
    html.push_str(&format!(
        "<p class=\"muted\">运行 <code>{}</code> · 结果 <code>{}</code></p><div class=\"cards\">",
        esc(&strval(&report["run_id"])),
        esc(&strval(&report["result_hash"]))
    ));
    for (label, key) in [
        ("收益 (bps)", "return_bps"),
        ("最大回撤 (bps)", "max_drawdown_bps"),
        ("成交数", "successful_fills"),
        ("待成交订单", "pending_orders"),
        ("期末权益 raw", "final_equity_raw"),
    ] {
        html.push_str(&format!(
            "<div class=\"card\">{}<b>{}</b></div>",
            label,
            esc(&strval(&report[key]))
        ));
    }
    html.push_str("</div><section><h2>市场与数据质量</h2><table><thead><tr><th>标的</th><th>档位</th><th>来源</th><th>快照</th><th>成交</th><th>质量清单指纹</th><th>间隔 ms</th><th>缺口</th><th>重复</th><th>乱序</th><th>状态</th></tr></thead><tbody>");
    for market in report["markets"].as_array().into_iter().flatten() {
        let q = &market["dataset_quality"];
        let r = &q["quality_report"];
        let status = if q.is_null() {
            "未提供"
        } else if q["status"] == "clean" {
            "通过"
        } else {
            "有质量告警"
        };
        html.push_str("<tr>");
        for value in [
            &market["instrument"],
            &market["data_tier"],
            &market["source"],
            &market["snapshot_count"],
            &market["fills"],
            &q["manifest_fingerprint"],
            &r["interval_ms"],
            &r["missing_intervals"],
            &r["duplicate_rows"],
            &r["out_of_order_rows"],
        ] {
            html.push_str(&format!("<td>{}</td>", esc(&strval(value))));
        }
        html.push_str(&format!("<td>{status}</td></tr>"));
    }
    html.push_str("</tbody></table></section><p class=\"muted\">输入帧及质量清单在导出时重新校验。本地回测结果不代表真实 venue 成交。</p></body></html>");
    html
}

pub(crate) fn render_comparison_html(report: &Value) -> String {
    let mut html = String::from(
        "<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>多市场深度运行对照</title><style>body{font:15px system-ui,sans-serif;max-width:1100px;margin:2rem auto;padding:0 1rem;color:#172033}table{border-collapse:collapse;width:100%}th,td{text-align:left;border-bottom:1px solid #dbe3ee;padding:9px}code{overflow-wrap:anywhere}.muted{color:#64748b}</style><h1>多市场深度运行对照</h1>",
    );
    html.push_str(&format!("<p class=\"muted\">相同输入指纹 <code>{}</code></p><table><thead><tr><th>排名</th><th>运行</th><th>收益 bps</th><th>最大回撤 bps</th><th>成交</th><th>结果哈希</th></tr></thead><tbody>", esc(&strval(&report["data_fingerprint"]))));
    for row in report["ranking"].as_array().into_iter().flatten() {
        html.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><code>{}</code></td></tr>", esc(&strval(&row["rank"])), esc(&strval(&row["run_id"])), esc(&strval(&row["return_bps"])), esc(&strval(&row["max_drawdown_bps"])), esc(&strval(&row["successful_fills"])), esc(&strval(&row["result_hash"]))));
    }
    html.push_str("</tbody></table><p class=\"muted\">仅比较市场顺序和输入数据指纹相同的运行；排序为收益降序、回撤升序、run_id 升序。</p></html>");
    html
}

fn esc(text: &str) -> String {
    crate::report_html::escape_html(text)
}
