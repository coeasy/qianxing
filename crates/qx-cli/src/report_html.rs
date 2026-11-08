//! 自包含 HTML 结果报告（易用性 P3 / `docs/archive/竞品对比与易用性改进优化计划-v1.md` §6 P3）。
//!
//! 一份能双击打开的 HTML：指标卡 + 三张内嵌 SVG + 产物身份表 + 「未连接真实交易所」声明。
//! 竞品默认交付一张标注买卖点的图或一份交互式 HTML，我们此前只落 `*.equity.csv` /
//! `*.fills.csv` / `summary.json`，把「读结果」整段留给了用户（竞品对比 §1「看不见」）。
//!
//! 两条纪律：
//! - **不新增第二个指标算式来源**：卡片上的每个数都从 `summary.json` 已有的格子读
//!   （`metrics.*` / `account.*` / `fills` / `rejected_orders`），曲线与成交点只从
//!   `*.equity.csv` / `*.fills.csv` 派生，月度收益由净值序列按自然月归并。
//! - **确定性**：不用墙钟、不用随机、不遍历 HashMap，同输入两次生成逐字节相等。
//!
//! 产物无任何外部资源引用（内联 SVG 不带 `xmlns`、无 `http`、无外链 CSS/JS），因此可以
//! 离线双击打开、可以进版本库、可以被门禁按「出现 http 即判红」钉住。

use crate::build_identity;
use crate::report_credibility::credibility_panel;
pub(crate) use crate::report_equity_csv::{read_equity_analysis_csv, read_equity_csv};
use crate::report_fidelity::fidelity_panel;
use crate::report_output_paths::report_artifact_paths;
use crate::report_readout::{summary_number, summary_text, READOUT_ABSENT};
use crate::report_svg::{equity_svg, fills_svg, monthly_svg, year_month};
use qx_core::Event;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

mod fills;
use fills::fills_table;
pub(crate) use fills::{read_fills_csv, read_fills_csv_tail, FillPoint};

#[derive(Debug)]
pub(crate) struct ReportArtifacts {
    pub(crate) html: PathBuf,
    pub(crate) equity_svg: PathBuf,
    pub(crate) fills_svg: PathBuf,
    pub(crate) monthly_svg: PathBuf,
}

const MAX_HTML_FILL_ROWS: usize = 5_000;

/// 写自包含 HTML 与三张独立 SVG；默认均与摘要同目录、同前缀。
pub(crate) fn write_report_html(
    summary_path: &Path,
    summary: &serde_json::Value,
    output: Option<&Path>,
) -> Result<ReportArtifacts, String> {
    let (artifacts, bundle) = prepare_report_html_bundle(summary_path, summary, output)?;
    qx_app::artifact_store::sink::publish_bundle(&bundle)
        .map_err(|error| format!("发布 HTML 报告 bundle 失败: {error}"))?;
    Ok(artifacts)
}

