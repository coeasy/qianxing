//! 同输入的多轮回测横向报告；只读取已复核 summary，不重算任何指标。

use crate::{escape_html, report_readout::summary_text, report_readout::READOUT_ABSENT};
pub(crate) use qx_app::report_analysis::ComparisonEntry;
use qx_spec::RunEvidenceFidelity;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub(crate) fn comparison_entry(
    summary_path: &Path,
    summary: &Value,
) -> Result<ComparisonEntry, String> {
    let manifest_path = summary_text(summary, "/run_manifest")
        .ok_or_else(|| format!("对比摘要缺少 run_manifest: {}", summary_path.display()))?;
    let manifest_payload = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("读取对比 RunManifest 失败 {manifest_path}: {error}"))?;
    let manifest = qx_core::RunManifest::from_json(&manifest_payload)?;
    qx_app::report_analysis::comparison_entry_from_summary(summary_path, summary, manifest.run_id)
}

pub(crate) fn rank_comparison(entries: &mut [ComparisonEntry]) {
    qx_app::report_analysis::rank_comparison_by_return(entries);
}

pub(crate) fn write_comparison_html(
    base_summary: &Path,
    instrument: &str,
    data_fingerprint: &str,
    entries: &[ComparisonEntry],
    equity_series: &[Value],
    output: Option<&Path>,
) -> Result<PathBuf, String> {
    let stem = base_summary
        .file_name()
        .and_then(|value| value.to_str())
        .and_then(|value| value.strip_suffix(".summary.json"))
        .ok_or_else(|| format!("比较基准不是 *.summary.json: {}", base_summary.display()))?;
    let output_path = output.map_or_else(
        || base_summary.with_file_name(format!("{stem}.comparison.html")),
        Path::to_path_buf,
    );
    if !output_path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("html") || value.eq_ignore_ascii_case("htm")
        })
    {
        return Err(format!(
            "比较报告输出路径必须以 .html 或 .htm 结尾: {}",
            output_path.display()
        ));
    }
    let parent = output_path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建比较报告目录失败 {}: {error}", parent.display()))?;
    let mut html = String::from(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>牵星回测对照报告</title><style>",
    );
    html.push_str(STYLE);
    html.push_str("</style></head><body><main><h1>牵星回测横向对照</h1><p class=\"sub\">标的 <b>");
    html.push_str(&escape_html(instrument));
    html.push_str("</b> · 数据指纹 <code>");
    html.push_str(&escape_html(data_fingerprint));
    html.push_str("</code></p><p class=\"note\">排名按收益率降序、最大回撤升序、运行摘要名升序；每个指标直接读取对应 v5/v6/v7 summary。可选基准指标按同一份 benchmark BarFrame 与每轮权益曲线逐时间戳精确对齐，基准指纹、价格口径、对齐与周期口径在表中列明。可信度分类列展示字段来源类别和 JSON Pointer，不代表数据或模型已独立认证。缺失指标显示 ");
    html.push_str(READOUT_ABSENT);
    html.push_str("，不按零参与比较。下面的筛选、排序均在本地浏览器执行，不发送回测数据。</p><section class=\"toolbar\"><label>搜索运行/策略 <input id=\"qx-search\" type=\"search\" placeholder=\"输入关键字\"></label><label>策略 <select id=\"qx-strategy\"><option value=\"\">全部策略</option>");
    let mut strategies = entries
        .iter()
        .filter_map(|entry| entry.strategy_id.as_deref())
        .collect::<Vec<_>>();
    strategies.sort_unstable();
    strategies.dedup();
    for strategy in strategies {
        html.push_str("<option value=\"");
        html.push_str(&escape_html(strategy));
        html.push_str("\">");
        html.push_str(&escape_html(strategy));
        html.push_str("</option>");
    }
    html.push_str("</select></label><label>收益率至少 <input id=\"qx-min-return\" type=\"number\" step=\"0.01\" value=\"\" placeholder=\"%\"></label><span id=\"qx-visible-count\" aria-live=\"polite\"></span></section><div class=\"scroll\"><table id=\"qx-runs\"><thead><tr><th>原始收益排名</th><th>主动收益排名</th><th>运行</th><th>策略</th><th>来源分类</th><th>信号参数</th><th>模型假设</th><th>撮合模型</th><th>风控规则</th><th>费用来源</th><th data-sort=\"number\">收益率 ↕</th><th data-sort=\"number\">最大回撤 ↕</th><th>基准标的</th><th>基准指纹</th><th>价格口径</th><th>对齐口径</th><th>期间口径</th><th>基准收益</th><th>主动收益</th><th>平均主动收益</th><th>跟踪误差</th><th>信息比率</th><th>夏普</th><th>索提诺</th><th>卡玛</th><th>胜率</th><th>收益因子</th><th>手续费</th><th>换手</th><th>期末权益</th><th>成交数</th><th>结果哈希</th></tr></thead><tbody>");
    for entry in entries {
        html.push_str("<tr data-strategy=\"");
        html.push_str(&escape_html(entry.strategy_id.as_deref().unwrap_or("")));
        html.push_str("\" data-return-bps=\"");
        html.push_str(&entry.return_bps.to_string());
        html.push_str("\" data-search=\"");
        html.push_str(&escape_html(&format!(
            "{} {}",
            entry.run_id,
            entry.strategy_id.as_deref().unwrap_or("")
        )));
        html.push_str("\"><td>");
        html.push_str(&entry.rank.to_string());
        html.push_str("</td><td>");
        html.push_str(
            &entry
                .benchmark_rank
                .map_or_else(|| READOUT_ABSENT.into(), |rank| rank.to_string()),
        );
        html.push_str("</td><td><code>");
        html.push_str(&escape_html(&entry.run_id));
        html.push_str("</code></td><td>");
        html.push_str(&escaped_option(entry.strategy_id.as_deref()));
        html.push_str("</td><td>");
        html.push_str(&format_fidelity(&entry.fidelity));
        html.push_str("</td><td>");
        html.push_str(&format_document(entry.signal.as_ref()));
        html.push_str("</td><td>");
        html.push_str(&format_document(entry.assumptions.as_ref()));
        html.push_str("</td><td>");
        html.push_str(&format_document(entry.model_descriptors.as_ref()));
        html.push_str("</td><td>");
        html.push_str(&format_risk_rule(entry));
        html.push_str("</td><td>");
        html.push_str(&escaped_option(entry.execution_cost_source.as_deref()));
        html.push_str("</td><td class=\"positive\">");
        html.push_str(&format_bps(entry.return_bps));
        html.push_str("</td><td>");
        html.push_str(&format_bps(entry.max_drawdown_bps));
        html.push_str("</td><td>");
        html.push_str(&escaped_option(
            entry
                .benchmark
                .as_ref()
                .map(|value| value.instrument.as_str()),
        ));
        html.push_str("</td><td><code>");
        html.push_str(&escaped_option(
            entry
                .benchmark
                .as_ref()
                .map(|value| value.fingerprint.as_str()),
        ));
        html.push_str("</code></td><td>");
        html.push_str(&escaped_option(
            entry
                .benchmark
                .as_ref()
                .map(|value| value.price_basis.as_str()),
        ));
        html.push_str("</td><td>");
        html.push_str(&escaped_option(
            entry
                .benchmark
                .as_ref()
                .map(|value| value.alignment.as_str()),
        ));
        html.push_str("</td><td>");
        html.push_str(&escaped_option(
            entry
                .benchmark
                .as_ref()
                .map(|value| value.period_basis.as_str()),
        ));
        html.push_str("</td><td>");
        html.push_str(&format_optional_bps(
            entry
                .benchmark
                .as_ref()
                .and_then(|value| value.benchmark_return_bps),
        ));
        html.push_str("</td><td>");
        html.push_str(&format_optional_bps(
            entry
                .benchmark
                .as_ref()
                .and_then(|value| value.active_return_bps),
        ));
        html.push_str("</td><td>");
        html.push_str(&format_optional_bps(
            entry
                .benchmark
                .as_ref()
                .and_then(|value| value.mean_active_return_bps),
        ));
        html.push_str("</td><td>");
        html.push_str(&format_optional_bps(
            entry
                .benchmark
                .as_ref()
                .and_then(|value| value.tracking_error_bps),
        ));
        html.push_str("</td><td>");
        html.push_str(&format_ratio(
            entry
                .benchmark
                .as_ref()
                .and_then(|value| value.information_ratio),
        ));
        html.push_str("</td><td>");
        html.push_str(&format_ratio(entry.sharpe));
        html.push_str("</td><td>");
        html.push_str(&format_ratio(entry.sortino));
        html.push_str("</td><td>");
        html.push_str(&format_ratio(entry.calmar));
        html.push_str("</td><td>");
        html.push_str(&format_ratio(entry.win_rate));
        html.push_str("</td><td>");
        html.push_str(&format_ratio(entry.profit_factor));
        html.push_str("</td><td>");
        html.push_str(&format_money(entry.fees_raw));
        html.push_str("</td><td>");
        html.push_str(&format_money(entry.turnover_raw));
        html.push_str("</td><td>");
        html.push_str(&format_money(entry.final_equity_raw));
        html.push_str("</td><td>");
        html.push_str(&format_integer(entry.fills));
        html.push_str("</td><td><code>");
        html.push_str(&escaped_option(entry.result_hash.as_deref()));
        html.push_str("</code></td></tr>");
    }
    html.push_str("</tbody></table></div><p class=\"empty\" id=\"qx-empty\" hidden>没有符合条件的运行。</p><footer>牵星 Qianxing · 离线报告 · 不连接真实交易所 · 历史回测结果不构成投资建议</footer></main><script>\nconst table=document.getElementById('qx-runs'),body=table.tBodies[0],rows=[...body.rows],search=document.getElementById('qx-search'),strategy=document.getElementById('qx-strategy'),minimum=document.getElementById('qx-min-return'),count=document.getElementById('qx-visible-count'),empty=document.getElementById('qx-empty');\nfunction update(){let visible=0;for(const row of rows){const matchesText=row.dataset.search.toLocaleLowerCase().includes(search.value.trim().toLocaleLowerCase());const matchesStrategy=!strategy.value||row.dataset.strategy===strategy.value;const threshold=minimum.value===''?-Infinity:Number(minimum.value)*100;const matchesReturn=Number(row.dataset.returnBps)>=threshold;row.hidden=!(matchesText&&matchesStrategy&&matchesReturn);if(!row.hidden)visible++;}count.textContent='显示 '+visible+' / '+rows.length+' 组';empty.hidden=visible!==0;}\nfor(const control of [search,strategy,minimum])control.addEventListener('input',update);strategy.addEventListener('change',update);for(const header of table.tHead.rows[0].cells){if(header.dataset.sort!=='number')continue;header.tabIndex=0;header.setAttribute('role','button');header.addEventListener('click',()=>{const column=header.cellIndex;const direction=header.dataset.direction==='asc'?'desc':'asc';header.dataset.direction=direction;rows.sort((a,b)=>{const av=Number(a.cells[column].textContent.replace(/[^0-9+.-]/g,''));const bv=Number(b.cells[column].textContent.replace(/[^0-9+.-]/g,''));return (av-bv)*(direction==='asc'?1:-1);});for(const row of rows)body.appendChild(row);});header.addEventListener('keydown',event=>{if(event.key==='Enter'||event.key===' '){event.preventDefault();header.click();}});}update();\n</script></body></html>\n");
    let chart_data = serde_json::to_string(equity_series)
        .map_err(|error| format!("编码权益对比曲线失败: {error}"))?
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    html = html.replace(
        "<div class=\"scroll\"><table id=\"qx-runs\">",
        "<section class=\"equity-panel\"><h2>权益曲线对比</h2><p>按时间戳对齐并以各自首个权益点归一化；每条曲线最多显示 2,000 个抽样点。原始完整序列仍保留在权益 CSV 中。</p><div id=\"qx-equity-legend\" class=\"equity-legend\"></div><div class=\"equity-chart-wrap\"><canvas id=\"qx-equity-chart\" aria-label=\"多轮回测归一化权益曲线\" role=\"img\"></canvas><div id=\"qx-equity-tooltip\" class=\"equity-tooltip\" hidden></div></div></section><div class=\"scroll\"><table id=\"qx-runs\">",
    );
    html = html.replace(
        "</style>",
        ".equity-panel{margin:20px 0;padding:16px;border:1px solid #e5e7eb;border-radius:8px}.equity-panel h2{margin:0 0 6px}.equity-panel p{color:#5f6368}.equity-legend{display:flex;gap:12px;flex-wrap:wrap;margin:12px 0}.equity-legend label{display:flex;align-items:center;gap:6px}.equity-legend span{width:12px;height:12px;border-radius:50%}.equity-chart-wrap{position:relative;width:100%;height:320px}#qx-equity-chart{width:100%;height:320px;display:block}.equity-tooltip{position:absolute;max-width:280px;padding:8px;background:#202124;color:white;border-radius:6px;font-size:12px;pointer-events:none;z-index:2}button{padding:8px 12px;border:1px solid #ccd2da;border-radius:6px;background:white;font:inherit;cursor:pointer}</style>",
    );
    let chart_script = format!(
        r#"<script>
const equityData={chart_data},canvas=document.getElementById('qx-equity-chart'),ctx=canvas.getContext('2d'),legend=document.getElementById('qx-equity-legend'),tip=document.getElementById('qx-equity-tooltip');for(const item of equityData)item.points=item.points.map(point=>[point.ts,point.equity_raw]);
function nearestPoint(points,ts){{let lo=0,hi=points.length;while(lo<hi){{const mid=(lo+hi)>>1;if(Number(points[mid][0])<ts)lo=mid+1;else hi=mid;}}if(lo===0)return points[0];if(lo>=points.length)return points[points.length-1];const left=points[lo-1],right=points[lo];return Math.abs(Number(left[0])-ts)<=Math.abs(Number(right[0])-ts)?left:right;}}
const colors=['#2563eb','#dc2626','#059669','#d97706','#7c3aed','#0891b2','#be123c','#4d7c0f'];
for(let i=0;i<equityData.length;i++){{const item=equityData[i],label=document.createElement('label'),toggle=document.createElement('input'),swatch=document.createElement('span');toggle.type='checkbox';toggle.checked=item.available&&item.points.length>0;toggle.dataset.index=i;toggle.addEventListener('change',draw);swatch.style.background=colors[i%colors.length];label.append(toggle,swatch,document.createTextNode(item.run_id));legend.append(label);}}
function draw(){{const rect=canvas.getBoundingClientRect(),scale=window.devicePixelRatio||1;canvas.width=Math.max(1,Math.floor(rect.width*scale));canvas.height=Math.max(1,Math.floor(320*scale));ctx.setTransform(scale,0,0,scale,0,0);ctx.clearRect(0,0,rect.width,320);const selected=[...legend.querySelectorAll('input:checked')].map(input=>equityData[Number(input.dataset.index)]).filter(item=>item&&item.points.length>1);if(!selected.length){{ctx.fillStyle='#667085';ctx.fillText('没有可绘制的权益曲线',18,32);return;}}const allTs=selected.flatMap(item=>item.points.map(point=>point[0])),allValues=selected.flatMap(item=>{{const start=BigInt(item.points[0][1]);return item.points.map(point=>Number((BigInt(point[1])-start)*1000000n/start)/10000);}}),minTs=Math.min(...allTs),maxTs=Math.max(...allTs),minY=Math.min(0,...allValues),maxY=Math.max(0,...allValues),pad={{left:58,right:16,top:18,bottom:30}},width=rect.width-pad.left-pad.right,height=320-pad.top-pad.bottom,x=ts=>pad.left+(ts-minTs)/Math.max(1,maxTs-minTs)*width,y=value=>pad.top+(maxY-value)/Math.max(0.0001,maxY-minY)*height;ctx.strokeStyle='#d0d5dd';ctx.lineWidth=1;ctx.beginPath();ctx.moveTo(pad.left,pad.top);ctx.lineTo(pad.left,320-pad.bottom);ctx.lineTo(rect.width-pad.right,320-pad.bottom);ctx.stroke();for(let grid=0;grid<=4;grid++){{const value=minY+(maxY-minY)*grid/4,yy=y(value);ctx.fillStyle='#667085';ctx.font='11px system-ui';ctx.fillText(value.toFixed(2)+'%',5,yy+4);ctx.strokeStyle='#eaecf0';ctx.beginPath();ctx.moveTo(pad.left,yy);ctx.lineTo(rect.width-pad.right,yy);ctx.stroke();}}for(const item of selected){{const start=BigInt(item.points[0][1]);ctx.strokeStyle=colors[equityData.indexOf(item)%colors.length];ctx.lineWidth=1.8;ctx.beginPath();item.points.forEach((point,index)=>{{const value=Number((BigInt(point[1])-start)*1000000n/start)/10000,indexX=x(point[0]),indexY=y(value);if(index===0)ctx.moveTo(indexX,indexY);else ctx.lineTo(indexX,indexY);}});ctx.stroke();}}canvas._chart={{selected,x,y,pad,minTs,maxTs}};}}
canvas.addEventListener('pointermove',event=>{{const chart=canvas._chart;if(!chart)return;const rect=canvas.getBoundingClientRect(),px=event.clientX-rect.left;let nearest=chart.minTs+(px-chart.pad.left)/(rect.width-chart.pad.left-chart.pad.right)*(chart.maxTs-chart.minTs);nearest=Math.max(chart.minTs,Math.min(chart.maxTs,nearest));const rows=chart.selected.map(item=>{{const point=nearestPoint(item.points,nearest),start=BigInt(item.points[0][1]),pct=Number((BigInt(point[1])-start)*1000000n/start)/10000;return item.run_id+': '+pct.toFixed(2)+'%';}});tip.textContent=new Date(nearest).toLocaleString()+' · '+rows.join(' · ');tip.hidden=false;tip.style.left=Math.max(0,Math.min(rect.width-280,px+12))+'px';tip.style.top='8px';}});canvas.addEventListener('pointerleave',()=>tip.hidden=true);window.addEventListener('resize',draw);draw();
</script>"#
    );
    html = html.replace("</body>", &format!("{chart_script}</body>"));
    qx_app::artifact_store::sink::publish(
        &output_path,
        &html,
        qx_app::artifact_store::sink::PublishMode::Overwrite,
    )
    .map_err(|error| format!("发布比较报告失败 {}: {error}", output_path.display()))?;
    Ok(output_path)
}

