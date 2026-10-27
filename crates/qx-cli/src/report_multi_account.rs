//! Account-isolated research report for persisted multi-account Bar results.

use crate::escape_html;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_HTML_DETAIL_ROWS_PER_ACCOUNT: usize = 250;
const MAX_REPORT_FILL_ROWS_PER_ACCOUNT: usize = 5_000;
const MAX_REPORT_FILL_ROWS_TOTAL: usize = 50_000;
const MAX_REPORT_EVENT_ROWS_PER_ACCOUNT: usize = 5_000;
const MAX_REPORT_EVENT_ROWS_TOTAL: usize = 50_000;
const MAX_MULTI_ACCOUNT_DIAGNOSTICS_BYTES: u64 = 1024 * 1024;

// Keep CLI report options explicit; the caller owns their parsing and validation.
#[allow(clippy::too_many_arguments)]
pub(crate) fn maybe_run(
    path: &Path,
    as_json: bool,
    html: bool,
    evidence: bool,
    output: Option<&Path>,
    against: &[PathBuf],
    xlsx: Option<&Path>,
    has_benchmark: bool,
    strategy_diagnostics: Option<&Path>,
) -> Result<bool, String> {
    let payload = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(false),
    };
    let Ok(value) = serde_json::from_slice::<Value>(&payload) else {
        return Ok(false);
    };
    if value.get("batch_id").and_then(Value::as_str).is_none()
        || value.get("runs").and_then(Value::as_array).is_none()
        || value.get("summary").and_then(Value::as_object).is_none()
    {
        return Ok(false);
    }
    if !against.is_empty() || has_benchmark {
        return Err("多账户报告暂不支持 --against 或基准参数；请对账户运行分别生成对照报告".into());
    }
    if output.is_some() && !html {
        return Err("--output 只能与 --html 一起使用".into());
    }
    if value.get("result_kind").and_then(Value::as_str) == Some("multi_account_depth")
        || value
            .get("runs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|run| {
                run.pointer("/outcome/tier")
                    .and_then(Value::as_str)
                    .is_some()
            })
    {
        return maybe_run_depth_report(
            path,
            &value,
            as_json,
            html,
            evidence,
            output,
            against,
            xlsx,
            has_benchmark,
            strategy_diagnostics,
        );
    }
    let result: qx_app::MultiAccountBacktestResult = serde_json::from_value(value)
        .map_err(|error| format!("多账户回测结果契约无效: {error}"))?;
    let outcome_value =
        serde_json::to_value(&result).map_err(|error| format!("多账户结果编码失败: {error}"))?;
    let verified_batch = qx_app::read_multi_account_report_v1(
        &outcome_value,
        &qx_app::RunContext::new(
            qx_app::CallerCapability::Research,
            format!("report-{}", result.batch_id),
        ),
    )
    .map_err(|error| format!("多账户共享报告复核失败: {error}"))?;
    let verified_accounts = verified_batch
        .accounts
        .iter()
        .map(|account| (account.account_id.as_str(), account))
        .collect::<BTreeMap<_, _>>();
    let diagnostics = strategy_diagnostics
        .map(read_multi_account_diagnostics)
        .transpose()?;
    if let Some(diagnostics) = &diagnostics {
        let expected = result
            .runs
            .iter()
            .filter(|run| run.outcome.is_some())
            .map(|run| run.account_id.as_str())
            .collect::<BTreeSet<_>>();
        let supplied = diagnostics
            .reports
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if expected != supplied {
            return Err(
                "多账户策略诊断必须为每个成功账户各提供且仅提供一份，且不得绑定失败账户".into(),
            );
        }
    }
    if let Some(xlsx_path) = xlsx {
        crate::report_xlsx::validate_xlsx_path(xlsx_path)?;
        validate_report_output_target(xlsx_path, path, strategy_diagnostics, &result, evidence)?;
        if let Some(html_path) = output.filter(|_| html) {
            if crate::report_output_paths::same_output_target(xlsx_path, html_path) {
                return Err("HTML 与 XLSX 报告不能写入同一路径".into());
            }
        }
    }
    let accounts_total = result.runs.len();
    let fill_row_limit_max =
        MAX_REPORT_FILL_ROWS_PER_ACCOUNT.min(MAX_REPORT_FILL_ROWS_TOTAL.div_ceil(accounts_total));
    let event_row_limit_max =
        MAX_REPORT_EVENT_ROWS_PER_ACCOUNT.min(MAX_REPORT_EVENT_ROWS_TOTAL.div_ceil(accounts_total));

    let mut accounts = Vec::with_capacity(result.runs.len());
    for (account_index, run) in result.runs.iter().enumerate() {
        let Some(outcome) = &run.outcome else {
            accounts.push(json!({
                "account_id": &run.account_id,
                "status": "failed",
                "error": &run.error_json,
                "run_id": null,
                "instrument": null,
                "result_hash": null,
                "data_fingerprint": null,
                "return_bps": null,
                "max_drawdown_bps": null,
                "fills": null,
                "summary_path": null,
            }));
            continue;
        };
        let fill_row_limit = batch_row_budget(
            MAX_REPORT_FILL_ROWS_TOTAL,
            accounts_total,
            account_index,
            MAX_REPORT_FILL_ROWS_PER_ACCOUNT,
        );
        let event_row_limit = batch_row_budget(
            MAX_REPORT_EVENT_ROWS_TOTAL,
            accounts_total,
            account_index,
            MAX_REPORT_EVENT_ROWS_PER_ACCOUNT,
        );
        let summary_path = PathBuf::from(&outcome.artifacts.summary);
        let verified_account = verified_accounts
            .get(run.account_id.as_str())
            .copied()
            .ok_or_else(|| format!("多账户共享报告缺少账户 {}", run.account_id))?;
        if verified_account.status != qx_app::MultiAccountResearchStatusV1::Succeeded {
            return Err(format!("账户 {} 的共享报告状态不匹配", run.account_id));
        }
        let verified_report = verified_account
            .report
            .as_ref()
            .ok_or_else(|| format!("账户 {} 缺少共享核心报告", run.account_id))?;
        let summary = verified_report.summary.clone();
        let verification = &verified_report.verification;
        if !verification.verified {
            return Err(format!(
                "账户 {} 的运行工件复核未通过: {}",
                run.account_id,
                verification.mismatches.join("; ")
            ));
        }
        let run_id = summary
            .pointer("/run_id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("账户 {} 的 summary 缺少 run_id", run.account_id))?;
        let instrument = summary
            .pointer("/instrument")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("账户 {} 的 summary 缺少 instrument", run.account_id))?;
        let result_hash = summary
            .pointer("/result_hash")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("账户 {} 的 summary 缺少 result_hash", run.account_id))?;
        if run_id != outcome.run_id
            || instrument != outcome.instrument
            || result_hash != outcome.result_hash
            || verification.data_fingerprint != outcome.data_fingerprint
            || outcome.account_id.as_deref() != Some(run.account_id.as_str())
        {
            return Err(format!(
                "账户 {} 的结果身份与已验证 summary 不一致",
                run.account_id
            ));
        }
        let declared_input = qx_app::artifact_store::recompute_declared_backtest_input(&summary)?;
        if (evidence || diagnostics.is_some()) && declared_input.is_none() {
            return Err(format!(
                "账户 {} 的 summary 未声明可复核输入，不能绑定诊断或生成 EvidenceBundle",
                run.account_id
            ));
        }
        let bound_diagnostics = diagnostics
            .as_ref()
            .map(|attachment| {
                let diagnostic = attachment
                    .reports
                    .get(&run.account_id)
                    .ok_or_else(|| format!("账户 {} 缺少策略诊断", run.account_id))?;
                let strategy_id = summary
                    .pointer("/strategy/strategy_id")
                    .or_else(|| summary.pointer("/strategy_id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        format!("账户 {} 的 summary 缺少 strategy_id", run.account_id)
                    })?;
                crate::report_command::validate_strategy_diagnostics_identity(
                    diagnostic,
                    strategy_id,
                    instrument,
                    &declared_input
                        .as_ref()
                        .ok_or("策略诊断缺少重算后的数据输入身份")?
                        .fingerprint,
                )?;
                Ok::<_, String>(json!({
                    "identity_bound": true,
                    "account_id": &run.account_id,
                    "report": diagnostic,
                }))
            })
            .transpose()?;
        let account_evidence_path =
            if let Some(declared_input) = declared_input.as_ref().filter(|_| evidence) {
                let diagnostic_report = diagnostics
                    .as_ref()
                    .and_then(|attachment| attachment.reports.get(&run.account_id));
                let diagnostic_snapshot = diagnostic_report
                    .map(|report| {
                        write_account_diagnostic_snapshot(&summary_path, &run.account_id, report)
                    })
                    .transpose()?;
                let evidence_result = crate::run_evidence::write_run_evidence(
                    &summary_path,
                    &summary,
                    None,
                    diagnostic_snapshot
                        .as_ref()
                        .zip(diagnostic_report)
                        .map(|((path, _), report)| (path.as_path(), report)),
                );
                match evidence_result {
                    Ok(path) => Some(path),
                    Err(error) => {
                        if let Some((snapshot, created)) = diagnostic_snapshot {
                            if created {
                                let _ = std::fs::remove_file(snapshot);
                            }
                        }
                        return Err(format!(
                            "账户 {} 生成运行 EvidenceBundle 失败（输入 {}）: {error}",
                            run.account_id, declared_input.path
                        ));
                    }
                }
            } else {
                None
            };
        let (mut fill_rows, fill_rows_total) =
            read_account_fills(&outcome.artifacts.fills, outcome.fills, fill_row_limit)?;
        let (event_rows, event_trace_path, event_rows_total) = if let Some(trace_path) =
            &outcome.artifacts.event_trace
        {
            let trace_path = PathBuf::from(trace_path);
            let events = crate::report_html::read_event_trace(&trace_path, &summary)?;
            let mut order_events = std::collections::BTreeMap::new();
            let event_rows = events
                .iter()
                .skip(events.len().saturating_sub(event_row_limit))
                .map(|event| {
                    let (kind, order_id) = crate::report_html::event_kind_identity(&event.kind);
                    if let Some(order_id) = order_id {
                        order_events.entry(order_id).or_insert(event.seq);
                    }
                    json!({
                        "event_id": format!("account-{}-event-{}", run.account_id, event.seq),
                        "account_id": &run.account_id,
                        "seq": event.seq,
                        "timestamp_ms": event.ts,
                        "priority": event.prio,
                        "source_seq": event.source_seq,
                        "kind": kind,
                        "order_id": order_id,
                        "payload": serde_json::to_string(&event.kind).unwrap_or_else(|_| "{}".into()),
                    })
                })
                .collect::<Vec<_>>();
            for fill in &mut fill_rows {
                let Some(order_id) = fill
                    .get("order_id")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<u64>().ok())
                else {
                    continue;
                };
                if let Some(seq) = order_events.get(&order_id) {
                    fill["event_seq"] = json!(seq);
                    fill["event_id"] = json!(format!("account-{}-event-{}", run.account_id, seq));
                }
            }
            let event_rows_total = events.len();
            (event_rows, Some(trace_path), event_rows_total)
        } else {
            (Vec::new(), None, 0)
        };
        let equity_series = if Path::new(&outcome.artifacts.equity).is_file() {
            let values = crate::report_html::read_equity_csv(Path::new(&outcome.artifacts.equity))?;
            downsample_equity(&values, 2_000)
                .into_iter()
                .map(|(timestamp_ms, equity_raw)| {
                    json!({"timestamp_ms": timestamp_ms, "equity_raw": equity_raw.to_string()})
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        accounts.push(json!({
            "account_id": &run.account_id,
            "status": "succeeded",
            "run_id": &outcome.run_id,
            "instrument": &outcome.instrument,
            "strategy_id": summary.pointer("/strategy/strategy_id").or_else(|| summary.pointer("/strategy_id")),
            "result_hash": &outcome.result_hash,
            "data_fingerprint": declared_input
                .as_ref()
                .map(|input| input.fingerprint.as_str())
                .unwrap_or(&verification.data_fingerprint),
            "run_data_fingerprint": &verification.data_fingerprint,
            "strategy_bias_diagnostics": bound_diagnostics,
            "return_bps": outcome.return_bps,
            "max_drawdown_bps": outcome.max_drawdown_bps,
            "fills": outcome.fills,
            "fill_rows_total": fill_rows_total,
            "fill_rows": fill_rows,
            "event_rows": event_rows,
            "event_rows_total": event_rows_total,
            "event_trace_path": event_trace_path,
            "equity_points": outcome.equity_points,
            "equity_series": equity_series,
            "summary_path": &summary_path,
            "run_evidence_path": account_evidence_path,
            "equity_path": &outcome.artifacts.equity,
            "fills_path": &outcome.artifacts.fills,
        }));
    }
    let report = json!({
        "schema_version": 1,
        "report_kind": "multi_account_backtest",
        "source_path": path,
        "batch_id": &result.batch_id,
        "settlement_currency": &result.settlement_currency,
        "spec_fingerprint": &result.spec_fingerprint,
        "summary": &verified_batch.summary,
        "account_semantics": "accounts_are_independent; cash_positions_risk_and_returns_are_not_netted",
        "verification_scope": "run_manifest_summary_equity_and_fills_cross_checked; source_input_recomputed_and_cross_checked_when_declared",
        "strategy_diagnostics_attachment": diagnostics.as_ref().map(|attachment| json!({
            "kind": "qianxing-multi-account-strategy-bias-diagnostics",
            "input_path": &attachment.path,
            "input_sha256": &attachment.sha256,
            "account_count": attachment.reports.len(),
        })),
        "detail_limits": {
            "fill_rows_per_account_max": fill_row_limit_max,
            "fill_rows_batch_max": MAX_REPORT_FILL_ROWS_TOTAL,
            "event_rows_per_account_max": event_row_limit_max,
            "event_rows_batch_max": MAX_REPORT_EVENT_ROWS_TOTAL,
            "sampling": "most_recent_rows",
        },
        "accounts": accounts,
    });
    let html_path = if html {
        let target = output.map(Path::to_path_buf).unwrap_or_else(|| {
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("multi-account");
            path.with_file_name(format!("{name}.report.html"))
        });
        if !target
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"))
        {
            return Err(format!(
                "HTML 报告输出路径必须以 .html 或 .htm 结尾: {}",
                target.display()
            ));
        }
        validate_report_output_target(&target, path, strategy_diagnostics, &result, evidence)?;
        if let Some(parent) = target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建报告目录失败: {error}"))?;
        }
        let rendered = render_html(&report)?
            .replace(
                "结果来自经 RunRecord 输入身份与工件哈希校验的账户运行",
                "结果来自经 RunManifest、summary、权益与成交产物交叉复核的账户运行；本批次报告不重读原始 BarFrame 字节",
            )
            .replace("<th>结果哈希</th>", "<th>结果哈希</th><th>成交明细</th>")
            .replace(
                "rows.append(tr);",
                "const fillCell=document.createElement('td'),details=document.createElement('details'),summary=document.createElement('summary'),fills=item.fill_rows||[];summary.textContent='显示 '+fills.length+' / '+(item.fill_rows_total??fills.length)+' 笔成交（方向字段缺席）';details.append(summary);if(fills.length){{const fillTable=document.createElement('table'),head=document.createElement('tr');for(const title of ['时间','订单','数量 raw','数量 exact','价格 raw','价格 exact','费用 raw','精确费用 JSON','策略','事件链']){{const th=document.createElement('th');th.textContent=title;head.append(th);}}fillTable.append(head);for(const fill of fills){{const line=document.createElement('tr');for(const value of [new Date(fill.timestamp_ms).toLocaleString(),fill.order_id,fill.qty_raw,fill.qty_exact||'absent',fill.price_raw,fill.price_exact||'absent',fill.fee_raw,fill.exact_fees_json||'absent',fill.strategy_id||'absent']){{const cell=document.createElement('td');cell.textContent=value;line.append(cell);}}const eventCell=document.createElement('td');if(fill.event_id){{const link=document.createElement('a');link.href='#'+fill.event_id;link.textContent='事件 '+fill.event_seq;eventCell.append(link);}}else{{eventCell.textContent='absent';}}line.append(eventCell);fillTable.append(line);}}details.append(fillTable);}}fillCell.append(details);tr.append(fillCell);rows.append(tr);",
            )
            .replace(
                "<section class=\"cards\">",
                "<section class=\"equity-panel\"><h2>账户权益曲线</h2><p>各账户独立归一化到首个权益点；最多显示 2,000 个抽样点。缺失或无有效权益数据的账户不绘制。</p><div id=\"equity-legend\" class=\"equity-legend\"></div><div class=\"chart-wrap\"><canvas id=\"equity-chart\" aria-label=\"多账户独立归一化权益曲线\" role=\"img\"></canvas><div id=\"equity-tip\" hidden></div></div></section><section class=\"cards\">",
            )
            .replace(
                "</style>",
                ".equity-panel{margin:20px 0;padding:16px;border:1px solid #e5e7eb;border-radius:8px}.equity-panel p{color:#667085}.equity-legend{display:flex;gap:12px;flex-wrap:wrap;margin:12px 0}.equity-legend label{display:flex;align-items:center;gap:6px}.equity-legend span{width:12px;height:12px;border-radius:50%}.chart-wrap{position:relative;height:320px}#equity-chart{width:100%;height:320px;display:block}#equity-tip{position:absolute;padding:8px;background:#202124;color:white;border-radius:6px;font-size:12px;pointer-events:none;z-index:2}</style>",
            )
            .replace(
                "</script></body>",
                r#"</script><script>const chart=document.getElementById('equity-chart'),ctx=chart.getContext('2d'),legend=document.getElementById('equity-legend'),tip=document.getElementById('equity-tip'),colors=['#2563eb','#dc2626','#059669','#d97706','#7c3aed','#0891b2','#be123c','#4d7c0f'];for(let i=0;i<data.accounts.length;i++){const item=data.accounts[i],points=item.equity_series||[];if(points.length<2)continue;const label=document.createElement('label'),toggle=document.createElement('input'),swatch=document.createElement('span');toggle.type='checkbox';toggle.checked=true;toggle.dataset.index=i;toggle.addEventListener('change',draw);swatch.style.background=colors[i%colors.length];label.append(toggle,swatch,document.createTextNode(item.account_id));legend.append(label);}function draw(){const rect=chart.getBoundingClientRect(),scale=window.devicePixelRatio||1;chart.width=Math.max(1,Math.floor(rect.width*scale));chart.height=Math.floor(320*scale);ctx.setTransform(scale,0,0,scale,0,0);ctx.clearRect(0,0,rect.width,320);const selected=[...legend.querySelectorAll('input:checked')].map(input=>data.accounts[Number(input.dataset.index)]).filter(item=>item&&item.equity_series.length>1);if(!selected.length){ctx.fillStyle='#667085';ctx.fillText('没有可绘制的权益曲线',18,32);return;}const times=selected.flatMap(item=>item.equity_series.map(p=>p.timestamp_ms)),values=selected.flatMap(item=>{const start=BigInt(item.equity_series[0].equity_raw);return item.equity_series.map(p=>Number((BigInt(p.equity_raw)-start)*1000000n/start)/10000)}),minT=times.reduce((a,b)=>Math.min(a,b),Infinity),maxT=times.reduce((a,b)=>Math.max(a,b),-Infinity),minY=values.reduce((a,b)=>Math.min(a,b),0),maxY=values.reduce((a,b)=>Math.max(a,b),0),pad={left:58,right:16,top:18,bottom:30},w=rect.width-pad.left-pad.right,h=320-pad.top-pad.bottom,x=t=>pad.left+(t-minT)/Math.max(1,maxT-minT)*w,y=v=>pad.top+(maxY-v)/Math.max(.0001,maxY-minY)*h;ctx.strokeStyle='#d0d5dd';ctx.beginPath();ctx.moveTo(pad.left,pad.top);ctx.lineTo(pad.left,320-pad.bottom);ctx.lineTo(rect.width-pad.right,320-pad.bottom);ctx.stroke();for(const item of selected){const start=BigInt(item.equity_series[0].equity_raw);ctx.strokeStyle=colors[data.accounts.indexOf(item)%colors.length];ctx.lineWidth=1.8;ctx.beginPath();item.equity_series.forEach((p,i)=>{const value=Number((BigInt(p.equity_raw)-start)*1000000n/start)/10000,px=x(p.timestamp_ms),py=y(value);if(i)ctx.lineTo(px,py);else ctx.moveTo(px,py)});ctx.stroke()}chart._state={selected,minT,maxT,pad,x};}chart.addEventListener('pointermove',event=>{const s=chart._state;if(!s)return;const rect=chart.getBoundingClientRect(),px=event.clientX-rect.left,t=s.minT+(px-s.pad.left)/(rect.width-s.pad.left-s.pad.right)*(s.maxT-s.minT),lines=s.selected.map(item=>{const point=item.equity_series.reduce((best,p)=>Math.abs(p.timestamp_ms-t)<Math.abs(best.timestamp_ms-t)?p:best,item.equity_series[0]),start=BigInt(item.equity_series[0].equity_raw),pct=Number((BigInt(point.equity_raw)-start)*1000000n/start)/10000;return item.account_id+': '+pct.toFixed(2)+'%'});tip.textContent=new Date(t).toLocaleString()+' · '+lines.join(' · ');tip.hidden=false;tip.style.left=Math.max(0,px+12)+'px';tip.style.top='8px'});chart.addEventListener('pointerleave',()=>tip.hidden=true);window.addEventListener('resize',draw);draw();</script></body>"#,
            );
        let mut rendered = rendered.replace(
            "</main><script>",
            "<section class=\"event-panel\"><h2>撮合与账簿事件追溯</h2><p>事件流须与账户摘要中的 replay digest 和事件数一致才会显示。浏览器每个账户最多显示最近 250 笔成交和 250 条事件；完整 CSV、JSONL 与总数仍保留在报告数据中。成交明细中的事件链接按订单 ID 定位当前显示的事件。</p><label>筛选账户/事件/时间/订单 <input id=\"event-filter\" type=\"search\" placeholder=\"账户、订单、事件类型或序列号\"></label><div class=\"scroll\"><table><thead><tr><th>账户</th><th>序列</th><th>时间</th><th>优先级</th><th>来源序列</th><th>事件</th><th>订单</th><th>事实内容</th></tr></thead><tbody id=\"event-rows\"></tbody></table></div></section></main><script>",
        );
        rendered = rendered.replace(
            "</body>",
            "<script>const eventRows=data.accounts.flatMap(item=>item.event_rows||[]),eventBody=document.getElementById('event-rows'),eventFilter=document.getElementById('event-filter');function drawEvents(){const query=eventFilter.value.trim().toLocaleLowerCase();eventBody.replaceChildren();for(const event of eventRows){const search=[event.account_id,event.seq,event.timestamp_ms,event.kind,event.order_id].join(' ').toLocaleLowerCase();if(query&&!search.includes(query))continue;const row=document.createElement('tr');row.id=event.event_id;for(const value of [event.account_id,event.seq,new Date(event.timestamp_ms).toLocaleString(),event.priority,event.source_seq,event.kind,event.order_id??'absent',event.payload]){const cell=document.createElement('td');cell.textContent=value??'absent';row.append(cell);}eventBody.append(row);}}eventFilter.addEventListener('input',drawEvents);drawEvents();</script></body>",
        );
        rendered = rendered.replace(
            "<section class=\"event-panel\"><h2>撮合与账簿事件追溯</h2>",
            "<section id=\"strategy-diagnostics-panel\" class=\"event-panel\" hidden><h2>策略未来数据与预热诊断</h2><p>诊断由调用方预先生成；本报告只校验策略、标的与复核后的输入指纹并展示结果。未发现变化不构成不存在未来数据路径的证明。</p><div class=\"toolbar\"><label>搜索账户/策略<input id=\"strategy-diagnostic-search\" type=\"search\"></label><label>未来数据状态<select id=\"strategy-diagnostic-status\"><option value=\"\">全部</option><option>potential_lookahead_detected</option><option>no_change_detected</option><option>insufficient_history</option><option>insufficient_work_budget</option><option>causal_event_contract</option></select></label></div><div class=\"scroll\"><table><thead><tr><th>账户</th><th>策略</th><th>标的</th><th>未来数据</th><th>预热</th><th>PIT</th><th>完整证据</th></tr></thead><tbody id=\"strategy-diagnostic-rows\"></tbody></table></div></section><section class=\"event-panel\"><h2>撮合与账簿事件追溯</h2>",
        );
        rendered = rendered.replace(
            "</body>",
            "<script>const diagnosticPanel=document.getElementById('strategy-diagnostics-panel'),diagnosticBody=document.getElementById('strategy-diagnostic-rows'),diagnosticSearch=document.getElementById('strategy-diagnostic-search'),diagnosticStatus=document.getElementById('strategy-diagnostic-status'),diagnosticAccounts=(data.accounts||[]).filter(item=>item.strategy_bias_diagnostics?.report);if(diagnosticAccounts.length){diagnosticPanel.hidden=false;for(const item of diagnosticAccounts){const d=item.strategy_bias_diagnostics.report,row=document.createElement('tr');row.dataset.search=[item.account_id,d.strategy_id].join(' ').toLowerCase();row.dataset.lookahead=d.lookahead?.status||'';for(const value of [item.account_id,d.strategy_id,d.instrument,d.lookahead?.status,d.startup?.status,d.data_visibility?.status||'visibility_unknown']){const cell=document.createElement('td');cell.textContent=value??'absent';row.append(cell);}const detail=document.createElement('td'),details=document.createElement('details'),summary=document.createElement('summary'),pre=document.createElement('pre');summary.textContent='查看 JSON';pre.textContent=JSON.stringify(d,null,2);details.append(summary,pre);detail.append(details);row.append(detail);diagnosticBody.append(row);}function filterStrategyDiagnostics(){const query=diagnosticSearch.value.trim().toLowerCase(),status=diagnosticStatus.value;for(const row of diagnosticBody.rows)row.hidden=!(row.dataset.search.includes(query)&&(!status||row.dataset.lookahead===status));}diagnosticSearch.addEventListener('input',filterStrategyDiagnostics);diagnosticStatus.addEventListener('change',filterStrategyDiagnostics);}</script></body>",
        );
        qx_app::artifact_store::sink::publish(
            &target,
            &rendered,
            qx_app::artifact_store::sink::PublishMode::Overwrite,
        )
        .map_err(|error| format!("发布多账户 HTML 报告失败: {error}"))?;
        Some(target)
    } else {
        None
    };
    let xlsx_path = xlsx
        .map(|target| crate::report_xlsx::write_multi_account_report_xlsx(&report, target))
        .transpose()?;
    if as_json {
        let mut value = json!({"schema_version":1,"report":report});
        if let Some(path) = html_path {
            value["generated_artifacts"]["html"] = json!(path);
        }
        if let Some(path) = xlsx_path {
            value["generated_artifacts"]["xlsx"] = json!(path);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?
        );
    } else if let Some(path) = html_path {
        println!("[Report] html={}", path.display());
        if let Some(xlsx_path) = &xlsx_path {
            println!("[Report] xlsx={}", xlsx_path.display());
        }
        println!(
            "[Report] batch_id={} accounts={} complete={}",
            result.batch_id,
            result.runs.len(),
            result.summary.complete
        );
    } else {
        if let Some(path) = &xlsx_path {
            println!("[Report] xlsx={}", path.display());
        }
        println!(
            "[Report] batch_id={} accounts={} complete={}",
            result.batch_id,
            result.runs.len(),
            result.summary.complete
        );
        for account in report["accounts"].as_array().into_iter().flatten() {
            println!(
                "[Report] account={} status={} instrument={} return_bps={}",
                account["account_id"].as_str().unwrap_or("unknown"),
                account["status"].as_str().unwrap_or("unknown"),
                account["instrument"].as_str().unwrap_or("absent"),
                account["return_bps"]
            );
        }
    }
    Ok(true)
}