pub(crate) fn prepare_report_html_bundle(
    summary_path: &Path,
    summary: &serde_json::Value,
    output: Option<&Path>,
) -> Result<(ReportArtifacts, Vec<(PathBuf, Vec<u8>)>), String> {
    let paths = report_artifact_paths(summary_path)?;
    let equity_analysis = if paths.equity_csv.is_file() {
        read_equity_analysis_csv(&paths.equity_csv)?
    } else {
        Vec::new()
    };
    let equity: Vec<(i64, i128)> = equity_analysis
        .iter()
        .map(|(ts, value, _)| (*ts, *value))
        .collect();
    let (fills, fill_total) = if paths.fills_csv.is_file() {
        read_fills_csv_tail(&paths.fills_csv, MAX_HTML_FILL_ROWS)?
    } else {
        (Vec::new(), 0)
    };
    let event_trace = if paths.event_trace.is_file() {
        Some(read_event_trace(&paths.event_trace, summary)?)
    } else {
        None
    };
    let out = output.map_or_else(|| paths.html.clone(), Path::to_path_buf);
    if !out
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"))
    {
        return Err(format!(
            "HTML 报告输出路径必须以 .html 或 .htm 结尾: {}",
            out.display()
        ));
    }
    let asset_stem = if output.is_some() {
        out.file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "HTML 输出文件名无效".to_string())?
    } else {
        paths
            .html
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "默认 HTML 报告文件名无效".to_string())?
    };
    let parent = out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let equity_values: Vec<i128> = equity.iter().map(|(_, value)| *value).collect();
    let fill_values: Vec<(u64, i128, i128)> = fills
        .iter()
        .map(|fill| (fill.ts, fill.qty_raw, fill.price_raw))
        .collect();
    let monthly = monthly_series(summary, &equity);
    let artifacts = ReportArtifacts {
        html: out.clone(),
        equity_svg: parent.join(format!("{asset_stem}.equity.svg")),
        fills_svg: parent.join(format!("{asset_stem}.fills.svg")),
        monthly_svg: parent.join(format!("{asset_stem}.monthly.svg")),
    };
    let equity_svg_content = equity_svg(&equity_values);
    let fills_svg_content = fills_svg(&fill_values);
    let monthly_svg_content = monthly_svg(&monthly);
    let event_trace_link = if event_trace.is_some() && paths.event_trace.parent() == out.parent() {
        paths
            .event_trace
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
    } else {
        None
    };
    let html = render_report_html_with_trace(
        summary,
        &equity,
        &equity_analysis,
        &fills,
        fill_total,
        &monthly,
        event_trace.as_deref(),
        event_trace_link.as_deref(),
    );
    let bundle = vec![
        (
            artifacts.equity_svg.clone(),
            equity_svg_content.into_bytes(),
        ),
        (artifacts.fills_svg.clone(), fills_svg_content.into_bytes()),
        (
            artifacts.monthly_svg.clone(),
            monthly_svg_content.into_bytes(),
        ),
        (artifacts.html.clone(), html.into_bytes()),
    ];
    Ok((artifacts, bundle))
}

/// 从三份产物渲染 HTML（纯函数，不经文件系统）。
///
/// 月度收益由调用方传入：落盘路径（[`write_report_html`]）既要用它画 SVG、又要用它排版
/// HTML 表格，算一次再传进来，两个消费点就共用同一份切片，不会各自漂移。`monthly` 的唯一
/// 算式入口是 [`monthly_series`]。
#[cfg(test)]
pub(crate) fn render_report_html(
    summary: &serde_json::Value,
    equity: &[(i64, i128)],
    fills: &[FillPoint],
    monthly: &[(i32, u32, i64)],
) -> String {
    let analysis = equity
        .iter()
        .map(|(ts, value)| (*ts, *value, None))
        .collect::<Vec<_>>();
    render_report_html_with_trace(
        summary,
        equity,
        &analysis,
        fills,
        fills.len() as u64,
        monthly,
        None,
        None,
    )
}

