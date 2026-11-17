//! Bounded offline HTML rendering for single and comparison multi-market reports.

use serde_json::Value;

pub(super) fn render_html(report: &Value, comparison: bool) -> Result<String, String> {
    let mut html_report = report.clone();
    if comparison {
        for run in html_report["runs"].as_array_mut().into_iter().flatten() {
            bound_html_series(run);
        }
    } else {
        bound_html_series(&mut html_report);
    }
    let data = serde_json::to_string(&html_report)
        .map_err(|e| e.to_string())?
        .replace('<', "\\u003c");
    let metric_definition = if comparison {
        html_report["runs"]
            .as_array()
            .and_then(|runs| runs.first())
            .and_then(|run| run.get("metrics_definition"))
    } else {
        html_report.get("metrics_definition")
    };
    let metric_definition = metric_definition
        .map(serde_json::to_string_pretty)
        .transpose()
        .map_err(|error| error.to_string())?
        .unwrap_or_else(|| "{}".into());
    let exact_fees = if comparison {
        html_report["runs"]
            .as_array()
            .and_then(|runs| runs.first())
            .and_then(|run| run.pointer("/metrics_exact/fees_by_currency"))
    } else {
        html_report.pointer("/metrics_exact/fees_by_currency")
    }
    .map(serde_json::to_string)
    .transpose()
    .map_err(|error| error.to_string())?
    .unwrap_or_else(|| "{}".into());
    let metric_definition_section = format!(
        "<section id=\"metricDefinitions\"><h2>指标定义</h2><p class=\"muted\">每周期风险指标不年化；基准只使用严格时间戳对齐的观测；精确费用按币种汇总，raw 费用仅作兼容投影。</p><pre>{}</pre><p>精确手续费（按币种）：<code>{}</code></p></section>",
        crate::report_html::escape_html(&metric_definition),
        crate::report_html::escape_html(&exact_fees)
    );
    let template = r#"<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>牵星多市场研究报告</title><style>body{font:15px system-ui;margin:2rem;background:#f4f6fa;color:#172033}main{max-width:1320px;margin:auto}section,header{background:#fff;border-radius:12px;padding:1rem 1.4rem;margin:1rem 0;box-shadow:0 2px 12px #10204012}canvas{width:100%;height:360px}table{border-collapse:collapse;width:100%}td,th{padding:.55rem;border-bottom:1px solid #ddd;text-align:left;vertical-align:top}small,.muted{color:#536178}.cards,.legend{display:flex;gap:.65rem;flex-wrap:wrap}.card,.legend label{background:#eef3fb;padding:.65rem .9rem;border-radius:9px}.toolbar{display:flex;gap:1rem;flex-wrap:wrap;margin:.7rem 0}.toolbar input{min-width:15rem;padding:.45rem}pre{white-space:pre-wrap;max-width:38rem;max-height:20rem;overflow:auto}</style><main><header><h1>多市场共享账户回测研究报告</h1><div id="cards" class="cards"></div><p class="muted">输入、质量清单、权益与成交产物已完成身份复核。权益曲线展示共享账户总账；对照时按各轮起点归一化为基点收益。</p></header><section><h2>权益曲线</h2><div id="legend" class="legend"></div><div class="toolbar"><label>显示范围 <input id="range" type="range" min="1" max="100" value="100"></label><span id="hover" class="muted">移动鼠标到曲线上查看时间与收益</span></div><canvas id="equity"></canvas></section><section><h2>市场来源、策略与成交归因</h2><div class="toolbar"><input id="marketSearch" placeholder="筛选标的、数据指纹或来源路径"></div><table id="markets"></table></section><section id="compare"><h2>同输入运行排名</h2><div class="toolbar"><input id="runSearch" placeholder="筛选运行 ID"></div><table id="runs"></table></section><section><h2>成交明细</h2><div class="toolbar"><input id="fillSearch" placeholder="筛选运行、策略、订单或时间"><span id="fillCount" class="muted"></span></div><table id="fills"></table><p class="muted">完整成交数组保留在本地 JSON 报告中；HTML 明细最多显示 2,000 笔以控制浏览器资源。</p></section><section><h2>已验证事件时间线</h2><div class="toolbar"><input id="eventSearch" placeholder="筛选运行、事件、订单或时间"><span id="eventCount" class="muted"></span></div><table id="events"></table><p class="muted">完整事件流和重放摘要保留在本地 JSON 报告中；HTML 每轮最多展示最近 2,000 个事件。</p></section></main><script>const r=__DATA__,cmp=__CMP__,esc=s=>String(s??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));const runs=cmp?r.runs:[r],markets=cmp?r.market_sources:r.markets,colors=['#2463eb','#e4572e','#1b9e77','#8e44ad','#d4a017','#566573'];let visible=runs.map(()=>true),series=[];const eventLinks=new Map();function renderTables(){const mh='<tr><th>标的</th><th>Bar数</th><th>成交数</th><th>数据指纹</th><th>质量</th><th>来源与配置</th></tr>';document.querySelector('#markets').innerHTML=mh+markets.map(m=>`<tr data-search="${esc([m.instrument,m.path,m.content_hash].join(' ')).toLowerCase()}"><td>${esc(m.instrument)}</td><td>${esc(m.row_count)}</td><td>${esc(m.fills)}</td><td><code>${esc(m.content_hash)}</code></td><td>${esc(m.quality_status)}</td><td><code>${esc(m.path)}</code>${m.strategies?`<details><summary>策略、规则与费用</summary><pre>${esc(JSON.stringify({strategies:m.strategies,risk_market_spec:m.risk_market_spec,fee_schedule:m.fee_schedule},null,2))}</pre></details>`:''}</td></tr>`).join('');document.querySelector('#marketSearch').oninput=e=>filterRows('#markets tbody tr',e.target.value);if(cmp){document.querySelector('#compare').style.display='block';document.querySelector('#runs').innerHTML='<tr><th>排名</th><th>运行</th><th>收益 bp</th><th>最大回撤 bp</th><th>成交</th><th>结果哈希</th></tr>'+r.runs.map((x,i)=>`<tr data-search="${esc(x.run_id).toLowerCase()}"><td>${i+1}</td><td>${esc(x.run_id)}</td><td>${esc(x.return_bps)}</td><td>${esc(x.max_drawdown_bps)}</td><td>${esc(x.successful_fills)}</td><td><code>${esc(x.result_hash)}</code></td></tr>`).join('');document.querySelector('#runSearch').oninput=e=>filterRows('#runs tbody tr',e.target.value);}else document.querySelector('#compare').style.display='none';let fills=[];runs.forEach((x,ri)=>(x.fills||[]).forEach(f=>fills.push({run_id:x.run_id,run_index:ri,...f})));const shown=fills.slice(-2000).reverse();document.querySelector('#fillCount').textContent=`显示 ${shown.length} / ${fills.length} 笔（最新优先）`;document.querySelector('#fills').innerHTML='<tr><th>运行</th><th>策略</th><th>订单</th><th>时间戳</th><th>数量 raw</th><th>价格 raw</th><th>费用 raw</th></tr>'+shown.map(f=>`<tr data-search="${esc([f.run_id,f.strategy_id,f.order_id,f.ts].join(' ')).toLowerCase()}"><td>${esc(f.run_id)}</td><td>${esc(f.strategy_id??'—')}</td><td>${eventLinks.has(`${f.run_index}:${f.order_id}`)?`<a href='#${eventLinks.get(`${f.run_index}:${f.order_id}`)}'>${esc(f.order_id)}</a>`:esc(f.order_id)}</td><td>${esc(f.ts)}</td><td>${esc(f.qty?.raw??JSON.stringify(f.qty))}</td><td>${esc(f.price?.raw??JSON.stringify(f.price))}</td><td>${esc(f.fee?.raw??JSON.stringify(f.fee))}</td></tr>`).join('');document.querySelector('#fillSearch').oninput=e=>filterRows('#fills tbody tr',e.target.value);document.querySelector('#cards').innerHTML=cmp?[`同数据对照 ${r.runs.length} 轮`,`标的 ${markets.length} 个`,`Fingerprint ${r.data_fingerprint}`].map(x=>`<div class="card">${esc(x)}</div>`).join(''):[['账户',r.account_id],['策略槽',r.strategies_total],['总成交',r.successful_fills],['收益 bp',r.return_bps],['最大回撤 bp',r.max_drawdown_bps],['结果哈希',r.result_hash]].map(x=>`<div class="card">${esc(x[0])}：${esc(x[1])}</div>`).join('');}function renderEvents(){let all=[];runs.forEach((run,ri)=>(run.events||[]).forEach(ev=>{const kind=typeof ev.kind==='string'?ev.kind:Object.keys(ev.kind||{})[0]||'unknown',payload=typeof ev.kind==='object'?ev.kind[kind]:null,order=payload?.fill?.order_id??payload?.client_order_id??payload?.order?.client_order_id??'',id=`ev-${ri}-${ev.seq}`;all.push({run_id:run.run_id,run_index:ri,kind,order,ts:ev.ts,seq:ev.seq,id})}));const shown=all.slice(-2000).reverse();shown.forEach(ev=>{if(ev.kind==='Filled'&&ev.order!=='')eventLinks.set(`${ev.run_index}:${ev.order}`,ev.id)});document.querySelector('#eventCount').textContent=`显示 ${shown.length} / ${runs.reduce((sum,run)=>sum+Number(run.event_count||0),0)} 个事件（最新优先）`;document.querySelector('#events').innerHTML='<tr><th>运行</th><th>序号</th><th>时间戳</th><th>类型</th><th>订单</th></tr>'+shown.map(ev=>`<tr id="${ev.id}" data-search="${esc([ev.run_id,ev.seq,ev.ts,ev.kind,ev.order].join(' ')).toLowerCase()}"><td>${esc(ev.run_id)}</td><td>${esc(ev.seq)}</td><td>${esc(ev.ts)}</td><td>${esc(ev.kind)}</td><td>${esc(ev.order)}</td></tr>`).join('');document.querySelector('#eventSearch').oninput=e=>filterRows('#events tbody tr',e.target.value)}function filterRows(selector,query){const q=query.trim().toLowerCase();document.querySelectorAll(selector).forEach(row=>row.style.display=row.dataset.search.includes(q)?'':'none')}function draw(){const c=document.querySelector('#equity'),ctx=c.getContext('2d'),dpr=devicePixelRatio||1,w=c.clientWidth,h=360;c.width=w*dpr;c.height=h*dpr;ctx.scale(dpr,dpr);const limit=Math.max(2,Math.ceil((runs[0]?.equity?.length||0)*Number(document.querySelector('#range').value)/100));series=runs.map((run,ri)=>{const a=run.equity||[],end=Math.min(a.length,limit),stride=Math.max(1,Math.ceil(end/2000)),sample=[];for(let i=0;i<end;i+=stride)sample.push(a[i]);if(end&&sample.at(-1)!==a[end-1])sample.push(a[end-1]);const base=Number(a[0]?.[1]||1);return {name:run.run_id,points:sample.map(x=>({ts:x[0],value:(Number(x[1])/base-1)*10000})),color:colors[ri%colors.length]}});const active=series.filter((_,i)=>visible[i]);if(!active.length)return;const vals=active.flatMap(s=>s.points.map(p=>p.value)),lo=Math.min(0,...vals),hi=Math.max(0,...vals),pad=38,plotW=w-pad*2,plotH=h-pad*2;ctx.strokeStyle='#bcc5d2';ctx.beginPath();ctx.moveTo(pad,pad);ctx.lineTo(pad,h-pad);ctx.lineTo(w-pad,h-pad);ctx.stroke();for(const [ri,s] of series.entries()){if(!visible[ri]||!s.points.length)continue;ctx.strokeStyle=s.color;ctx.lineWidth=2;ctx.beginPath();s.points.forEach((p,i)=>{const x=pad+i*plotW/Math.max(1,s.points.length-1),y=h-pad-(p.value-lo)/Math.max(1,hi-lo)*plotH;i?ctx.lineTo(x,y):ctx.moveTo(x,y)});ctx.stroke()}ctx.fillStyle='#536178';ctx.fillText(`${lo.toFixed(1)} bp`,2,pad);ctx.fillText(`${hi.toFixed(1)} bp`,2,h-pad);c.onmousemove=e=>{const rect=c.getBoundingClientRect(),x=e.clientX-rect.left-pad,idx=Math.max(0,Math.min(active[0].points.length-1,Math.round(x/plotW*Math.max(1,active[0].points.length-1)))),p=active[0].points[idx];if(!p)return;document.querySelector('#hover').textContent=`ts ${p.ts} · `+active.map(s=>`${s.name}: ${s.points[idx]?.value.toFixed(2)??'—'} bp`).join(' · ')};c.onmouseleave=()=>document.querySelector('#hover').textContent='移动鼠标到曲线上查看时间与收益';}function renderLegend(){document.querySelector('#legend').innerHTML=series.map((s,i)=>`<label><input type="checkbox" data-series="${i}" ${visible[i]?'checked':''}><span style="color:${s.color}">●</span> ${esc(s.name)}</label>`).join('');document.querySelectorAll('[data-series]').forEach(box=>box.onchange=e=>{visible[Number(e.target.dataset.series)]=e.target.checked;draw()})}renderEvents();renderTables();draw();renderLegend();document.querySelector('#range').oninput=()=>{draw();renderLegend()};window.onresize=draw;</script></html>"#;
    let diagnostic_markup = r#"<section id="strategyDiagnostics" style="display:none"><h2>策略未来数据与预热诊断</h2><p class="muted">诊断结果按策略、标的与数据指纹绑定；报告展示具体计算边界。</p><div class="toolbar"><input id="diagSearch" type="search" placeholder="筛选标的或策略"><select id="diagLookahead"><option value="">所有未来数据状态</option><option value="potential_lookahead_detected">检测到变化</option><option value="no_change_detected">扰动后此前决策未变化</option><option value="causal_event_contract">因果事件接口；未做扰动</option><option value="insufficient_history">历史不足</option><option value="insufficient_work_budget">工作量预算不足</option></select></div><table id="diagnostics"></table></section>"#;
    let template = template.replace(
        "<section id=\"compare\">",
        &format!("{diagnostic_markup}<section id=\"compare\">"),
    );
    let contribution_markup = r#"<section id="marketContributions" style="display:none"><h2>按标的权益贡献</h2><p class="muted">采样轴与共享权益曲线对齐，并已核验初始资金加各标的贡献等于共享权益。金额保留 raw 精确值，曲线按初始资金折算为基点。</p><div class="toolbar"><label>运行 <select id="contributionRun"></select></label><label>标的 <select id="contributionInstrument"></select></label><span id="contributionHover" class="muted">移动鼠标查看时间、贡献 raw 与基点</span></div><canvas id="contributionChart"></canvas></section>"#;
    let template = template.replace(
        "<section><h2>市场来源、策略与成交归因</h2>",
        &format!("{contribution_markup}<section><h2>市场来源、策略与成交归因</h2>"),
    );
    let contribution_script = r#"function renderMarketContributions(){const section=document.querySelector('#marketContributions'),runSelect=document.querySelector('#contributionRun'),instrumentSelect=document.querySelector('#contributionInstrument'),canvas=document.querySelector('#contributionChart'),hover=document.querySelector('#contributionHover'),available=runs.filter(run=>run.instrument_equity_contributions&&typeof run.instrument_equity_contributions==='object'&&Object.keys(run.instrument_equity_contributions).length);if(!available.length)return;section.style.display='block';runSelect.innerHTML=available.map((run,i)=>`<option value="${i}">${esc(run.run_id)}</option>`).join('');const instruments=Object.keys(available[0].instrument_equity_contributions).sort();instrumentSelect.innerHTML=instruments.map(instrument=>`<option value="${esc(instrument)}">${esc(instrument)}</option>`).join('');function draw(){const run=available[Number(runSelect.value)],instrument=instrumentSelect.value,rows=run.instrument_equity_contributions[instrument]||[],initial=BigInt(run.initial_cash_raw||'0');if(!rows.length||initial<=0n){hover.textContent='该运行或标的没有可显示的已验证贡献采样';return}const points=rows.map(row=>({ts:row.ts,raw:row.pnl_contribution_raw,bps:Number(BigInt(row.pnl_contribution_raw)*10000n/initial)})),ctx=canvas.getContext('2d'),dpr=devicePixelRatio||1,w=canvas.clientWidth,h=360;canvas.width=w*dpr;canvas.height=h*dpr;ctx.scale(dpr,dpr);const lo=Math.min(0,...points.map(p=>p.bps)),hi=Math.max(0,...points.map(p=>p.bps)),pad=42,pw=w-pad*2,ph=h-pad*2;ctx.strokeStyle='#bcc5d2';ctx.beginPath();ctx.moveTo(pad,pad);ctx.lineTo(pad,h-pad);ctx.lineTo(w-pad,h-pad);ctx.stroke();ctx.strokeStyle='#1b9e77';ctx.lineWidth=2;ctx.beginPath();points.forEach((p,i)=>{const x=pad+i*pw/Math.max(1,points.length-1),y=h-pad-(p.bps-lo)/Math.max(1,hi-lo)*ph;i?ctx.lineTo(x,y):ctx.moveTo(x,y)});ctx.stroke();ctx.fillStyle='#536178';ctx.fillText(`${lo.toFixed(1)} bp`,2,pad);ctx.fillText(`${hi.toFixed(1)} bp`,2,h-pad);canvas.onmousemove=e=>{const rect=canvas.getBoundingClientRect(),x=e.clientX-rect.left-pad,index=Math.max(0,Math.min(points.length-1,Math.round(x/pw*Math.max(1,points.length-1)))),p=points[index];hover.textContent=`${run.run_id} · ${instrument} · ts ${p.ts} · ${p.raw} ${run.settlement_currency} raw · ${p.bps} bp`};canvas.onmouseleave=()=>{hover.textContent='移动鼠标查看时间、贡献 raw 与基点'}}runSelect.onchange=draw;instrumentSelect.onchange=draw;window.addEventListener('resize',draw);draw()}"#;
    let template = template.replace(
        "function renderEvents(){",
        r#"function renderDiagnostics(){const items=cmp?[]:(r.market_strategy_diagnostics||[]);if(!items.length)return;document.querySelector('#strategyDiagnostics').style.display='block';const rows=items.map(item=>{const d=item.report||{},look=d.lookahead?.status??'—',startup=d.startup?.status??'—',pit=d.data_visibility?.status??'—';return `<tr data-search="${esc([item.instrument,item.strategy_id].join(' ')).toLowerCase()}" data-lookahead="${esc(look)}"><td>${esc(item.instrument)}</td><td><code>${esc(item.strategy_id)}</code></td><td>${esc(d.input_kind??'bar')} · ${esc(d.input_bars??d.input_events??'—')} · ${esc(d.as_of??'—')}</td><td>${esc(look)}</td><td>${esc(startup)}</td><td>${esc(pit)}</td><td><details><summary>查看</summary><pre>${esc(JSON.stringify(d,null,2))}</pre></details></td></tr>`}).join('');document.querySelector('#diagnostics').innerHTML='<tr><th>标的</th><th>策略</th><th>输入覆盖</th><th>未来数据</th><th>预热</th><th>PIT</th><th>明细</th></tr>'+rows;function apply(){const q=document.querySelector('#diagSearch').value.trim().toLowerCase(),s=document.querySelector('#diagLookahead').value;document.querySelectorAll('#diagnostics tr[data-search]').forEach(row=>{row.hidden=!(row.dataset.search.includes(q)&&(!s||row.dataset.lookahead===s))})}document.querySelector('#diagSearch').oninput=apply;document.querySelector('#diagLookahead').onchange=apply}function renderEvents(){"#,
    );
    let template = template.replace(
        "function renderEvents(){",
        &(contribution_script.to_owned() + "function renderEvents(){"),
    );
    let template = template.replace(
        "renderEvents();renderTables();draw();",
        "renderEvents();renderTables();renderDiagnostics();renderMarketContributions();draw();",
    );
    let rendered = template
        .replace("__DATA__", &data)
        .replace("__CMP__", if comparison { "true" } else { "false" });
    Ok(rendered.replace("</main>", &format!("{metric_definition_section}</main>")))
}

pub(super) fn bound_html_series(report: &mut Value) {
    if let Some(equity) = report["equity"].as_array_mut() {
        if equity.len() > 2_000 {
            let stride = equity.len().div_ceil(1_999);
            let last = equity.last().cloned();
            let mut sampled = equity
                .iter()
                .step_by(stride)
                .take(1_999)
                .cloned()
                .collect::<Vec<_>>();
            if let Some(last) = last {
                if sampled.last() != Some(&last) {
                    sampled.push(last);
                }
            }
            *equity = sampled;
        }
    }
    if let Some(fills) = report["fills"].as_array_mut() {
        if fills.len() > 2_000 {
            let recent = fills.split_off(fills.len() - 2_000);
            *fills = recent;
        }
    }
    if let Some(events) = report["events"].as_array_mut() {
        if events.len() > 2_000 {
            let recent = events.split_off(events.len() - 2_000);
            *events = recent;
        }
    }
}
