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
use crate::report_svg::{equity_svg, fills_svg, money, monthly_svg, year_month};
use qx_core::{Event, EventKind, EventLog};
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// One verified fill projection used by the SVG, HTML table, and XLSX report.
pub(crate) struct FillPoint {
    pub(crate) ts: u64,
    pub(crate) order_id: u64,
    pub(crate) leg: Option<String>,
    pub(crate) qty_raw: i128,
    pub(crate) price_raw: i128,
    pub(crate) fee_raw: Option<i128>,
    pub(crate) account_id: Option<String>,
    pub(crate) strategy_id: String,
    pub(crate) signal_id: Option<String>,
    pub(crate) intent_id: Option<String>,
    pub(crate) venue_id: Option<String>,
    pub(crate) venue_order_id: Option<String>,
}

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
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建报告目录失败 {}: {error}", parent.display()))?;
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
    for (path, content) in [
        (&artifacts.equity_svg, equity_svg(&equity_values)),
        (&artifacts.fills_svg, fills_svg(&fill_values)),
        (&artifacts.monthly_svg, monthly_svg(&monthly)),
    ] {
        qx_app::artifact_store::sink::publish(
            path,
            &content,
            qx_app::artifact_store::sink::PublishMode::Overwrite,
        )
        .map_err(|error| format!("发布 SVG 报告失败 {}: {error}", path.display()))?;
    }
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
    qx_app::artifact_store::sink::publish(
        &artifacts.html,
        &html,
        qx_app::artifact_store::sink::PublishMode::Overwrite,
    )
    .map_err(|error| format!("发布 HTML 报告失败 {}: {error}", artifacts.html.display()))?;
    Ok(artifacts)
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
    let cash = |raw: Option<i128>| {
        raw.map_or_else(
            || READOUT_ABSENT.to_string(),
            |value| format!("¥{}", money(value)),
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
        &cash(number("/metrics/final_equity_raw")),
        "neutral",
    ));
    cards.push_str(&card(
        "期初本金",
        &cash(number("/account/initial_cash_raw")),
        "neutral",
    ));
    cards.push_str(&card(
        "手续费",
        &cash(number("/metrics/fees_raw")),
        "neutral",
    ));
    cards.push_str(&card(
        "换手",
        &cash(number("/metrics/turnover_raw")),
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
    html.push_str(STYLE);
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
    let _ = writeln!(html, "<section class=\"cards\">{cards}</section>");
    let _ = writeln!(
        html,
        "<section class=\"cards\" aria-label=\"风险调整收益比率\">{risk_cards}</section>"
    );
    if let Some(section) = crate::report_benchmark::benchmark_html_section(summary) {
        html.push_str(&section);
    }
    if let Some(section) = crate::ashare_analysis_html_section(summary) {
        html.push_str(&section);
    }
    html.push_str(&credibility_panel(summary));
    html.push_str(&fidelity_panel(summary));
    html.push_str(&strategy_bias_panel(summary));
    html.push_str(&execution::execution_attempts_section(summary));
    html.push_str(&interactive_research_chart(
        equity_analysis,
        fills,
        fill_total,
    ));
    html.push_str("<section class=\"charts\">\n");
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
    html.push_str(&fills_table(fills, fill_total, order_event_links.as_ref()));
    if let Some(events) = event_trace {
        html.push_str(&event_trace_section(events, event_trace_link));
    }
    let _ = writeln!(
        html,
        "<section class=\"prov\"><h2>产物身份</h2>{provenance}</section>"
    );
    html.push_str(
        "<footer>牵星 Qianxing · 分级校准，量天定位 · 本页所有数值均取自同名 \
         <code>*.summary.json</code>，曲线与成交点取自 <code>*.equity.csv</code> / \
         <code>*.fills.csv</code>。</footer>\n",
    );
    html.push_str("</main>\n</body>\n</html>\n");
    html
}

pub(crate) fn read_event_trace(
    path: &Path,
    summary: &serde_json::Value,
) -> Result<Vec<Event>, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取事件追溯产物失败 {}: {error}", path.display()))?;
    let mut log = EventLog::new();
    for (line_index, line) in payload.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: Event = serde_json::from_str(line).map_err(|error| {
            format!(
                "事件追溯 {} 第 {} 行 JSON 无效: {error}",
                path.display(),
                line_index + 1
            )
        })?;
        log.append_checked(event).map_err(|error| {
            format!(
                "事件追溯 {} 第 {} 行违反顺序/唯一性约束: {error}",
                path.display(),
                line_index + 1
            )
        })?;
    }
    log.validate()
        .map_err(|error| format!("事件追溯 {} 校验失败: {error}", path.display()))?;
    let expected_digest = summary
        .pointer("/replay/log_digest")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "摘要缺少 replay.log_digest，无法验证事件追溯文件".to_string())?;
    let expected_digest = u64::from_str_radix(expected_digest, 16)
        .map_err(|error| format!("摘要 replay.log_digest 格式无效: {error}"))?;
    if log.digest() != expected_digest {
        return Err(format!(
            "事件追溯摘要不匹配: expected={expected_digest:016x} actual={:016x}",
            log.digest()
        ));
    }
    let expected_count = summary
        .pointer("/replay/events")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "摘要缺少 replay.events，无法验证事件数量".to_string())?;
    if log.len() as u64 != expected_count {
        return Err(format!(
            "事件追溯数量不匹配: expected={expected_count} actual={}",
            log.len()
        ));
    }
    Ok(log.events().to_vec())
}