// The report renderer receives each precomputed section independently to keep rendering pure.
#[allow(clippy::too_many_arguments)]
fn render_report_html_with_trace(
    summary: &serde_json::Value,
    equity: &[(i64, i128)],
    equity_analysis: &[(i64, i128, Option<i128>)],
    fills: &[FillPoint],
    fill_total: u64,
    monthly: &[(i32, u32, i64)],
    event_trace: Option<&[Event]>,
    event_trace_link: Option<&str>,
) -> String {
    let text = |pointer: &str| {
        escape_html(&summary_text(summary, pointer).unwrap_or_else(|| READOUT_ABSENT.to_string()))
    };
    let number = |pointer: &str| summary_number(summary, pointer);
    let pct = |raw: Option<i128>| {
        raw.map_or_else(
            || READOUT_ABSENT.to_string(),
            |bps| format!("{:+.2}%", bps as f64 / 100.0),
        )
    };
    let count = |raw: Option<i128>| {
        raw.map_or_else(|| READOUT_ABSENT.to_string(), |value| value.to_string())
    };

    let return_bps = number("/metrics/return_bps");
    let drawdown = number("/metrics/max_drawdown_bps");
    let mut cards = String::new();
    cards.push_str(&card("收益率", &pct(return_bps), tone_of(return_bps)));
    // 回撤是「幅度」不是「方向」：不带正号；为 0 时是中性的灰，不染成绿（否则「没回撤」看着像跌）。
    let drawdown_text = drawdown.map_or_else(
        || READOUT_ABSENT.to_string(),
        |bps| format!("{:.2}%", bps as f64 / 100.0),
    );
    let drawdown_tone = if drawdown.is_some_and(|bps| bps > 0) {
        "down"
    } else {
        "neutral"
    };
    cards.push_str(&card("最大回撤", &drawdown_text, drawdown_tone));
    cards.push_str(&card(
        "期末权益",
        &details::amount(
            summary,
            "/metrics_exact/equity/final",
            "/metrics/final_equity_raw",
        ),
        "neutral",
    ));
    cards.push_str(&card(
        "期初本金",
        &details::amount(
            summary,
            "/metrics_exact/equity/initial",
            "/account/initial_cash_raw",
        ),
        "neutral",
    ));
    cards.push_str(&card("手续费", &details::fees(summary), "neutral"));
    cards.push_str(&card(
        "换手",
        &details::amount(
            summary,
            "/metrics_exact/turnover/amount",
            "/metrics/turnover_raw",
        ),
        "neutral",
    ));
    cards.push_str(&card("成交笔数", &count(number("/fills")), "neutral"));
    cards.push_str(&card(
        "拒单数",
        &count(number("/rejected_orders")),
        "neutral",
    ));

    // 风险调整收益比率卡（P2/P5）：全部从摘要 `metrics.*` 读，与落盘是同一组数，不重算。
    // 缺格（分母为 0 / 样本不足）印 `—` 而非 0，沿用摘要"没算过 ≠ 算出来是零"的口径。
    let ratio = |key: &str| -> Option<f64> {
        summary
            .pointer(&format!("/metrics/{key}"))
            .and_then(serde_json::Value::as_f64)
    };
    let ratio_card = |label: &str, key: &str| {
        let value = ratio(key);
        let tone = if value.is_some_and(|value| value > 0.0) {
            "up"
        } else if value.is_some_and(|value| value < 0.0) {
            "down"
        } else {
            "neutral"
        };
        let text = value.map_or_else(|| "—".to_string(), |value| format!("{value:.2}"));
        card(label, &text, tone)
    };
    let mut risk_cards = String::new();
    risk_cards.push_str(&ratio_card("夏普(每周期)", "sharpe"));
    risk_cards.push_str(&ratio_card("索提诺", "sortino"));
    risk_cards.push_str(&ratio_card("卡玛", "calmar"));
    risk_cards.push_str(&ratio_card("盈利周期占比", "win_rate"));
    risk_cards.push_str(&ratio_card("收益因子", "profit_factor"));
    let equity_series: Vec<i128> = equity.iter().map(|(_, value)| *value).collect();
    let fills_points: Vec<(u64, i128, i128)> = fills
        .iter()
        .map(|fill| (fill.ts, fill.qty_raw, fill.price_raw))
        .collect();

    let provenance = provenance_table(&text);
    let strategy = text("/strategy_id");
    let instrument = text("/instrument");
    let result_hash = text("/result_hash");
    let build = build_identity::RUNTIME_VERSION;

    let mut html = String::new();
    html.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    let _ = writeln!(
        html,
        "<title>牵星回测报告 — {strategy} @ {instrument}</title>"
    );
    html.push_str("<style>\n");
    html.push_str(STYLE);
    html.push_str("</style>\n");
    html.push_str("</head>\n<body>\n<main>\n");
    let _ = writeln!(
        html,
        "<header><h1>牵星回测报告</h1>\
         <p class=\"sub\">策略 <b>{strategy}</b> · 标的 <b>{instrument}</b> · \
         结果哈希 <code>{result_hash}</code> · 构建 <code>{build}</code></p></header>"
    );
    html.push_str(
        "<p class=\"disclaimer\">本报告由本地回测引擎产出，<b>未连接真实交易所、未发送任何订单</b>；\
         结果基于历史数据，不构成投资建议。</p>\n",
    );
    html.push_str("<nav class=\"report-nav\" aria-label=\"报告导航\"><a href=\"#report-overview\">运行概览</a><a href=\"#report-configuration\">策略与账户</a><a href=\"#report-charts\">图表</a><a href=\"#report-fills\">成交</a><a href=\"#report-equity\">权益明细</a><a href=\"#report-provenance\">数据追溯</a><a href=\"#report-source\">摘要下载</a></nav>");
    html.push_str(&details::overview(summary));
    let _ = writeln!(html, "<section class=\"cards\">{cards}</section>");
    let _ = writeln!(
        html,
        "<section class=\"cards\" aria-label=\"风险调整收益比率\">{risk_cards}</section>"
    );
    if let Some(definition_version) = summary
        .pointer("/metrics_definition/schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        let ratio_basis = text("/metrics_definition/risk_ratio_period_basis");
        let fee_basis = text("/metrics_definition/fees_exact_basis");
        let _ = writeln!(
            html,
            "<p class=\"metric-definition\">指标口径 v{definition_version}：风险比率按 {ratio_basis}；精确手续费按 {fee_basis} 汇总，raw 手续费仅作兼容投影。</p>"
        );
    }
    if let Some(section) = crate::report_benchmark::benchmark_html_section(summary) {
        html.push_str(&section);
    }
    if let Some(section) = crate::ashare_analysis_html_section(summary) {
        html.push_str(&section);
    }
    html.push_str(&details::configuration(summary));
    html.push_str(&interactive_research_chart(
        equity_analysis,
        fills,
        fill_total,
    ));
    html.push_str("<section class=\"charts\" id=\"report-charts\">\n");
    let _ = writeln!(
        html,
        "<figure><figcaption>权益曲线（含最大回撤区间）</figcaption>{}</figure>",
        equity_svg(&equity_series)
    );
    let _ = writeln!(
        html,
        "<figure><figcaption>成交价格点（方向未包含在成交产物中，故以中性点显示）</figcaption>{}</figure>",
        fills_svg(&fills_points)
    );
    let _ = writeln!(
        html,
        "<figure><figcaption>月度收益（%）</figcaption>{}</figure>",
        monthly_svg(monthly)
    );
    html.push_str("</section>\n");
    let order_event_links = event_trace.map(|events| {
        events
            .iter()
            .skip(events.len().saturating_sub(MAX_INLINE_TRACE_EVENTS))
            .filter_map(|event| {
                let (_, order_id) = event_kind_identity(&event.kind);
                order_id.map(|order_id| (order_id, event.seq))
            })
            .fold(BTreeMap::new(), |mut links, (order_id, seq)| {
                links.entry(order_id).or_insert(seq);
                links
            })
    });
    html.push_str(&details::monthly_table(monthly));
    html.push_str(&details::equity_table(equity_analysis));
    html.push_str(&fills_table(fills, fill_total, order_event_links.as_ref()));
    html.push_str(&execution::execution_attempts_section(summary));
    if let Some(events) = event_trace {
        html.push_str(&event_trace_section(events, event_trace_link));
    }
    let _ = writeln!(
        html,
        "<section class=\"prov\" id=\"report-provenance\"><h2>产物身份</h2>{provenance}</section>"
    );
    html.push_str(&credibility_panel(summary));
    html.push_str(&fidelity_panel(summary));
    html.push_str(&strategy_bias_panel(summary));
    html.push_str(&details::summary_download(summary));
    html.push_str(
        "<footer>牵星 Qianxing · 分级校准，量天定位 · 本页所有数值均取自同名 \
         <code>*.summary.json</code>，曲线与成交点取自 <code>*.equity.csv</code> / \
         <code>*.fills.csv</code>。</footer>\n",
    );
    html.push_str("</main>\n</body>\n</html>\n");
    html
}

