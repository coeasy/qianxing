use crate::escape_html;
use serde_json::Value;

pub(super) const MAX_HTML_DETAIL_ROWS_PER_ACCOUNT: usize = 250;

pub(super) fn batch_row_budget(
    total: usize,
    accounts: usize,
    account_index: usize,
    per_account: usize,
) -> usize {
    let base = total / accounts;
    let remainder = total % accounts;
    per_account.min(base + usize::from(account_index < remainder))
}

pub(super) fn downsample_equity(points: &[(i64, i128)], max_points: usize) -> Vec<(i64, i128)> {
    if points.len() <= max_points || max_points < 2 {
        return points.to_vec();
    }
    (0..max_points)
        .map(|index| {
            let source = index * (points.len() - 1) / (max_points - 1);
            points[source]
        })
        .collect()
}

pub(super) fn render_html(report: &Value) -> Result<String, String> {
    let html_report = bounded_html_report(report);
    let payload = serde_json::to_string(&html_report)
        .map_err(|error| format!("编码多账户报告失败: {error}"))?
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    Ok(format!(
        r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>牵星多账户回测研究报告</title><style>body{{margin:0;background:#f5f7fa;color:#202124;font:14px system-ui,'Microsoft YaHei',sans-serif}}main{{max-width:1300px;margin:32px auto;padding:24px;background:#fff;border-radius:12px;box-shadow:0 3px 18px #0001}}h1{{margin:0 0 8px}}.muted{{color:#667085}}.cards{{display:flex;gap:12px;flex-wrap:wrap;margin:20px 0}}.card{{padding:14px 18px;border:1px solid #e5e7eb;border-radius:8px;min-width:140px}}.toolbar{{display:flex;gap:12px;align-items:end;flex-wrap:wrap;margin:20px 0}}label{{display:grid;gap:6px;color:#667085}}input,select{{padding:8px;border:1px solid #ccd2da;border-radius:6px;font:inherit}}.scroll{{overflow:auto;max-height:65vh}}table{{border-collapse:collapse;width:100%;white-space:nowrap}}th,td{{padding:10px 12px;border-bottom:1px solid #e5e7eb;text-align:right}}th{{position:sticky;top:0;background:#f3f5f8}}th:first-child,td:first-child,th:nth-child(2),td:nth-child(2){{text-align:left}}code{{font-family:ui-monospace,Consolas,monospace}}.warning{{padding:12px;background:#fff7ed;color:#9a3412;border-radius:6px}}.empty{{padding:20px;text-align:center;color:#667085}}</style></head><body><main><h1>多账户回测研究报告</h1><p class="muted">批次 <code>{}</code> · 结算币种 {} · 规格指纹 <code>{}</code></p><p class="warning">账户之间的现金、持仓、风险和收益彼此独立；本报告不进行跨账户净额、资金池合并或组合风险推断。加权收益仅按批次声明的成功账户本金权重展示。</p><section class="cards"><div class="card">账户总数<br><b>{}</b></div><div class="card">成功 / 失败<br><b>{} / {}</b></div><div class="card">本金加权收益<br><b>{}</b></div><div class="card">成功账户成交<br><b>{}</b></div></section><section class="toolbar"><label>搜索账户/标的/策略<input id="search" type="search"></label><label>状态<select id="status"><option value="">全部</option><option value="succeeded">成功</option><option value="failed">失败</option></select></label><label>收益率至少 (%)<input id="minimum" type="number" step="0.01"></label><span id="count" aria-live="polite"></span></section><div class="scroll"><table><thead><tr><th>账户</th><th>状态</th><th>标的</th><th>策略</th><th>收益率</th><th>最大回撤</th><th>成交数</th><th>权益点</th><th>Run ID</th><th>结果哈希</th></tr></thead><tbody id="rows"></tbody></table></div><p id="empty" class="empty" hidden>没有符合条件的账户。</p><footer class="muted">离线报告 · 不连接交易所 · 结果来自经 RunRecord 输入身份与工件哈希校验的账户运行</footer></main><script>const data={payload},rows=document.getElementById('rows'),search=document.getElementById('search'),status=document.getElementById('status'),minimum=document.getElementById('minimum'),count=document.getElementById('count'),empty=document.getElementById('empty');function esc(value){{return String(value??'absent').replace(/[&<>"']/g,char=>({{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}}[char]));}}function pct(value){{return value==null?'absent':(Number(value)/100).toFixed(2)+'%';}}function render(){{const query=search.value.trim().toLocaleLowerCase(),minimumBps=minimum.value===''?-Infinity:Number(minimum.value)*100;let visible=0;rows.innerHTML='';for(const item of data.accounts){{const text=[item.account_id,item.instrument,item.strategy_id].join(' ').toLocaleLowerCase(),matches=(!query||text.includes(query))&&(!status.value||item.status===status.value)&&(item.return_bps==null||Number(item.return_bps)>=minimumBps);if(!matches)continue;visible++;const cells=[item.account_id,item.status,item.instrument,item.strategy_id,pct(item.return_bps),pct(item.max_drawdown_bps),item.fills,item.equity_points,item.run_id,item.result_hash];const tr=document.createElement('tr');tr.innerHTML=cells.map(value=>'<td>'+esc(value)+'</td>').join('');if(item.status==='failed')tr.title=item.error||'运行失败';rows.append(tr);}}count.textContent='显示 '+visible+' / '+data.accounts.length+' 个账户';empty.hidden=visible!==0;}}for(const control of [search,status,minimum])control.addEventListener('input',render);status.addEventListener('change',render);render();</script></body></html>"#,
        escape_html(report["batch_id"].as_str().unwrap_or("")),
        escape_html(report["settlement_currency"].as_str().unwrap_or("")),
        escape_html(report["spec_fingerprint"].as_str().unwrap_or("")),
        report
            .pointer("/summary/accounts_total")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        report
            .pointer("/summary/accounts_succeeded")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        report
            .pointer("/summary/accounts_failed")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        report
            .pointer("/summary/weighted_return_bps")
            .map(|value| format!("{} bps", value))
            .unwrap_or_else(|| "absent".into()),
        report
            .pointer("/summary/successful_fills")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
    ))
}

pub(super) fn bounded_html_report(report: &Value) -> Value {
    let mut html_report = report.clone();
    let Some(accounts) = html_report
        .get_mut("accounts")
        .and_then(Value::as_array_mut)
    else {
        return html_report;
    };
    for account in accounts {
        let Some(account) = account.as_object_mut() else {
            continue;
        };
        for field in ["fill_rows", "event_rows"] {
            let Some(rows) = account.get_mut(field).and_then(Value::as_array_mut) else {
                continue;
            };
            if rows.len() > MAX_HTML_DETAIL_ROWS_PER_ACCOUNT {
                let keep_from = rows.len() - MAX_HTML_DETAIL_ROWS_PER_ACCOUNT;
                *rows = rows.split_off(keep_from);
            }
        }
        if let Some(events) = account.get("event_rows").and_then(Value::as_array) {
            let visible_ids = events
                .iter()
                .filter_map(|event| event.get("event_id").and_then(Value::as_str))
                .map(str::to_owned)
                .collect::<std::collections::BTreeSet<_>>();
            if let Some(fills) = account.get_mut("fill_rows").and_then(Value::as_array_mut) {
                for fill in fills {
                    if fill
                        .get("event_id")
                        .and_then(Value::as_str)
                        .is_some_and(|event_id| !visible_ids.contains(event_id))
                    {
                        if let Some(fill) = fill.as_object_mut() {
                            fill.remove("event_id");
                            fill.remove("event_seq");
                        }
                    }
                }
            }
        }
    }
    html_report
}