fn maybe_run_depth_report(
    path: &Path,
    value: &Value,
    as_json: bool,
    html: bool,
    evidence: bool,
    output: Option<&Path>,
    against: &[PathBuf],
    xlsx: Option<&Path>,
    has_benchmark: bool,
    strategy_diagnostics: Option<&Path>,
) -> Result<bool, String> {
    if !against.is_empty() || has_benchmark {
        return Err(
            "多账户深度报告暂不支持 --against 或基准参数；请对账户运行分别生成对照报告".into(),
        );
    }
    if output.is_some() && !html {
        return Err("--output 只能与 --html 一起使用".into());
    }
    if output.is_some_and(|html_path| {
        xlsx.is_some_and(|xlsx_path| {
            crate::report_output_paths::same_output_target(html_path, xlsx_path)
        })
    }) {
        return Err("HTML 与 XLSX 报告必须使用不同输出路径".into());
    }
    let result: qx_app::MultiAccountDepthBacktestResult = serde_json::from_value(value.clone())
        .map_err(|error| format!("多账户深度回测结果契约无效: {error}"))?;
    let verified_batch = qx_app::read_multi_account_report_v1(
        value,
        &qx_app::RunContext::new(
            qx_app::CallerCapability::Research,
            format!("report-{}", result.batch_id),
        ),
    )
    .map_err(|error| format!("多账户深度共享报告复核失败: {error}"))?;
    let verified_accounts = verified_batch
        .accounts
        .iter()
        .map(|account| (account.account_id.as_str(), account))
        .collect::<BTreeMap<_, _>>();
    let diagnostics = strategy_diagnostics
        .map(read_multi_account_diagnostics)
        .transpose()?;
    if let Some(diagnostics) = &diagnostics {
        let expected = result
            .runs
            .iter()
            .filter(|run| run.outcome.is_some())
            .map(|run| run.account_id.as_str())
            .collect::<BTreeSet<_>>();
        let supplied = diagnostics
            .reports
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if expected != supplied {
            return Err(
                "多账户深度策略诊断必须为每个成功账户各提供且仅提供一份，且不得绑定失败账户".into(),
            );
        }
    }
    if let Some(xlsx_path) = xlsx {
        crate::report_xlsx::validate_xlsx_path(xlsx_path)?;
        validate_depth_report_output_target(
            xlsx_path,
            path,
            strategy_diagnostics,
            &result,
            evidence,
        )?;
    }
    let html_target = if html {
        let target = output.map(Path::to_path_buf).unwrap_or_else(|| {
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("multi-account-depth");
            path.with_file_name(format!("{name}.depth.report.html"))
        });
        if !target
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm")
            })
        {
            return Err(format!(
                "HTML 报告输出路径必须以 .html 或 .htm 结尾: {}",
                target.display()
            ));
        }
        validate_depth_report_output_target(
            &target,
            path,
            strategy_diagnostics,
            &result,
            evidence,
        )?;
        Some(target)
    } else {
        None
    };

    let accounts_total = result.runs.len();
    let fill_row_limit_max =
        MAX_REPORT_FILL_ROWS_PER_ACCOUNT.min(MAX_REPORT_FILL_ROWS_TOTAL.div_ceil(accounts_total));
    let event_row_limit_max =
        MAX_REPORT_EVENT_ROWS_PER_ACCOUNT.min(MAX_REPORT_EVENT_ROWS_TOTAL.div_ceil(accounts_total));
    let mut accounts = Vec::with_capacity(accounts_total);
    for (account_index, run) in result.runs.iter().enumerate() {
        let Some(outcome) = run.outcome.as_ref() else {
            accounts.push(json!({
                "account_id": &run.account_id,
                "status": "failed",
                "error": &run.error_json,
                "run_id": null,
                "instrument": null,
                "tier": null,
                "result_hash": null,
                "data_fingerprint": null,
                "return_bps": null,
                "max_drawdown_bps": null,
                "fills": null,
                "equity_points": null,
                "snapshots": null,
                "fill_rows": [],
                "fill_rows_total": 0,
                "fill_rows_truncated": false,
                "event_rows": [],
                "event_rows_total": 0,
                "event_rows_truncated": false,
                "summary_path": null,
            }));
            continue;
        };
        let fill_row_limit = batch_row_budget(
            MAX_REPORT_FILL_ROWS_TOTAL,
            accounts_total,
            account_index,
            MAX_REPORT_FILL_ROWS_PER_ACCOUNT,
        );
        let event_row_limit = batch_row_budget(
            MAX_REPORT_EVENT_ROWS_TOTAL,
            accounts_total,
            account_index,
            MAX_REPORT_EVENT_ROWS_PER_ACCOUNT,
        );
        let verified_account = verified_accounts
            .get(run.account_id.as_str())
            .copied()
            .ok_or_else(|| format!("多账户深度共享报告缺少账户 {}", run.account_id))?;
        if verified_account.status != qx_app::MultiAccountResearchStatusV1::Succeeded {
            return Err(format!("账户 {} 的共享报告状态不匹配", run.account_id));
        }
        let verified_report = verified_account
            .report
            .as_ref()
            .ok_or_else(|| format!("账户 {} 缺少共享核心报告", run.account_id))?;
        let summary = verified_report.summary.clone();
        let verification = &verified_report.verification;
        if !verification.verified
            || verified_report.run_id != outcome.run_id
            || verified_report.instrument != outcome.instrument
            || verification.result_hash != outcome.result_hash
            || verification.data_fingerprint != outcome.data_fingerprint
            || outcome.account_id.as_deref() != Some(run.account_id.as_str())
        {
            return Err(format!("账户 {} 的深度报告身份不一致", run.account_id));
        }
        let summary_path = PathBuf::from(&outcome.artifacts.summary);
        let declared_input = qx_app::artifact_store::recompute_declared_backtest_input(&summary)?;
        if (evidence || diagnostics.is_some()) && declared_input.is_none() {
            return Err(format!(
                "账户 {} 的 summary 未声明可复核输入，不能绑定诊断或生成 EvidenceBundle",
                run.account_id
            ));
        }
        let bound_diagnostics = diagnostics
            .as_ref()
            .map(|attachment| {
                let diagnostic = attachment
                    .reports
                    .get(&run.account_id)
                    .ok_or_else(|| format!("账户 {} 缺少策略诊断", run.account_id))?;
                let strategy_id = summary
                    .pointer("/strategy/strategy_id")
                    .or_else(|| summary.pointer("/strategy_id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        format!("账户 {} 的 summary 缺少 strategy_id", run.account_id)
                    })?;
                crate::report_command::validate_strategy_diagnostics_identity(
                    diagnostic,
                    strategy_id,
                    &outcome.instrument,
                    &declared_input
                        .as_ref()
                        .ok_or("策略诊断缺少重算后的数据输入身份")?
                        .fingerprint,
                )?;
                Ok::<_, String>(json!({
                    "identity_bound": true,
                    "account_id": &run.account_id,
                    "report": diagnostic,
                }))
            })
            .transpose()?;
        let account_evidence_path =
            if let Some(declared_input) = declared_input.as_ref().filter(|_| evidence) {
                let diagnostic_report = diagnostics
                    .as_ref()
                    .and_then(|attachment| attachment.reports.get(&run.account_id));
                let diagnostic_snapshot = diagnostic_report
                    .map(|report| {
                        write_account_diagnostic_snapshot(&summary_path, &run.account_id, report)
                    })
                    .transpose()?;
                match crate::run_evidence::write_run_evidence(
                    &summary_path,
                    &summary,
                    None,
                    diagnostic_snapshot
                        .as_ref()
                        .zip(diagnostic_report)
                        .map(|((snapshot, _), report)| (snapshot.as_path(), report)),
                ) {
                    Ok(evidence_path) => Some(evidence_path),
                    Err(error) => {
                        if let Some((snapshot, created)) = diagnostic_snapshot {
                            if created {
                                let _ = std::fs::remove_file(snapshot);
                            }
                        }
                        return Err(format!(
                            "账户 {} 生成运行 EvidenceBundle 失败（输入 {}）: {error}",
                            run.account_id, declared_input.path
                        ));
                    }
                }
            } else {
                None
            };
        let (mut fill_rows, fill_rows_total) =
            read_depth_account_fills(verified_report, outcome.fills, fill_row_limit)?;
        let fill_rows_truncated =
            verified_report.fills.truncated || fill_rows_total > fill_rows.len() as u64;
        let (event_rows, event_trace_path, event_rows_total) = if let Some(trace_path) =
            &outcome.artifacts.event_trace
        {
            let event_page = qx_app::read_event_report_v1_with_reader(
                trace_path,
                &summary,
                &outcome.artifacts.run_manifest,
                None,
                &qx_app::artifact_store::FileArtifactReader,
            )
            .map_err(|error| format!("账户 {} 深度 EventLog 复核失败: {error}", run.account_id))?;
            let mut order_events = BTreeMap::new();
            let rows = event_page
                    .rows
                    .iter()
                    .skip(event_page.rows.len().saturating_sub(
                        event_row_limit.min(qx_app::MAX_EVENT_REPORT_PAGE_ROWS),
                    ))
                    .map(|event| {
                        let kind_value = event.get("kind").cloned().unwrap_or(Value::Null);
                        let (kind, order_id) = event
                            .get("kind")
                            .and_then(Value::as_object)
                            .and_then(|tagged| tagged.iter().next())
                            .map(|(kind, payload)| {
                                let order = payload
                                    .pointer("/fill/order_id")
                                    .or_else(|| payload.pointer("/order/client_order_id"))
                                    .or_else(|| payload.get("client_order_id"));
                                let order_id = order.map(|value| {
                                    value
                                        .as_str()
                                        .map(str::to_owned)
                                        .unwrap_or_else(|| value.to_string())
                                });
                                (kind.clone(), order_id)
                            })
                            .unwrap_or_else(|| ("unknown".into(), None));
                        if let (Some(order_id), Some(seq)) =
                            (order_id.as_ref(), event.get("seq").and_then(Value::as_u64))
                        {
                            order_events.entry(order_id.clone()).or_insert(seq);
                        }
                        json!({
                            "event_id": format!("account-{}-event-{}", run.account_id, event["seq"]),
                            "account_id": &run.account_id,
                            "seq": event["seq"],
                            "timestamp_ms": event["ts"],
                            "priority": event["prio"],
                            "source_seq": event["source_seq"],
                            "kind": kind,
                            "order_id": order_id,
                            "payload": serde_json::to_string(&kind_value).unwrap_or_else(|_| "{}".into()),
                        })
                    })
                    .collect::<Vec<_>>();
            for fill in &mut fill_rows {
                if let Some(seq) = fill
                    .get("order_id")
                    .and_then(Value::as_str)
                    .and_then(|order_id| order_events.get(order_id))
                {
                    fill["event_seq"] = json!(seq);
                    fill["event_id"] = json!(format!("account-{}-event-{}", run.account_id, seq));
                }
            }
            (rows, Some(PathBuf::from(trace_path)), event_page.total)
        } else {
            (Vec::new(), None, 0)
        };
        let event_rows_truncated =
            event_trace_path.is_some() && event_rows_total > event_rows.len() as u64;
        let equity_values =
            crate::report_html::read_equity_csv(Path::new(&outcome.artifacts.equity))?;
        let equity_series = downsample_equity(&equity_values, 2_000)
            .into_iter()
            .map(|(timestamp_ms, equity_raw)| {
                json!({"timestamp_ms": timestamp_ms, "equity_raw": equity_raw.to_string()})
            })
            .collect::<Vec<_>>();
        let equity_series_truncated =
            verified_report.equity.truncated || outcome.equity_points > equity_series.len() as u64;
        accounts.push(json!({
            "account_id": &run.account_id,
            "status": "succeeded",
            "run_id": &outcome.run_id,
            "instrument": &outcome.instrument,
            "tier": &outcome.tier,
            "snapshots": outcome.snapshots,
            "strategy_id": summary.pointer("/strategy/strategy_id").or_else(|| summary.pointer("/strategy_id")),
            "result_hash": &outcome.result_hash,
            "data_fingerprint": declared_input.as_ref().map(|input| input.fingerprint.as_str()).unwrap_or(&verification.data_fingerprint),
            "run_data_fingerprint": &verification.data_fingerprint,
            "strategy_bias_diagnostics": bound_diagnostics,
            "return_bps": outcome.return_bps,
            "max_drawdown_bps": outcome.max_drawdown_bps,
            "fills": outcome.fills,
            "fill_rows_total": fill_rows_total,
            "fill_rows": fill_rows,
            "fill_rows_truncated": fill_rows_truncated,
            "event_rows": event_rows,
            "event_rows_total": event_rows_total,
            "event_rows_truncated": event_rows_truncated,
            "event_trace_path": event_trace_path,
            "equity_points": outcome.equity_points,
            "equity_series": equity_series,
            "equity_series_truncated": equity_series_truncated,
            "summary_path": &summary_path,
            "run_evidence_path": account_evidence_path,
            "equity_path": &outcome.artifacts.equity,
            "fills_path": &outcome.artifacts.fills,
        }));
    }

    let report = json!({
        "schema_version": 1,
        "report_kind": "multi_account_depth_backtest",
        "source_path": path,
        "batch_id": &verified_batch.run_id,
        "settlement_currency": &verified_batch.settlement_currency,
        "spec_fingerprint": &verified_batch.spec_fingerprint,
        "summary": &verified_batch.summary,
        "account_semantics": "accounts_are_independent; cash_positions_risk_and_returns_are_not_netted",
        "verification_scope": "shared_qx_app_report_verified_parent_child_artifacts_and_replayed_depth_event_logs",
        "detail_limits": {
            "fill_rows_per_account_max": fill_row_limit_max,
            "fill_rows_batch_max": MAX_REPORT_FILL_ROWS_TOTAL,
            "event_rows_per_account_max": event_row_limit_max.min(qx_app::MAX_EVENT_REPORT_PAGE_ROWS),
            "event_rows_batch_max": MAX_REPORT_EVENT_ROWS_TOTAL.min(qx_app::MAX_EVENT_REPORT_PAGE_ROWS * accounts_total),
            "sampling": "verified_qx_app_rows_with_cli_tail_limits_and_equity_downsampling",
        },
        "accounts": accounts,
    });
    if let Some(xlsx_path) = xlsx {
        crate::report_xlsx::validate_xlsx_path(xlsx_path)?;
        validate_depth_report_output_target(
            xlsx_path,
            path,
            strategy_diagnostics,
            &result,
            evidence,
        )?;
    }
    let html_path = if let Some(target) = html_target {
        if let Some(parent) = target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建报告目录失败: {error}"))?;
        }
        let mut rendered = render_html(&report)?
            .replace("牵星多账户回测研究报告", "牵星多账户深度回测研究报告")
            .replace(
                "<h1>多账户回测研究报告</h1>",
                "<h1>多账户深度回测研究报告</h1>",
            );
        let depth_rows = report["accounts"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|account| {
                format!(
                    "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    escape_html(account["account_id"].as_str().unwrap_or("")),
                    escape_html(account["instrument"].as_str().unwrap_or("")),
                    escape_html(account["tier"].as_str().unwrap_or("")),
                    escape_html(
                        account["snapshots"]
                            .as_u64()
                            .map(|value| value.to_string())
                            .as_deref()
                            .unwrap_or("absent")
                    ),
                )
            })
            .collect::<String>();
        rendered = rendered.replace(
            "<section class=\"cards\">",
            &format!("<section><h2>深度输入</h2><div class=\"scroll\"><table><thead><tr><th>账户</th><th>标的</th><th>档位</th><th>快照</th></tr></thead><tbody>{depth_rows}</tbody></table></div></section><section class=\"cards\">"),
        );
        qx_app::artifact_store::sink::publish(
            &target,
            &rendered,
            qx_app::artifact_store::sink::PublishMode::Overwrite,
        )
        .map_err(|error| format!("发布多账户深度 HTML 报告失败: {error}"))?;
        Some(target)
    } else {
        None
    };
    let xlsx_path = xlsx
        .map(|target| crate::report_xlsx::write_multi_account_report_xlsx(&report, target))
        .transpose()?;
    if as_json {
        let mut value = json!({"schema_version":1,"report":report});
        if let Some(path) = html_path {
            value["generated_artifacts"]["html"] = json!(path);
        }
        if let Some(path) = xlsx_path {
            value["generated_artifacts"]["xlsx"] = json!(path);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?
        );
    } else if let Some(html_path) = html_path {
        println!("[Report] html={}", html_path.display());
        if let Some(xlsx_path) = xlsx_path {
            println!("[Report] xlsx={}", xlsx_path.display());
        }
        println!(
            "[Report] batch_id={} accounts={} complete={}",
            result.batch_id,
            result.runs.len(),
            result.summary.complete
        );
    } else {
        if let Some(xlsx_path) = xlsx_path {
            println!("[Report] xlsx={}", xlsx_path.display());
        }
        println!(
            "[Report] batch_id={} accounts={} complete={}",
            result.batch_id,
            result.runs.len(),
            result.summary.complete
        );
    }
    Ok(true)
}