pub(crate) const MAX_INTERACTIVE_CHART_POINTS: usize = 2_000;

/// Render a bounded, offline interactive view from the already-verified equity/fill artifacts.
/// The chart only changes display scale; exact raw values remain in the linked tables below.
pub(crate) fn interactive_research_chart(
    equity: &[(i64, i128, Option<i128>)],
    fills: &[FillPoint],
    fill_total: u64,
) -> String {
    let point_indices = sample_indices(equity.len(), MAX_INTERACTIVE_CHART_POINTS);
    let points = point_indices
        .iter()
        .map(|index| {
            let (ts, value, position) = equity[*index];
            serde_json::json!({
                "ts": ts,
                "equity_raw": value.to_string(),
                "position_raw": position.map(|value| value.to_string()),
            })
        })
        .collect::<Vec<_>>();
    let fill_indices = sample_indices(fills.len(), MAX_INTERACTIVE_CHART_POINTS);
    let fill_points = fill_indices
        .iter()
        .map(|index| {
            let fill = &fills[*index];
            serde_json::json!({"ts": fill.ts, "order_id": fill.order_id.to_string()})
        })
        .collect::<Vec<_>>();
    let payload =
        serde_json::to_string(&serde_json::json!({"points": points, "fills": fill_points}))
            .unwrap_or_else(|_| "{\"points\":[],\"fills\":[]}".to_string())
            .replace('<', "\\u003c")
            .replace('>', "\\u003e")
            .replace('&', "\\u0026");
    format!(
        r#"<section class="interactive-research"><h2>联动研究图</h2><p class="chart-note">基于权益 CSV 的最多 {MAX_INTERACTIVE_CHART_POINTS} 个等距样本点，鼠标悬停查看时点、权益、持仓与成交订单；曲线可单独开关。权益和回撤相对起点显示，持仓按本次样本最大绝对持仓归一化。原始明细保留在下方表格。</p><div class="chart-controls"><label><input type="checkbox" data-series="equity" checked> 权益变化</label><label><input type="checkbox" data-series="drawdown" checked> 区间回撤</label><label><input type="checkbox" data-series="position" checked> 持仓变化</label><span id="research-chart-count" aria-live="polite"></span></div><div class="research-chart-wrap"><canvas id="research-chart" role="img" aria-label="联动权益、回撤、持仓和成交的研究图"></canvas><div id="research-chart-tip" hidden></div></div></section><script>const researchData={payload};(function(){{const canvas=document.getElementById('research-chart'),ctx=canvas.getContext('2d'),tip=document.getElementById('research-chart-tip'),count=document.getElementById('research-chart-count'),toggles=[...document.querySelectorAll('[data-series]')],points=researchData.points,fills=researchData.fills;count.textContent='显示 '+points.length+' 个样本点 · 成交标记 '+fills.length+' / 全部成交 '+{fill_total};function nearestPoint(ts){{let lo=0,hi=points.length;while(lo<hi){{const mid=(lo+hi)>>1;if(Number(points[mid].ts)<ts)lo=mid+1;else hi=mid;}}if(lo===0)return 0;if(lo>=points.length)return points.length-1;return Math.abs(Number(points[lo-1].ts)-ts)<=Math.abs(Number(points[lo].ts)-ts)?lo-1:lo;}}const fillIndexes=fills.map(fill=>{{const index=nearestPoint(Number(fill.ts));return{{fill,index}};}}),fillsByPoint=new Map();for(const item of fillIndexes){{const ids=fillsByPoint.get(item.index)||[];ids.push(item.fill.order_id);fillsByPoint.set(item.index,ids);}}function seriesValues(){{if(!points.length)return{{equity:[],drawdown:[],position:[]}};const initial=BigInt(points[0].equity_raw);let peak=initial,maxPosition=0n;const drawdown=[];for(const point of points){{const value=BigInt(point.equity_raw);if(value>peak)peak=value;drawdown.push(Number((value-peak)*1000000n/peak)/10000);if(point.position_raw!==null){{const position=BigInt(point.position_raw),abs=position<0n?-position:position;if(abs>maxPosition)maxPosition=abs;}}}}return{{equity:points.map(point=>Number((BigInt(point.equity_raw)-initial)*1000000n/initial)/10000),drawdown,position:points.map(point=>point.position_raw===null||maxPosition===0n?null:Number(BigInt(point.position_raw)*1000000n/maxPosition)/10000)}};}}function draw(){{const rect=canvas.getBoundingClientRect(),scale=window.devicePixelRatio||1,w=Math.max(1,rect.width),h=340;canvas.width=Math.floor(w*scale);canvas.height=Math.floor(h*scale);ctx.setTransform(scale,0,0,scale,0,0);ctx.clearRect(0,0,w,h);if(points.length<2){{ctx.fillStyle='#667085';ctx.fillText('权益样本不足，无法绘制联动曲线',16,28);return;}}const values=seriesValues(),enabled=toggles.filter(item=>item.checked).map(item=>item.dataset.series),all=enabled.flatMap(key=>values[key].filter(value=>value!==null));if(!all.length){{ctx.fillStyle='#667085';ctx.fillText('请选择至少一条曲线',16,28);return;}}const lo=Math.min(...all,0),hi=Math.max(...all,0),pad={{l:58,r:16,t:14,b:34}},plotW=Math.max(1,w-pad.l-pad.r),plotH=h-pad.t-pad.b,x=i=>pad.l+i/Math.max(1,points.length-1)*plotW,y=value=>pad.t+(hi-value)/Math.max(.0001,hi-lo)*plotH;ctx.strokeStyle='#e5e7eb';ctx.fillStyle='#667085';ctx.font='11px system-ui';for(let tick=0;tick<5;tick++){{const v=hi-(hi-lo)*tick/4,yy=y(v);ctx.beginPath();ctx.moveTo(pad.l,yy);ctx.lineTo(w-pad.r,yy);ctx.stroke();ctx.fillText(v.toFixed(2)+'%',4,yy+4);}}const colors={{equity:'#2563eb',drawdown:'#dc2626',position:'#7c3aed'}};for(const key of enabled){{ctx.beginPath();let started=false;for(let i=0;i<points.length;i++){{const value=values[key][i];if(value===null)continue;if(!started){{ctx.moveTo(x(i),y(value));started=true;}}else ctx.lineTo(x(i),y(value));}}ctx.strokeStyle=colors[key];ctx.lineWidth=1.7;ctx.stroke();}}ctx.fillStyle='#d97706';for(const item of fillIndexes){{const nearest=item.index;if(values.equity[nearest]!==undefined){{ctx.beginPath();ctx.arc(x(nearest),y(values.equity[nearest]),2.7,0,Math.PI*2);ctx.fill();}}}}canvas._chart={{x, y, values, enabled, pad, plotW, plotH}};}}canvas.addEventListener('mousemove',event=>{{if(!points.length||!canvas._chart)return;const rect=canvas.getBoundingClientRect(),chart=canvas._chart,localX=event.clientX-rect.left,index=Math.max(0,Math.min(points.length-1,Math.round((localX-chart.pad.l)/chart.plotW*(points.length-1)))),point=points[index],when=new Date(Number(point.ts)).toLocaleString(),linked=fillsByPoint.get(index)||[];tip.textContent='时间：'+when+'\n权益 raw：'+point.equity_raw+'\n持仓 raw：'+(point.position_raw??'未提供')+'\n权益变化：'+chart.values.equity[index].toFixed(3)+'%\n区间回撤：'+chart.values.drawdown[index].toFixed(3)+'%\n成交订单：'+(linked.length?linked.join(', '):'无');tip.hidden=false;tip.style.left=Math.max(0,Math.min(Math.max(0,rect.width-250),localX+12))+'px';tip.style.top='20px';}});canvas.addEventListener('mouseleave',()=>tip.hidden=true);for(const toggle of toggles)toggle.addEventListener('change',draw);window.addEventListener('resize',draw);if(typeof ResizeObserver!=='undefined')new ResizeObserver(()=>requestAnimationFrame(draw)).observe(canvas.parentElement);draw();}})();</script>"#,
    )
}

