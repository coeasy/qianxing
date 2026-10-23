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
    html.push_str("</tbody></table></section>");
    render_strategy_diagnostics(report, &mut html);
    html.push_str("<p class=\"muted\">输入帧及质量清单在导出时重新校验。本地回测结果不代表真实 venue 成交。</p></body></html>");
    html
}

fn render_strategy_diagnostics(report: &Value, html: &mut String) {
    let Some(diagnostics) = report
        .get("market_strategy_diagnostics")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
    else {
        return;
    };
    html.push_str("<section id=\"strategy-diagnostics\"><h2>策略未来数据与预热诊断</h2><p class=\"muted\">诊断来源和计算边界均写入报告；Rust 内置策略会重放预热轨迹，并扰动采样的未来 Bar 后复核此前决策；供应商 PIT 仍标注为未知。</p><label>筛选标的或策略 <input id=\"diagnostic-filter\" type=\"search\" placeholder=\"输入 instrument / strategy_id\"></label> <label>未来数据状态 <select id=\"diagnostic-lookahead-filter\"><option value=\"\">全部</option><option value=\"potential_lookahead_detected\">检测到变化</option><option value=\"no_change_detected\">扰动后此前决策未变化</option><option value=\"causal_event_contract\">因果事件接口；未做扰动</option><option value=\"insufficient_history\">历史不足</option><option value=\"insufficient_work_budget\">工作量预算不足</option></select></label><table><thead><tr><th>标的</th><th>策略</th><th>输入覆盖</th><th>未来数据</th><th>预热</th><th>PIT</th><th>诊断明细</th></tr></thead><tbody id=\"diagnostic-rows\">");
    for item in diagnostics {
        let diagnostic = &item["report"];
        let instrument = strval(&item["instrument"]);
        let strategy_id = strval(&item["strategy_id"]);
        let lookahead = strval(
            &diagnostic
                .pointer("/lookahead/status")
                .cloned()
                .unwrap_or(Value::Null),
        );
        let startup = strval(
            &diagnostic
                .pointer("/startup/status")
                .cloned()
                .unwrap_or(Value::Null),
        );
        let pit = strval(
            &diagnostic
                .pointer("/data_visibility/status")
                .cloned()
                .unwrap_or(Value::Null),
        );
        let coverage = format!(
            "{} · {} · 截至 {}",
            strval(&diagnostic["input_kind"]),
            strval(&diagnostic["input_events"]),
            strval(&diagnostic["as_of"])
        );
        let detail = serde_json::to_string_pretty(diagnostic).unwrap_or_else(|_| "{}".into());
        html.push_str(&format!(
            "<tr data-instrument=\"{}\" data-strategy=\"{}\" data-lookahead=\"{}\"><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><details><summary>查看</summary><pre>{}</pre></details></td></tr>",
            esc(&instrument), esc(&strategy_id), esc(&lookahead), esc(&instrument), esc(&strategy_id), esc(&coverage), esc(&lookahead), esc(&startup), esc(&pit), esc(&detail)
        ));
    }
    html.push_str("</tbody></table></section><script>(function(){const text=document.getElementById('diagnostic-filter');const state=document.getElementById('diagnostic-lookahead-filter');const rows=[...document.querySelectorAll('#diagnostic-rows tr')];function apply(){const q=text.value.trim().toLowerCase();const s=state.value;for(const row of rows){const matchesText=(row.dataset.instrument+' '+row.dataset.strategy).toLowerCase().includes(q);row.hidden=!(matchesText&&(!s||row.dataset.lookahead===s));}}text.addEventListener('input',apply);state.addEventListener('change',apply);})();</script>");
}