fn escaped_option(value: Option<&str>) -> String {
    value.map_or_else(|| READOUT_ABSENT.into(), escape_html)
}

fn format_bps(value: i128) -> String {
    let magnitude = value.unsigned_abs();
    let sign = if value < 0 { "-" } else { "+" };
    format!("{sign}{}.{:02}%", magnitude / 100, magnitude % 100)
}

fn format_ratio(value: Option<f64>) -> String {
    value.map_or_else(|| READOUT_ABSENT.into(), |value| format!("{value:.3}"))
}

fn format_optional_bps(value: Option<f64>) -> String {
    value.map_or_else(|| READOUT_ABSENT.into(), |value| format!("{value:+.2} bps"))
}

fn format_money(value: Option<i128>) -> String {
    value.map_or_else(|| READOUT_ABSENT.into(), crate::report_svg::money)
}

fn format_integer(value: Option<i128>) -> String {
    value.map_or_else(|| READOUT_ABSENT.into(), |value| value.to_string())
}

fn format_document(value: Option<&Value>) -> String {
    value.map_or_else(
        || READOUT_ABSENT.into(),
        |value| {
            format!(
                "<details><summary>查看</summary><code>{}</code></details>",
                escape_html(&value.to_string())
            )
        },
    )
}

fn format_fidelity(fidelity: &RunEvidenceFidelity) -> String {
    let groups = [
        ("观测", &fidelity.observed),
        ("配置", &fidelity.configured),
        ("估计", &fidelity.estimated),
        ("计算", &fidelity.computed),
        ("缺席", &fidelity.absent),
    ];
    let summary = groups
        .iter()
        .map(|(label, paths)| format!("{label} {} 项", paths.len()))
        .collect::<Vec<_>>()
        .join(" · ");
    let mut details = String::from("<details><summary>");
    details.push_str(&escape_html(&format!(
        "{} 工件 · {summary}",
        fidelity.source_artifact
    )));
    details.push_str("</summary>");
    for (label, paths) in groups {
        details.push_str("<p><b>");
        details.push_str(&escape_html(label));
        details.push_str("</b>: <code>");
        details.push_str(&escape_html(&paths.join(" · ")));
        details.push_str("</code></p>");
    }
    details.push_str("</details>");
    details
}