fn validate_depth_report_output_target(
    target: &Path,
    source: &Path,
    strategy_diagnostics: Option<&Path>,
    result: &qx_app::MultiAccountDepthBacktestResult,
    evidence: bool,
) -> Result<(), String> {
    if crate::report_output_paths::same_output_target(target, source) {
        return Err("报告不能覆盖多账户深度结果输入".into());
    }
    if strategy_diagnostics.is_some_and(|diagnostics| {
        crate::report_output_paths::same_output_target(target, diagnostics)
    }) {
        return Err("报告不能覆盖策略诊断输入".into());
    }
    for outcome in result.runs.iter().filter_map(|run| run.outcome.as_ref()) {
        for artifact in [
            &outcome.artifacts.run_manifest,
            &outcome.artifacts.summary,
            &outcome.artifacts.equity,
            &outcome.artifacts.fills,
        ]
        .into_iter()
        .chain(outcome.artifacts.event_trace.iter())
        {
            if crate::report_output_paths::same_output_target(target, Path::new(artifact)) {
                return Err(format!("报告不能覆盖账户运行工件: {artifact}"));
            }
        }
        if evidence {
            let summary_path = Path::new(&outcome.artifacts.summary);
            let stem = evidence_summary_stem(summary_path)?;
            for suffix in [".evidence.json", ".strategy-diagnostics.evidence.json"] {
                let evidence_path = summary_path.with_file_name(format!("{stem}{suffix}"));
                if crate::report_output_paths::same_output_target(target, &evidence_path) {
                    return Err(format!(
                        "报告不能覆盖账户运行 EvidenceBundle: {}",
                        evidence_path.display()
                    ));
                }
            }
            if let Some(account_id) = outcome.account_id.as_deref() {
                let diagnostic_path = account_diagnostic_snapshot_path(summary_path, account_id)?;
                if crate::report_output_paths::same_output_target(target, &diagnostic_path) {
                    return Err(format!(
                        "报告不能覆盖账户策略诊断证据附件: {}",
                        diagnostic_path.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_report_output_target(
    target: &Path,
    source: &Path,
    strategy_diagnostics: Option<&Path>,
    result: &qx_app::MultiAccountBacktestResult,
    evidence: bool,
) -> Result<(), String> {
    if crate::report_output_paths::same_output_target(target, source) {
        return Err("报告不能覆盖多账户结果输入".into());
    }
    if strategy_diagnostics.is_some_and(|diagnostics| {
        crate::report_output_paths::same_output_target(target, diagnostics)
    }) {
        return Err("报告不能覆盖策略诊断输入".into());
    }
    for outcome in result.runs.iter().filter_map(|run| run.outcome.as_ref()) {
        for artifact in [
            &outcome.artifacts.run_manifest,
            &outcome.artifacts.summary,
            &outcome.artifacts.equity,
            &outcome.artifacts.fills,
        ]
        .into_iter()
        .chain(outcome.artifacts.event_trace.iter())
        {
            if crate::report_output_paths::same_output_target(target, Path::new(artifact)) {
                return Err(format!("报告不能覆盖账户运行工件: {artifact}"));
            }
        }
        if evidence {
            let summary_path = Path::new(&outcome.artifacts.summary);
            let stem = evidence_summary_stem(summary_path)?;
            for suffix in [".evidence.json", ".strategy-diagnostics.evidence.json"] {
                let evidence_path = summary_path.with_file_name(format!("{stem}{suffix}"));
                if crate::report_output_paths::same_output_target(target, &evidence_path) {
                    return Err(format!(
                        "报告不能覆盖账户运行 EvidenceBundle: {}",
                        evidence_path.display()
                    ));
                }
            }
            if strategy_diagnostics.is_some() {
                if let Some(account_id) = outcome.account_id.as_deref() {
                    let diagnostic_path =
                        account_diagnostic_snapshot_path(summary_path, account_id)?;
                    if crate::report_output_paths::same_output_target(target, &diagnostic_path) {
                        return Err(format!(
                            "报告不能覆盖账户策略诊断证据附件: {}",
                            diagnostic_path.display()
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn evidence_summary_stem(summary_path: &Path) -> Result<String, String> {
    summary_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".summary.json"))
        .map(str::to_string)
        .ok_or_else(|| format!("账户 summary 文件名无效: {}", summary_path.display()))
}

fn account_diagnostic_snapshot_path(
    summary_path: &Path,
    account_id: &str,
) -> Result<PathBuf, String> {
    let stem = evidence_summary_stem(summary_path)?;
    let account_digest = qx_strategy::sha256_hex(account_id.as_bytes());
    Ok(summary_path.with_file_name(format!(
        "{stem}.account-{}.strategy-diagnostics.json",
        &account_digest[..16]
    )))
}

/// Write a stable, account-specific diagnostic snapshot for a standard single-run EvidenceBundle.
/// Existing bytes are reused only when identical; a different file is never overwritten.
fn write_account_diagnostic_snapshot(
    summary_path: &Path,
    account_id: &str,
    report: &Value,
) -> Result<(PathBuf, bool), String> {
    let path = account_diagnostic_snapshot_path(summary_path, account_id)?;
    let bytes = serde_json::to_vec_pretty(report)
        .map_err(|error| format!("编码账户 {account_id} 策略诊断快照失败: {error}"))?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
                let _ = std::fs::remove_file(&path);
                return Err(format!(
                    "写入账户 {account_id} 策略诊断快照失败 {}: {error}",
                    path.display()
                ));
            }
            Ok((path, true))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read(&path).map_err(|read_error| {
                format!(
                    "读取已有账户策略诊断快照失败 {}: {read_error}",
                    path.display()
                )
            })?;
            if existing != bytes {
                return Err(format!(
                    "账户 {account_id} 的策略诊断快照已存在且内容不同，拒绝覆盖: {}",
                    path.display()
                ));
            }
            Ok((path, false))
        }
        Err(error) => Err(format!(
            "创建账户 {account_id} 策略诊断快照失败 {}: {error}",
            path.display()
        )),
    }
}

fn read_depth_account_fills(
    report: &qx_app::ResearchRunReportV1,
    expected: u64,
    row_limit: usize,
) -> Result<(Vec<Value>, u64), String> {
    if report.fills.total != expected {
        return Err(format!(
            "Depth 成交报告行数与 outcome.fills 不一致: rows={} expected={expected}",
            report.fills.total
        ));
    }
    let rows = &report.fills.rows;
    let start = rows.len().saturating_sub(row_limit);
    let projected = rows[start..]
        .iter()
        .map(|row| {
            let timestamp_ms = row
                .get("ts")
                .and_then(|value| {
                    value
                        .as_u64()
                        .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
                })
                .ok_or("Depth 成交报告缺少有效 ts")?;
            let cell = |name: &str| {
                row.get(name)
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string())
                    })
                    .ok_or_else(|| format!("Depth 成交报告缺少 {name}"))
            };
            Ok::<_, String>(json!({
                "timestamp_ms": timestamp_ms,
                "order_id": cell("order_id")?,
                "qty_raw": cell("qty_raw")?,
                "price_raw": cell("price_raw")?,
                "fee_raw": cell("fee_raw")?,
                "strategy_id": row.get("strategy_id").filter(|value| !value.is_null()),
                "side": null,
                "side_status": "absent_from_fill_contract",
            }))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((projected, report.fills.total))
}

fn read_account_fills(
    path: &str,
    expected: u64,
    row_limit: usize,
) -> Result<(Vec<Value>, u64), String> {
    let (fills, total) = crate::report_html::read_fills_csv_tail(Path::new(path), row_limit)?;
    if total != expected {
        return Err(format!(
            "成交工件行数与 outcome.fills 不一致 {path}: rows={total} expected={expected}",
        ));
    }
    let rows = fills
        .into_iter()
        .map(|fill| {
            json!({
                "timestamp_ms": fill.ts,
                "order_id": fill.order_id.to_string(),
                "qty_raw": fill.qty_raw.to_string(),
                "qty_exact": fill.qty_exact,
                "price_raw": fill.price_raw.to_string(),
                "price_exact": fill.price_exact,
                "fee_raw": fill.fee_raw.map(|value| value.to_string()),
                "exact_fees_json": fill.exact_fees_json,
                "strategy_id": if fill.strategy_id.is_empty() { None } else { Some(fill.strategy_id) },
                "side": null,
                "side_status": "absent_from_fill_contract",
            })
        })
        .collect();
    Ok((rows, total))
}

struct MultiAccountDiagnostics {
    path: String,
    sha256: String,
    reports: BTreeMap<String, Value>,
}

fn read_multi_account_diagnostics(path: &Path) -> Result<MultiAccountDiagnostics, String> {
    let size = std::fs::metadata(path)
        .map_err(|error| format!("读取多账户策略诊断文件属性失败 {}: {error}", path.display()))?
        .len();
    if size > MAX_MULTI_ACCOUNT_DIAGNOSTICS_BYTES {
        return Err(format!(
            "多账户策略诊断文件超过 {} MiB 上限: {}",
            MAX_MULTI_ACCOUNT_DIAGNOSTICS_BYTES / (1024 * 1024),
            path.display()
        ));
    }
    let bytes = std::fs::read(path)
        .map_err(|error| format!("读取多账户策略诊断文件失败 {}: {error}", path.display()))?;
    let payload: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("多账户策略诊断 JSON 无效 {}: {error}", path.display()))?;
    if payload.get("schema_version").and_then(Value::as_u64) != Some(1)
        || payload.get("kind").and_then(Value::as_str)
            != Some("qianxing-multi-account-strategy-bias-diagnostics")
        || payload.as_object().is_none_or(|object| {
            object.len() != 3
                || !object.contains_key("schema_version")
                || !object.contains_key("kind")
                || !object.contains_key("accounts")
        })
    {
        return Err(
            "多账户策略诊断须符合 qianxing-multi-account-strategy-bias-diagnostics v1".into(),
        );
    }
    let accounts = payload
        .get("accounts")
        .and_then(Value::as_array)
        .filter(|accounts| !accounts.is_empty() && accounts.len() <= 32)
        .ok_or("多账户策略诊断 accounts 必须包含 1..=32 项")?;
    let mut reports = BTreeMap::new();
    for item in accounts {
        if item.as_object().is_none_or(|object| {
            object.len() != 2
                || !object.contains_key("account_id")
                || !object.contains_key("report")
        }) {
            return Err("多账户策略诊断项须且仅须包含 account_id 与 report".into());
        }
        let account_id = item
            .get("account_id")
            .and_then(Value::as_str)
            .filter(|account| !account.trim().is_empty())
            .ok_or("多账户策略诊断 account_id 必须为非空字符串")?;
        let report = item
            .get("report")
            .filter(|report| report.is_object())
            .ok_or_else(|| format!("账户 {account_id} 的策略诊断 report 必须为对象"))?;
        if reports
            .insert(account_id.to_string(), report.clone())
            .is_some()
        {
            return Err(format!("多账户策略诊断含重复 account_id: {account_id}"));
        }
    }
    Ok(MultiAccountDiagnostics {
        path: path.to_string_lossy().into_owned(),
        sha256: qx_data::sha256_hex_bytes(&bytes),
        reports,
    })
}

fn batch_row_budget(
    total: usize,
    accounts: usize,
    account_index: usize,
    per_account: usize,
) -> usize {
    let base = total / accounts;
    let remainder = total % accounts;
    per_account.min(base + usize::from(account_index < remainder))
}

fn downsample_equity(points: &[(i64, i128)], max_points: usize) -> Vec<(i64, i128)> {
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

fn render_html(report: &Value) -> Result<String, String> {
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

fn bounded_html_report(report: &Value) -> Value {
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

#[cfg(test)]
mod tests {
    use super::{
        batch_row_budget, bounded_html_report, read_account_fills, MAX_HTML_DETAIL_ROWS_PER_ACCOUNT,
    };
    use serde_json::json;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn html_detail_projection_is_bounded_and_drops_links_to_hidden_events() {
        let mut fills = (0..300)
            .map(|index| {
                json!({
                    "timestamp_ms": index,
                    "event_id": format!("event-{index}"),
                    "event_seq": index
                })
            })
            .collect::<Vec<_>>();
        fills[299]["event_id"] = json!("event-0");
        fills[299]["event_seq"] = json!(0);
        let events = (0..300)
            .map(|index| json!({"event_id": format!("event-{index}"), "seq": index}))
            .collect::<Vec<_>>();
        let report = json!({
            "accounts": [{
                "fill_rows_total": 450,
                "event_rows_total": 9_000,
                "fill_rows": fills,
                "event_rows": events
            }]
        });

        let html_report = bounded_html_report(&report);
        let account = &html_report["accounts"][0];
        assert_eq!(
            account["fill_rows"].as_array().unwrap().len(),
            MAX_HTML_DETAIL_ROWS_PER_ACCOUNT
        );
        assert_eq!(
            account["event_rows"].as_array().unwrap().len(),
            MAX_HTML_DETAIL_ROWS_PER_ACCOUNT
        );
        assert_eq!(account["fill_rows"][0]["timestamp_ms"], 50);
        assert_eq!(account["fill_rows"][0]["event_id"], "event-50");
        assert!(account["fill_rows"][249].get("event_id").is_none());
        assert_eq!(account["fill_rows_total"], 450);
        assert_eq!(account["event_rows_total"], 9_000);
        assert_eq!(
            report["accounts"][0]["fill_rows"].as_array().unwrap().len(),
            300
        );
    }

    #[test]
    fn report_fill_reader_keeps_only_the_recent_bounded_window_and_checks_total() {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::current_dir()
            .unwrap()
            .join("target/report-fill-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!(
            "qx-report-fill-{}-{}.csv",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "ts,order_id,qty_raw,price_raw,fee_raw,strategy_id").unwrap();
        for index in 0..10 {
            writeln!(file, "{index},{index},1,2,0,strategy").unwrap();
        }
        drop(file);
        let path_text = path.to_str().unwrap();
        let (rows, total) = read_account_fills(path_text, 10, 3).unwrap();
        assert_eq!(total, 10);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["timestamp_ms"], 7);
        assert_eq!(rows[2]["timestamp_ms"], 9);
        assert!(read_account_fills(path_text, 9, 3).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn report_row_budgets_are_fair_and_never_exceed_the_batch_limit() {
        for accounts in [2, 3, 11, 32] {
            let limits = (0..accounts)
                .map(|index| batch_row_budget(50_000, accounts, index, 5_000))
                .collect::<Vec<_>>();
            assert!(limits.windows(2).all(|pair| pair[0] >= pair[1]));
            assert!(limits.iter().all(|limit| *limit <= 5_000));
            assert!(limits.iter().sum::<usize>() <= 50_000);
        }
        assert_eq!(
            (0..32)
                .map(|index| batch_row_budget(50_000, 32, index, 5_000))
                .sum::<usize>(),
            50_000
        );
    }
}