pub(crate) fn sample_indices(length: usize, limit: usize) -> Vec<usize> {
    if length <= limit || limit < 2 {
        return (0..length).collect();
    }
    (0..limit)
        .map(|index| index * (length - 1) / (limit - 1))
        .collect()
}

mod details;
mod event_trace;
mod execution;
pub(crate) use event_trace::{event_kind_identity, read_event_trace};
use event_trace::{event_trace_section, MAX_INLINE_TRACE_EVENTS};

mod strategy_bias;
use strategy_bias::strategy_bias_panel;

/// 转义摘要中的文本字段，避免用户控制的标的名/路径被解释成 HTML 或外部资源。
pub(crate) fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// 指标卡的色调：涨红、跌绿、中性墨色。
fn tone_of(value: Option<i128>) -> &'static str {
    match value {
        Some(v) if v > 0 => "up",
        Some(v) if v < 0 => "down",
        _ => "neutral",
    }
}

fn card(label: &str, value: &str, tone: &str) -> String {
    format!(
        "<div class=\"card\"><span class=\"k\">{label}</span><span class=\"v {tone}\">{value}</span></div>"
    )
}

/// 产物身份表：读者要能一眼看出「这份结果跑的是哪份输入、用哪套口径」。
fn provenance_table(text: &dyn Fn(&str) -> String) -> String {
    let rows: [(&str, String); 15] = [
        ("输入种类", text("/input/kind")),
        ("输入路径", text("/input/path")),
        (
            "数据集",
            format!(
                "{} @ {}",
                text("/input/dataset_id"),
                text("/input/dataset_version")
            ),
        ),
        ("输入指纹", text("/input/fingerprint")),
        ("本金来源", text("/account/source")),
        ("撮合内核", text("/matching_kernel")),
        (
            "风控规则",
            format!(
                "{}（{}）",
                text("/risk_rules/rule_set_version"),
                text("/risk_rules/source")
            ),
        ),
        ("成本口径", text("/execution_costs/source")),
        ("重放日志摘要", text("/replay/log_digest")),
        ("重放事件数", text("/replay/events")),
        ("重放账簿条目", text("/replay/ledger_entries")),
        ("输入事件哈希", text("/input_data_hash")),
        ("摘要版本", text("/schema_version")),
        ("运行清单", text("/run_manifest")),
        ("运行记录", text("/run_record")),
    ];
    let mut table = String::from("<table><tbody>");
    for (key, value) in rows {
        let _ = write!(
            table,
            "<tr><th>{key}</th><td><code>{value}</code></td></tr>"
        );
    }
    table.push_str("</tbody></table>");
    table
}