pub(crate) fn render_comparison_html(report: &Value) -> String {
    let mut html = String::from(
        "<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>多市场深度运行对照</title><style>body{font:15px system-ui,sans-serif;max-width:1100px;margin:2rem auto;padding:0 1rem;color:#172033}table{border-collapse:collapse;width:100%}th,td{text-align:left;border-bottom:1px solid #dbe3ee;padding:9px}code{overflow-wrap:anywhere}.muted{color:#64748b}</style><h1>多市场深度运行对照</h1>",
    );
    html.push_str(&format!("<p class=\"muted\">相同输入指纹 <code>{}</code></p><table><thead><tr><th>排名</th><th>运行</th><th>收益 bps</th><th>最大回撤 bps</th><th>成交</th><th>结果哈希</th></tr></thead><tbody>", esc(&strval(&report["data_fingerprint"]))));
    for row in report["ranking"].as_array().into_iter().flatten() {
        html.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><code>{}</code></td></tr>", esc(&strval(&row["rank"])), esc(&strval(&row["run_id"])), esc(&strval(&row["return_bps"])), esc(&strval(&row["max_drawdown_bps"])), esc(&strval(&row["successful_fills"])), esc(&strval(&row["result_hash"]))));
    }
    html.push_str("</tbody></table><p class=\"muted\">仅比较市场顺序和输入数据指纹相同的运行；排序为收益降序、回撤升序、run_id 升序。</p>");
    render_comparison_diagnostics(report, &mut html);
    html.push_str("</html>");
    html
}

fn render_comparison_diagnostics(report: &Value, html: &mut String) {
    let rows = report
        .get("runs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|run| {
            let run_id = run.get("run_id").cloned().unwrap_or(Value::Null);
            run.get("market_strategy_diagnostics")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(move |diagnostic| (run_id.clone(), diagnostic))
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return;
    }
    html.push_str("<section id=\"comparison-diagnostics\"><h2>逐运行策略诊断对照</h2><p class=\"muted\">每份诊断均按对应 run_id、策略槽、标的、输入指纹、事件数量和结束时点复核；报告不会重新执行策略。</p><label>筛选运行、标的或策略 <input id=\"comparison-diagnostic-filter\" type=\"search\"></label> <label>未来数据状态 <select id=\"comparison-lookahead-filter\"><option value=\"\">全部</option><option value=\"potential_lookahead_detected\">检测到变化</option><option value=\"no_change_detected\">扰动后此前决策未变化</option><option value=\"causal_event_contract\">因果事件接口；未做扰动</option><option value=\"insufficient_history\">历史不足</option><option value=\"insufficient_work_budget\">工作量预算不足</option></select></label><table><thead><tr><th>运行</th><th>标的</th><th>策略</th><th>未来数据</th><th>预热</th><th>PIT</th><th>证据</th></tr></thead><tbody id=\"comparison-diagnostic-rows\">");
    for (run_id, item) in rows {
        let diagnostic = &item["report"];
        let instrument = strval(&item["instrument"]);
        let strategy_id = strval(&item["strategy_id"]);
        let lookahead = strval(
            &diagnostic
                .pointer("/lookahead/status")
                .cloned()
                .unwrap_or(Value::Null),
        );
        let startup = strval(
            &diagnostic
                .pointer("/startup/status")
                .cloned()
                .unwrap_or(Value::Null),
        );
        let pit = strval(
            &diagnostic
                .pointer("/data_visibility/status")
                .cloned()
                .unwrap_or(Value::Null),
        );
        let run_id = strval(&run_id);
        let search = format!("{run_id} {instrument} {strategy_id}");
        let detail = serde_json::to_string_pretty(diagnostic).unwrap_or_else(|_| "{}".into());
        html.push_str(&format!(
            "<tr data-search=\"{}\" data-lookahead=\"{}\"><td><code>{}</code></td><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td><details><summary>查看</summary><pre>{}</pre></details></td></tr>",
            esc(&search), esc(&lookahead), esc(&run_id), esc(&instrument), esc(&strategy_id),
            esc(&lookahead), esc(&startup), esc(&pit), esc(&detail)
        ));
    }
    html.push_str("</tbody></table></section><script>(function(){const text=document.getElementById('comparison-diagnostic-filter');const state=document.getElementById('comparison-lookahead-filter');const rows=[...document.querySelectorAll('#comparison-diagnostic-rows tr')];function apply(){const q=text.value.trim().toLowerCase();const s=state.value;for(const row of rows){row.hidden=!(row.dataset.search.toLowerCase().includes(q)&&(!s||row.dataset.lookahead===s));}}text.addEventListener('input',apply);state.addEventListener('change',apply);})();</script>");
}

fn esc(text: &str) -> String {
    crate::report_html::escape_html(text)
}
