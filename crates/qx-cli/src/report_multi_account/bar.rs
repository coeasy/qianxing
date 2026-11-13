//! Verified multi-account Bar report projection and output publication.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn run_bar_report(
    path: &Path,
    value: Value,
    as_json: bool,
    html: bool,
    evidence: bool,
    output: Option<&Path>,
    xlsx: Option<&Path>,
    strategy_diagnostics: Option<&Path>,
) -> Result<bool, String> {
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

    let mut evidence_outputs = Vec::new();
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
        let account_evidence_path = if let Some(declared_input) =
            declared_input.as_ref().filter(|_| evidence)
        {
            let diagnostic_report = diagnostics
                .as_ref()
                .and_then(|attachment| attachment.reports.get(&run.account_id));
            let diagnostic_snapshot = diagnostic_report
                .map(|report| {
                    write_account_diagnostic_snapshot(&summary_path, &run.account_id, report)
                })
                .transpose()?;
            let evidence_result = crate::run_evidence::prepare_run_evidence(
                &summary_path,
                &summary,
                None,
                diagnostic_snapshot
                    .as_ref()
                    .zip(diagnostic_report)
                    .map(|((path, _), report)| (path.as_path(), report)),
            );
            match evidence_result {
                Ok((path, payload)) => {
                    evidence_outputs.push((path.clone(), payload));
                    if let Some((snapshot_path, _)) = diagnostic_snapshot {
                        let snapshot_bytes = std::fs::read(&snapshot_path).map_err(|error| {
                            format!(
                                "读取账户策略诊断快照失败 {}: {error}",
                                snapshot_path.display()
                            )
                        })?;
                        evidence_outputs.push((snapshot_path, snapshot_bytes));
                    }
                    Some(path)
                }
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
    let mut output_bundle = crate::report_publish::ReportOutputBundle::default();
    output_bundle.extend(evidence_outputs);
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
        output_bundle.add_bytes(&target, rendered.into_bytes());
        Some(target)
    } else {
        None
    };
    let xlsx_path = if let Some(target) = xlsx {
        let staged = output_bundle.stage_path(target)?;
        crate::report_xlsx::write_multi_account_report_xlsx(&report, &staged)?;
        output_bundle.add_staged(target, &staged)?;
        Some(target.to_path_buf())
    } else {
        None
    };
    output_bundle.commit()?;
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