/// 净值序列按自然月归并成 `(年, 月, 收益 bps)`：每月取该月最后一个样本的权益，与上月
/// 末值相比；首月以 `initial_raw`（摘要的期初本金）为基。基为 0 时不给假数，落 0。
/// 月度收益的唯一算式入口：`monthly_returns` 只由这里调用，SVG 与 HTML 表格都吃它的产物。
pub(crate) fn monthly_series(
    summary: &serde_json::Value,
    equity: &[(i64, i128)],
) -> Vec<(i32, u32, i64)> {
    monthly_returns(equity, summary_number(summary, "/account/initial_cash_raw"))
}

fn monthly_returns(equity: &[(i64, i128)], initial_raw: Option<i128>) -> Vec<(i32, u32, i64)> {
    let mut months: Vec<((i32, u32), i128)> = Vec::new();
    for (ts, value) in equity {
        let ym = year_month(*ts);
        match months.last_mut() {
            Some((last, slot)) if *last == ym => *slot = *value,
            _ => months.push((ym, *value)),
        }
    }
    let mut previous = initial_raw
        .or_else(|| equity.first().map(|(_, value)| *value))
        .unwrap_or(0);
    let mut out = Vec::with_capacity(months.len());
    for (ym, value) in months {
        let bps = if previous == 0 {
            0
        } else {
            ((value - previous) as f64 / previous as f64 * 10_000.0).round() as i64
        };
        out.push((ym.0, ym.1, bps));
        previous = value;
    }
    out
}

