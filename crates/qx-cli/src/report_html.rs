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

use super::*;
use std::fmt::Write as _;

/// 一条成交点：只保留画图需要的三格。
pub(crate) struct FillPoint {
    pub(crate) ts: u64,
    pub(crate) qty_raw: i128,
    pub(crate) price_raw: i128,
}

#[derive(Debug)]
pub(crate) struct ReportArtifacts {
    pub(crate) html: PathBuf,
    pub(crate) equity_svg: PathBuf,
    pub(crate) fills_svg: PathBuf,
    pub(crate) monthly_svg: PathBuf,
}

/// 写自包含 HTML 与三张独立 SVG；默认均与摘要同目录、同前缀。
pub(crate) fn write_report_html(
    summary_path: &Path,
    summary: &serde_json::Value,
    output: Option<&Path>,
) -> Result<ReportArtifacts, String> {
    let stem = summary_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".summary.json"))
        .ok_or_else(|| {
            format!(
                "报告摘要文件名不是 *.summary.json: {}",
                summary_path.display()
            )
        })?;
    let sibling = |suffix: &str| summary_path.with_file_name(format!("{stem}.{suffix}"));
    let equity_path = sibling("equity.csv");
    let fills_path = sibling("fills.csv");
    let equity = if equity_path.is_file() {
        read_equity_csv(&equity_path)?
    } else {
        Vec::new()
    };
    let fills = if fills_path.is_file() {
        read_fills_csv(&fills_path)?
    } else {
        Vec::new()
    };
    let out = output.map_or_else(|| sibling("report.html"), Path::to_path_buf);
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
        stem
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
        std::fs::write(path, content)
            .map_err(|error| format!("写入 SVG 报告失败 {}: {error}", path.display()))?;
    }
    let html = render_report_html(summary, &equity, &fills, &monthly);
    std::fs::write(&artifacts.html, html)
        .map_err(|error| format!("写入 HTML 报告失败 {}: {error}", artifacts.html.display()))?;
    Ok(artifacts)
}

/// 从三份产物渲染 HTML（纯函数，不经文件系统）。
///
/// 月度收益由调用方传入：落盘路径（[`write_report_html`]）既要用它画 SVG、又要用它排版
/// HTML 表格，算一次再传进来，两个消费点就共用同一份切片，不会各自漂移。`monthly` 的唯一
/// 算式入口是 [`monthly_series`]。
pub(crate) fn render_report_html(
    summary: &serde_json::Value,
    equity: &[(i64, i128)],
    fills: &[FillPoint],
    monthly: &[(i32, u32, i64)],
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
    html.push_str(&credibility_panel(summary));
    html.push_str("<section class=\"charts\">\n");
    let _ = writeln!(
        html,
        "<figure><figcaption>权益曲线（含最大回撤区间）</figcaption>{}</figure>",
        equity_svg(&equity_series)
    );
    let _ = writeln!(
        html,
        "<figure><figcaption>成交点（▲ 买 / ▼ 卖）</figcaption>{}</figure>",
        fills_svg(&fills_points)
    );
    let _ = writeln!(
        html,
        "<figure><figcaption>月度收益（%）</figcaption>{}</figure>",
        monthly_svg(monthly)
    );
    html.push_str("</section>\n");
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
    format!("<div class=\"card\"><span class=\"k\">{label}</span><span class=\"v {tone}\">{value}</span></div>")
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

/// 读 `*.equity.csv`（`index,ts,equity_raw,position_raw`）。非表头的行解析不出即报错，
/// 不静默跳过——静默跳过的曲线会与摘要数字对不上，那是更难查的错。
fn read_equity_csv(path: &Path) -> Result<Vec<(i64, i128)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("读取权益曲线失败 {}: {error}", path.display()))?;
    let mut rows = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        if line_no == 0 || line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split(',').collect();
        let ts = cols
            .get(1)
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or_else(|| format!("权益曲线 {} 第 {} 行 ts 非法", path.display(), line_no + 1))?;
        let equity = cols
            .get(2)
            .and_then(|value| value.parse::<i128>().ok())
            .ok_or_else(|| {
                format!(
                    "权益曲线 {} 第 {} 行 equity_raw 非法",
                    path.display(),
                    line_no + 1
                )
            })?;
        rows.push((ts, equity));
    }
    Ok(rows)
}

/// 读 `*.fills.csv`（`order_id,ts,qty_raw,price_raw,...`），口径同 [`read_equity_csv`]。
fn read_fills_csv(path: &Path) -> Result<Vec<FillPoint>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("读取成交明细失败 {}: {error}", path.display()))?;
    let mut rows = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        if line_no == 0 || line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split(',').collect();
        let parse = |index: usize, name: &str| -> Result<i128, String> {
            cols.get(index)
                .and_then(|value| value.parse::<i128>().ok())
                .ok_or_else(|| {
                    format!(
                        "成交明细 {} 第 {} 行 {name} 非法",
                        path.display(),
                        line_no + 1
                    )
                })
        };
        rows.push(FillPoint {
            ts: parse(1, "ts")? as u64,
            qty_raw: parse(2, "qty_raw")?,
            price_raw: parse(3, "price_raw")?,
        });
    }
    Ok(rows)
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