const MAX_INLINE_TRACE_EVENTS: usize = 5_000;

fn event_trace_section(events: &[Event], trace_link: Option<&str>) -> String {
    let mut html = String::from(
        "<section class=\"event-trace\"><h2>撮合与账簿事件追溯</h2><p class=\"fills-note\">事件按原日志序列展示。按订单 ID 可关联成交、提交、接受/拒绝与账簿事件；完整事件流在报告旁的 JSONL 文件中，报告读入前已核对重放摘要与事件数量。</p>",
    );
    if let Some(link) = trace_link {
        let _ = write!(
            html,
            "<p><a download href=\"{}\">下载完整事件流 JSONL</a></p>",
            escape_html(link)
        );
    }
    let displayed = events.len().min(MAX_INLINE_TRACE_EVENTS);
    let _ = write!(
        html,
        "<p>内嵌显示最近 {} / {} 个事件；完整事件流保留在 JSONL 中。</p><label>筛选事件/时间/订单 <input id=\"event-filter\" type=\"search\" placeholder=\"订单 ID、事件类型、序列号\"></label><span id=\"event-count\" aria-live=\"polite\"></span><div class=\"fill-scroll\"><table id=\"event-table\"><thead><tr><th>序列</th><th>时间</th><th>优先级</th><th>来源序列</th><th>事件</th><th>订单 ID</th><th>事实内容</th></tr></thead><tbody>",
        displayed,
        events.len()
    );
    for event in events.iter().skip(events.len().saturating_sub(displayed)) {
        let (kind_name, order_id) = event_kind_identity(&event.kind);
        let payload = serde_json::to_string(&event.kind).unwrap_or_else(|_| "{}".to_string());
        let search = format!(
            "{} {} {} {}",
            event.seq,
            event.ts,
            kind_name,
            order_id.map_or_else(String::new, |value| value.to_string())
        );
        let _ = write!(
            html,
            "<tr id=\"event-{}\" data-order-id=\"{}\" data-search=\"{}\"><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><code>{}</code></td></tr>",
            event.seq,
            order_id.map_or_else(String::new, |value| value.to_string()),
            escape_html(&search),
            event.seq,
            event.ts,
            event.prio,
            event.source_seq,
            kind_name,
            order_id.map_or_else(|| READOUT_ABSENT.to_string(), |value| value.to_string()),
            escape_html(&payload),
        );
    }
    html.push_str("</tbody></table></div><p id=\"event-empty\" hidden>没有匹配的事件。</p>");
    if events.len() > displayed {
        let _ = write!(
            html,
            "<p class=\"fills-note\">为保持报告体积可控，内嵌表格截取最近 {displayed} 项；JSONL 文件包含全部事件。</p>"
        );
    }
    let _ = write!(
        html,
        "<script>const eventRows=[...document.querySelectorAll('#event-table tbody tr')],eventFilter=document.getElementById('event-filter'),eventCount=document.getElementById('event-count'),eventEmpty=document.getElementById('event-empty');function filterEvents(){{const query=eventFilter.value.trim().toLocaleLowerCase();let visible=0;for(const row of eventRows){{row.hidden=!row.dataset.search.toLocaleLowerCase().includes(query);if(!row.hidden)visible++;}}eventCount.textContent='显示 '+visible+' / {} 项';eventEmpty.hidden=visible!==0;}}eventFilter.addEventListener('input',filterEvents);filterEvents();</script></section>",
        displayed
    );
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
        r#"<section class="interactive-research"><h2>联动研究图</h2><p class="chart-note">基于权益 CSV 的最多 {MAX_INTERACTIVE_CHART_POINTS} 个等距样本点，鼠标悬停查看时点、权益、持仓与成交订单；曲线可单独开关。权益和回撤相对起点显示，持仓按本次样本最大绝对持仓归一化。原始明细保留在下方表格。</p><div class="chart-controls"><label><input type="checkbox" data-series="equity" checked> 权益变化</label><label><input type="checkbox" data-series="drawdown" checked> 区间回撤</label><label><input type="checkbox" data-series="position" checked> 持仓变化</label><span id="research-chart-count" aria-live="polite"></span></div><div class="research-chart-wrap"><canvas id="research-chart" role="img" aria-label="联动权益、回撤、持仓和成交的研究图"></canvas><div id="research-chart-tip" hidden></div></div></section><script>const researchData={payload};(function(){{const canvas=document.getElementById('research-chart'),ctx=canvas.getContext('2d'),tip=document.getElementById('research-chart-tip'),count=document.getElementById('research-chart-count'),toggles=[...document.querySelectorAll('[data-series]')],points=researchData.points,fills=researchData.fills;count.textContent='显示 '+points.length+' 个样本点 · 成交标记 '+fills.length+' / 全部成交 '+{fill_total};function nearestPoint(ts){{let lo=0,hi=points.length;while(lo<hi){{const mid=(lo+hi)>>1;if(Number(points[mid].ts)<ts)lo=mid+1;else hi=mid;}}if(lo===0)return 0;if(lo>=points.length)return points.length-1;return Math.abs(Number(points[lo-1].ts)-ts)<=Math.abs(Number(points[lo].ts)-ts)?lo-1:lo;}}const fillIndexes=fills.map(fill=>{{const index=nearestPoint(Number(fill.ts));return{{fill,index}};}}),fillsByPoint=new Map();for(const item of fillIndexes){{const ids=fillsByPoint.get(item.index)||[];ids.push(item.fill.order_id);fillsByPoint.set(item.index,ids);}}function seriesValues(){{if(!points.length)return{{equity:[],drawdown:[],position:[]}};const initial=BigInt(points[0].equity_raw);let peak=initial,maxPosition=0n;const drawdown=[];for(const point of points){{const value=BigInt(point.equity_raw);if(value>peak)peak=value;drawdown.push(Number((value-peak)*1000000n/peak)/10000);if(point.position_raw!==null){{const position=BigInt(point.position_raw),abs=position<0n?-position:position;if(abs>maxPosition)maxPosition=abs;}}}}return{{equity:points.map(point=>Number((BigInt(point.equity_raw)-initial)*1000000n/initial)/10000),drawdown,position:points.map(point=>point.position_raw===null||maxPosition===0n?null:Number(BigInt(point.position_raw)*1000000n/maxPosition)/10000)}};}}function draw(){{const rect=canvas.getBoundingClientRect(),scale=window.devicePixelRatio||1,w=Math.max(1,rect.width),h=340;canvas.width=Math.floor(w*scale);canvas.height=Math.floor(h*scale);ctx.setTransform(scale,0,0,scale,0,0);ctx.clearRect(0,0,w,h);if(points.length<2){{ctx.fillStyle='#667085';ctx.fillText('权益样本不足，无法绘制联动曲线',16,28);return;}}const values=seriesValues(),enabled=toggles.filter(item=>item.checked).map(item=>item.dataset.series),all=enabled.flatMap(key=>values[key].filter(value=>value!==null));if(!all.length){{ctx.fillStyle='#667085';ctx.fillText('请选择至少一条曲线',16,28);return;}}const lo=Math.min(...all,0),hi=Math.max(...all,0),pad={{l:58,r:16,t:14,b:34}},plotW=w-pad.l-pad.r,plotH=h-pad.t-pad.b,x=i=>pad.l+i/Math.max(1,points.length-1)*plotW,y=value=>pad.t+(hi-value)/Math.max(.0001,hi-lo)*plotH;ctx.strokeStyle='#e5e7eb';ctx.fillStyle='#667085';ctx.font='11px system-ui';for(let tick=0;tick<5;tick++){{const v=hi-(hi-lo)*tick/4,yy=y(v);ctx.beginPath();ctx.moveTo(pad.l,yy);ctx.lineTo(w-pad.r,yy);ctx.stroke();ctx.fillText(v.toFixed(2)+'%',4,yy+4);}}const colors={{equity:'#2563eb',drawdown:'#dc2626',position:'#7c3aed'}};for(const key of enabled){{ctx.beginPath();let started=false;for(let i=0;i<points.length;i++){{const value=values[key][i];if(value===null)continue;if(!started){{ctx.moveTo(x(i),y(value));started=true;}}else ctx.lineTo(x(i),y(value));}}ctx.strokeStyle=colors[key];ctx.lineWidth=1.7;ctx.stroke();}}ctx.fillStyle='#d97706';for(const item of fillIndexes){{const nearest=item.index;if(values.equity[nearest]!==undefined){{ctx.beginPath();ctx.arc(x(nearest),y(values.equity[nearest]),2.7,0,Math.PI*2);ctx.fill();}}}}canvas._chart={{x, y, values, enabled, pad, plotW, plotH}};}}canvas.addEventListener('mousemove',event=>{{if(!points.length||!canvas._chart)return;const rect=canvas.getBoundingClientRect(),chart=canvas._chart,localX=event.clientX-rect.left,index=Math.max(0,Math.min(points.length-1,Math.round((localX-chart.pad.l)/chart.plotW*(points.length-1)))),point=points[index],when=new Date(Number(point.ts)).toLocaleString(),linked=fillsByPoint.get(index)||[];tip.textContent='时间：'+when+'\n权益 raw：'+point.equity_raw+'\n持仓 raw：'+(point.position_raw??'未提供')+'\n权益变化：'+chart.values.equity[index].toFixed(3)+'%\n区间回撤：'+chart.values.drawdown[index].toFixed(3)+'%\n成交订单：'+(linked.length?linked.join(', '):'无');tip.hidden=false;tip.style.left=Math.max(0,Math.min(Math.max(0,rect.width-250),localX+12))+'px';tip.style.top='20px';}});canvas.addEventListener('mouseleave',()=>tip.hidden=true);for(const toggle of toggles)toggle.addEventListener('change',draw);window.addEventListener('resize',draw);draw();}})();</script>"#,
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

pub(crate) fn event_kind_identity(kind: &EventKind) -> (&'static str, Option<u64>) {
    match kind {
        EventKind::MarketQuote { .. } => ("MarketQuote", None),
        EventKind::AccountBalanceSnapshot { .. } => ("AccountBalanceSnapshot", None),
        EventKind::AccountPositionSnapshot { .. } => ("AccountPositionSnapshot", None),
        EventKind::FundingRateSnapshot { .. } => ("FundingRateSnapshot", None),
        EventKind::AccountCashflow { .. } => ("AccountCashflow", None),
        EventKind::OrderSubmitted { order } => ("OrderSubmitted", Some(order.client_id)),
        EventKind::StrategyDecision { .. } => ("StrategyDecision", None),
        EventKind::OrderRiskDecision { decision } => {
            ("OrderRiskDecision", Some(decision.client_order_id))
        }
        EventKind::Submit { client_order_id } => ("Submit", Some(*client_order_id)),
        EventKind::Accepted {
            client_order_id, ..
        } => ("Accepted", Some(*client_order_id)),
        EventKind::Rejected {
            client_order_id, ..
        } => ("Rejected", Some(*client_order_id)),
        EventKind::Cancelled { client_order_id } => ("Cancelled", Some(*client_order_id)),
        EventKind::Filled { fill } => ("Filled", Some(fill.order_id)),
        EventKind::LedgerApplied { .. } => ("LedgerApplied", None),
        EventKind::ReconcileRequired { client_order_id } => {
            ("ReconcileRequired", Some(*client_order_id))
        }
        EventKind::Settle => ("Settle", None),
    }
}

mod execution;

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

fn strategy_bias_panel(summary: &serde_json::Value) -> String {
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

fn fills_table(
    fills: &[FillPoint],
    fill_total: u64,
    order_event_links: Option<&BTreeMap<u64, u64>>,
) -> String {
    let mut html = String::from(
        "<section class=\"fills\"><h2>成交明细</h2><p class=\"fills-note\">成交事实只展示来源文件明确提供的字段。成交产物不含买卖方向，不按数量正负推断买卖方向或风险决定。存在校验通过的事件流时，可按订单 ID 跳转到订单提交/撮合事件。数量、价格与费用使用 raw 定点值。</p><label class=\"fill-search\">筛选时间/订单/账户/策略/腿 <input id=\"fill-filter\" type=\"search\" placeholder=\"输入订单号、账户、策略 ID 或腿名\"></label><span id=\"fill-count\" aria-live=\"polite\"></span><div class=\"fill-scroll\"><table id=\"fill-table\"><thead><tr><th>时间戳</th><th>订单 ID</th><th>组合腿</th><th>事件链</th><th>账户</th><th>策略 ID</th><th>信号 ID</th><th>意图 ID</th><th>Venue</th><th>Venue 订单 ID</th><th>数量 raw</th><th>价格 raw</th><th>费用 raw</th></tr></thead><tbody>",
    );
    for fill in fills {
        let strategy = escape_html(if fill.strategy_id.is_empty() {
            READOUT_ABSENT
        } else {
            &fill.strategy_id
        });
        let leg = escape_html(fill.leg.as_deref().unwrap_or(READOUT_ABSENT));
        let event_link = order_event_links
            .and_then(|links| links.get(&fill.order_id))
            .map_or_else(
                || READOUT_ABSENT.to_string(),
                |seq| format!("<a href=\"#event-{seq}\">事件 {seq}</a>"),
            );
        let _ = write!(
            html,
            "<tr data-search=\"{} {} {} {} {} {} {} {} {}\"><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            fill.order_id,
            fill.ts,
            escape_html(fill.account_id.as_deref().unwrap_or("")),
            strategy,
            escape_html(fill.signal_id.as_deref().unwrap_or("")),
            escape_html(fill.intent_id.as_deref().unwrap_or("")),
            escape_html(fill.venue_id.as_deref().unwrap_or("")),
            escape_html(fill.venue_order_id.as_deref().unwrap_or("")),
            leg,
            fill.ts,
            fill.order_id,
            leg,
            event_link,
            escape_html(fill.account_id.as_deref().unwrap_or(READOUT_ABSENT)),
            strategy,
            escape_html(fill.signal_id.as_deref().unwrap_or(READOUT_ABSENT)),
            escape_html(fill.intent_id.as_deref().unwrap_or(READOUT_ABSENT)),
            escape_html(fill.venue_id.as_deref().unwrap_or(READOUT_ABSENT)),
            escape_html(fill.venue_order_id.as_deref().unwrap_or(READOUT_ABSENT)),
            fill.qty_raw,
            fill.price_raw,
            fill.fee_raw
                .map_or_else(|| READOUT_ABSENT.to_string(), |value| value.to_string())
        );
    }
    let _ = write!(
        html,
        "</tbody></table></div><p class=\"fills-note\">当前展示最近 {} / {fill_total} 笔成交；完整成交记录保留在 fills CSV 中。</p><p id=\"fill-empty\" hidden>没有匹配的成交。</p><script>const fillRows=[...document.querySelectorAll('#fill-table tbody tr')],fillFilter=document.getElementById('fill-filter'),fillCount=document.getElementById('fill-count'),fillEmpty=document.getElementById('fill-empty');function filterFills(){{const query=fillFilter.value.trim().toLocaleLowerCase();let visible=0;for(const row of fillRows){{row.hidden=!row.dataset.search.toLocaleLowerCase().includes(query);if(!row.hidden)visible++;}}fillCount.textContent='显示 '+visible+' / {} 笔（当前已加载）';fillEmpty.hidden=visible!==0;}}fillFilter.addEventListener('input',filterFills);filterFills();</script></section>",
        fills.len(),
        fills.len()
    );
    html
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

/// Read one of the supported qx-app fills artifact contracts.
pub(crate) fn read_fills_csv(path: &Path) -> Result<Vec<FillPoint>, String> {
    read_fills_csv_window(path, None).map(|(rows, _)| rows)
}

fn read_fills_csv_tail(path: &Path, limit: usize) -> Result<(Vec<FillPoint>, u64), String> {
    read_fills_csv_window(path, Some(limit))
}

fn read_fills_csv_window(
    path: &Path,
    limit: Option<usize>,
) -> Result<(Vec<FillPoint>, u64), String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("读取成交明细失败 {}: {error}", path.display()))?;
    let mut lines = BufReader::new(file).lines();
    let header_line = lines
        .next()
        .transpose()
        .map_err(|error| format!("读取成交明细表头失败 {}: {error}", path.display()))?
        .unwrap_or_default();
    let header = header_line.trim();
    let extended = header
        == "order_id,ts,qty_raw,price_raw,fee_raw,account_id,strategy_id,signal_id,intent_id,venue_id,venue_order_id";
    let multi_leg = header
        == "leg,order_id,ts,qty_raw,price_raw,fee_raw,account_id,strategy_id,signal_id,intent_id,venue_id,venue_order_id";
    let legacy = header == "ts,order_id,qty_raw,price_raw,fee_raw,strategy_id";
    let oldest = header == "order_id,ts,qty_raw,price_raw";
    if !extended && !multi_leg && !legacy && !oldest {
        return Err(format!(
            "成交明细 {} 表头不属于已登记的 4 列、6 列、11 列或 12 列契约",
            path.display()
        ));
    }
    let effective_limit = limit.unwrap_or(usize::MAX);
    let mut rows = VecDeque::with_capacity(limit.unwrap_or_default().min(4_096));
    let mut total = 0_u64;
    for (line_index, line) in lines.enumerate() {
        let line = line.map_err(|error| {
            format!(
                "读取成交明细失败 {} 第 {} 行: {error}",
                path.display(),
                line_index + 2
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split(',').collect();
        let expected_columns = if multi_leg {
            12
        } else if extended {
            11
        } else if legacy {
            6
        } else {
            4
        };
        if cols.len() != expected_columns {
            return Err(format!(
                "成交明细 {} 第 {} 行列数不是 {expected_columns}",
                path.display(),
                line_index + 2
            ));
        }
        let parse = |index: usize, name: &str| -> Result<i128, String> {
            cols[index].parse::<i128>().map_err(|_| {
                format!(
                    "成交明细 {} 第 {} 行 {name} 非法",
                    path.display(),
                    line_index + 2
                )
            })
        };
        let (
            ts_index,
            order_id_index,
            qty_index,
            price_index,
            fee_index,
            account_index,
            strategy_index,
            signal_index,
            intent_index,
            venue_index,
            venue_order_index,
        ) = if multi_leg {
            (
                2,
                1,
                3,
                4,
                Some(5),
                Some(6),
                Some(7),
                Some(8),
                Some(9),
                Some(10),
                Some(11),
            )
        } else if extended {
            (
                1,
                0,
                2,
                3,
                Some(4),
                Some(5),
                Some(6),
                Some(7),
                Some(8),
                Some(9),
                Some(10),
            )
        } else if legacy {
            (0, 1, 2, 3, Some(4), None, Some(5), None, None, None, None)
        } else {
            (1, 0, 2, 3, None, None, None, None, None, None, None)
        };
        let optional = |index: usize| (!cols[index].is_empty()).then(|| cols[index].to_string());
        rows.push_back(FillPoint {
            ts: u64::try_from(parse(ts_index, "ts")?).map_err(|_| {
                format!(
                    "成交明细 {} 第 {} 行 ts 超出 u64 范围",
                    path.display(),
                    line_index + 2
                )
            })?,
            order_id: u64::try_from(parse(order_id_index, "order_id")?).map_err(|_| {
                format!(
                    "成交明细 {} 第 {} 行 order_id 超出 u64 范围",
                    path.display(),
                    line_index + 2
                )
            })?,
            leg: multi_leg.then(|| cols[0].to_string()),
            qty_raw: parse(qty_index, "qty_raw")?,
            price_raw: parse(price_index, "price_raw")?,
            fee_raw: fee_index.map(|index| parse(index, "fee_raw")).transpose()?,
            account_id: account_index.and_then(optional),
            strategy_id: strategy_index.map_or_else(String::new, |index| cols[index].to_string()),
            signal_id: signal_index.and_then(optional),
            intent_id: intent_index.and_then(optional),
            venue_id: venue_index.and_then(optional),
            venue_order_id: venue_order_index.and_then(optional),
        });
        total = total
            .checked_add(1)
            .ok_or_else(|| format!("成交明细 {} 行数超出 u64 范围", path.display()))?;
        if rows.len() > effective_limit {
            rows.pop_front();
        }
    }
    Ok((rows.into_iter().collect(), total))
}

/// 自包含样式：全部内联，无外链字体/脚本/图片（`http` 出现即被门禁判红）。
const STYLE: &str = r#"<style>
:root { color-scheme: light; }
* { box-sizing: border-box; }
body { margin: 0; background: #f5f6f7; color: #202124;
  font-family: -apple-system, "Segoe UI", "Microsoft YaHei", sans-serif; }
main { max-width: 820px; margin: 0 auto; padding: 24px 20px 48px; }
h1 { font-size: 22px; margin: 0 0 4px; }
h2 { font-size: 16px; margin: 0 0 10px; }
.sub { color: #5f6368; font-size: 13px; margin: 0; }
.sub code { background: #eceff1; padding: 1px 5px; border-radius: 4px; }
.disclaimer { margin: 16px 0; padding: 10px 14px; background: #fff8e1;
  border-left: 4px solid #f9a825; font-size: 13px; color: #5f4b00; }
.cards { display: grid; grid-template-columns: repeat(4, 1fr); gap: 10px; margin: 18px 0; }
.card { background: #fff; border: 1px solid #e0e0e0; border-radius: 8px;
  padding: 10px 12px; display: flex; flex-direction: column; gap: 4px; }
.card .k { font-size: 12px; color: #5f6368; }
.card .v { font-size: 18px; font-variant-numeric: tabular-nums; }
.card .v.up { color: #c0392b; }
.card .v.down { color: #1e8449; }
.card .v.neutral { color: #202124; }
.cred { margin: 18px 0; background: #fff; border: 1px solid #e0e0e0; border-radius: 8px; padding: 14px; }
.cred h2 { font-size: 16px; margin: 0 0 10px; }
.cred table { width: 100%; border-collapse: collapse; font-size: 13px; }
.cred th { text-align: left; color: #5f6368; font-weight: 500; padding: 4px 10px 4px 0; white-space: nowrap; vertical-align: top; }
.cred .cdet { padding: 4px 0; color: #202124; }
.cred .cstat { font-weight: 600; padding: 3px 8px; border-radius: 4px; white-space: nowrap; text-align: center; }
.cred .cstat.ok { color: #0b57d0; background: #e8f0fe; }
.cred .cstat.warn { color: #7a4f01; background: #fef7e0; }
.cred .cstat.absent { color: #5f6368; background: #f1f3f4; }
.fidelity { margin: 18px 0; }
.fidelity-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); gap: 12px; }
.fidelity article { min-width: 0; background: #fff; border: 1px solid #e0e0e0; border-radius: 8px; padding: 12px; }
.fidelity h2, .fidelity h3 { margin: 0 0 10px; font-size: 15px; }
.fidelity-note { color: #7a4f01; background: #fef7e0; padding: 8px 10px; font-size: 13px; }
.strategy-diagnostics { margin: 18px 0; background: #fff; border: 1px solid #e0e0e0; border-radius: 8px; padding: 14px; }
.strategy-diagnostics h2 { margin: 0 0 10px; font-size: 16px; }
.diagnostic-status { display: flex; flex-wrap: wrap; gap: 8px 20px; font-size: 13px; }
.diagnostic-status b { font-family: ui-monospace, Consolas, monospace; }
.strategy-diagnostics details { margin-top: 12px; font-size: 13px; }
.strategy-diagnostics input { margin: 10px 0; padding: 6px 8px; }
.diagnostic-scroll { max-height: 340px; overflow: auto; }
.strategy-diagnostics td { overflow-wrap: anywhere; }
.interactive-research { margin: 18px 0; background: #fff; border: 1px solid #e0e0e0; border-radius: 8px; padding: 14px; }
.interactive-research h2 { margin-bottom: 6px; }
.chart-note { color: #5f6368; font-size: 12px; line-height: 1.6; }
.chart-controls { display: flex; gap: 16px; align-items: center; flex-wrap: wrap; margin: 10px 0; font-size: 13px; }
.chart-controls label { display: flex; gap: 6px; align-items: center; }
.research-chart-wrap { height: 340px; position: relative; }
#research-chart { width: 100%; height: 340px; display: block; touch-action: pan-y; }
#research-chart-tip { white-space: pre-line; position: absolute; z-index: 2; max-width: 250px; padding: 8px 10px; border-radius: 6px; background: #202124; color: white; font: 12px/1.6 ui-monospace, Consolas, monospace; pointer-events: none; }
.charts { display: flex; flex-direction: column; gap: 18px; }
figure { margin: 0; background: #fff; border: 1px solid #e0e0e0; border-radius: 8px; padding: 12px; }
figcaption { font-size: 13px; color: #5f6368; margin-bottom: 8px; }
figure svg { width: 100%; height: auto; display: block; }
.prov { margin-top: 22px; background: #fff; border: 1px solid #e0e0e0; border-radius: 8px; padding: 14px; }
table { width: 100%; border-collapse: collapse; font-size: 13px; }
th { text-align: left; color: #5f6368; font-weight: 500; padding: 4px 10px 4px 0; white-space: nowrap; vertical-align: top; }
td { padding: 4px 0; }
td code { background: #f1f3f4; padding: 1px 5px; border-radius: 4px; word-break: break-all; }
footer { margin-top: 24px; color: #80868b; font-size: 12px; line-height: 1.6; }
@media (max-width: 640px) { .cards { grid-template-columns: repeat(2, 1fr); } }
</style>
"#;

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