/// 自包含样式：全部内联，无外链字体/脚本/图片（`http` 出现即被门禁判红）。
const STYLE: &str = include_str!("report_html/report.css");

#[cfg(test)]
mod fill_window_tests {
    use super::{fills_table, read_fills_csv, read_fills_csv_tail};
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn multi_leg_fill_reader_preserves_leg_and_extended_identity() {
        let directory = std::env::current_dir()
            .unwrap()
            .join("target/report-fill-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("multi-leg-{}.csv", std::process::id()));
        std::fs::write(
            &path,
            "leg,order_id,ts,qty_raw,price_raw,fee_raw,account_id,strategy_id,signal_id,intent_id,venue_id,venue_order_id\nprimary,17,1000,200,300,4,main,momentum,5,6,BINANCE,venue-17\n",
        )
        .unwrap();
        let fills = read_fills_csv(&path).unwrap();
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].leg.as_deref(), Some("primary"));
        assert_eq!(fills[0].order_id, 17);
        assert_eq!(fills[0].ts, 1000);
        assert_eq!(fills[0].strategy_id, "momentum");
        assert_eq!(fills[0].venue_order_id.as_deref(), Some("venue-17"));
        let table = fills_table(&fills, 1, None);
        assert!(table.contains("<td>primary</td>"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn exact_fill_reader_preserves_small_decimals_and_quoted_fee_json() {
        let directory = std::env::current_dir()
            .unwrap()
            .join("target/report-fill-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("exact-{}.csv", std::process::id()));
        std::fs::write(
            &path,
            concat!(
                "order_id,ts,qty_raw,price_raw,fee_raw,account_id,strategy_id,signal_id,intent_id,venue_id,venue_order_id,qty_exact,price_exact,exact_fees_json\n",
                "17,1000,0,0,0,main,momentum,5,6,BINANCE,venue-17,0.001,0.00001234,\"[{\"\"currency\"\":\"\"USDT\"\",\"\"amount\"\":\"\"0.00000001\"\"},{\"\"currency\"\":\"\"BNB\"\",\"\"amount\"\":\"\"0.00000002\"\"}]\"\n"
            ),
        )
        .unwrap();

        let fills = read_fills_csv(&path).unwrap();
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].qty_raw, 0);
        assert_eq!(fills[0].qty_exact.as_deref(), Some("0.001"));
        assert_eq!(fills[0].price_exact.as_deref(), Some("0.00001234"));
        assert_eq!(
            fills[0].exact_fees_json.as_deref(),
            Some(
                r#"[{"currency":"USDT","amount":"0.00000001"},{"currency":"BNB","amount":"0.00000002"}]"#
            )
        );
        let html = fills_table(&fills, 1, None);
        assert!(html.contains("<td>0.001</td>"));
        assert!(html.contains("<td>0.00001234</td>"));
        assert!(html.contains("&quot;USDT&quot;"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn exact_multi_leg_fill_reader_preserves_leg_and_exact_amounts() {
        let directory = std::env::current_dir()
            .unwrap()
            .join("target/report-fill-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("multi-leg-exact-{}.csv", std::process::id()));
        std::fs::write(
            &path,
            concat!(
                "leg,order_id,ts,qty_raw,price_raw,fee_raw,account_id,strategy_id,signal_id,intent_id,venue_id,venue_order_id,qty_exact,price_exact,exact_fees_json\n",
                "reference,17,1000,0,0,0,main,momentum,5,6,OKX,venue-17,0.00000001,0.00001234,[]\n"
            ),
        )
        .unwrap();

        let fills = read_fills_csv(&path).unwrap();
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].leg.as_deref(), Some("reference"));
        assert_eq!(fills[0].qty_exact.as_deref(), Some("0.00000001"));
        assert_eq!(fills[0].price_exact.as_deref(), Some("0.00001234"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn html_fill_reader_is_bounded_but_xlsx_reader_still_returns_every_fill() {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::current_dir()
            .unwrap()
            .join("target/report-fill-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!(
            "single-report-{}-{}.csv",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "ts,order_id,qty_raw,price_raw,fee_raw,strategy_id").unwrap();
        for index in 0..10 {
            writeln!(file, "{index},{index},1,2,0,strategy").unwrap();
        }
        drop(file);

        let (recent, total) = read_fills_csv_tail(&path, 3).unwrap();
        assert_eq!(total, 10);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].ts, 7);
        assert_eq!(recent[2].ts, 9);
        let html = fills_table(&recent, total, None);
        assert!(html.contains("当前展示最近 3 / 10 笔成交"));
        assert_eq!(read_fills_csv(&path).unwrap().len(), 10);
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod metric_definition_tests {
    use super::render_report_html;

    #[test]
    fn html_surfaces_versioned_metric_period_and_fee_basis() {
        let summary = serde_json::json!({
            "metrics_definition": {
                "schema_version": 1,
                "risk_ratio_period_basis": "per_sample_no_annualization",
                "fees_exact_basis": "ledger_fee_entries_grouped_by_currency"
            }
        });

        let html = render_report_html(&summary, &[], &[], &[]);

        assert!(html.contains("指标口径 v1"));
        assert!(html.contains("per_sample_no_annualization"));
        assert!(html.contains("ledger_fee_entries_grouped_by_currency"));
    }
}