fn format_risk_rule(entry: &ComparisonEntry) -> String {
    match (
        entry.risk_rule_set_version.as_deref(),
        entry.risk_source.as_deref(),
    ) {
        (Some(version), Some(source)) => escape_html(&format!("{version} ({source})")),
        (Some(version), None) => escape_html(version),
        (None, Some(source)) => escape_html(source),
        (None, None) => READOUT_ABSENT.into(),
    }
}

const STYLE: &str = "body{margin:0;background:#f5f7fa;color:#202124;font:14px system-ui,'Microsoft YaHei',sans-serif}main{max-width:1500px;margin:32px auto;padding:24px;background:white;border-radius:12px;box-shadow:0 3px 18px #0001}h1{margin:0 0 8px}.sub,.note,footer{color:#5f6368}.toolbar{display:flex;gap:16px;align-items:end;flex-wrap:wrap;margin:18px 0}.toolbar label{display:grid;gap:6px;color:#5f6368}.toolbar input,.toolbar select{padding:8px;border:1px solid #ccd2da;border-radius:6px;font:inherit}.scroll{overflow:auto;max-height:72vh}table{border-collapse:collapse;width:100%;white-space:nowrap;margin-top:8px}th,td{padding:10px 12px;border-bottom:1px solid #e5e7eb;text-align:right}th{background:#f3f5f8;position:sticky;top:0}th[data-sort]{cursor:pointer}th:nth-child(3),th:nth-child(4),td:nth-child(3),td:nth-child(4){text-align:left}.positive{color:#c0392b}code{font-family:ui-monospace,Consolas,monospace}footer{margin-top:22px}.empty{padding:24px;text-align:center;color:#5f6368}";

#[cfg(test)]
mod interactive_report_tests {
    use super::write_comparison_html;
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn comparison_chart_hover_uses_binary_search_over_timestamp_sorted_points() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let dir = workspace.join("target").join(format!(
            "qx-comparison-chart-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("comparison.html");
        write_comparison_html(
            &dir.join("run.summary.json"),
            "BTCUSDT.BINANCE",
            "data-fingerprint",
            &[],
            &[json!({"run_id":"run-1","points":[{"ts":1,"equity_raw":"100"},{"ts":2,"equity_raw":"101"}],"available":true})],
            Some(&output),
        )
        .unwrap();
        let html = std::fs::read_to_string(&output).unwrap();
        assert!(html.contains("function nearestPoint(points,ts)"));
        assert!(html.contains("nearestPoint(item.points,nearest)"));
        assert!(!html.contains("item.points.reduce((best,current)"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
