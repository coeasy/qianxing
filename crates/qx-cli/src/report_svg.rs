//! 无依赖 SVG 生成器（易用性 P3 / `docs/竞品对比与易用性改进优化计划-V1.md` §6 P3）。
//!
//! 交付件是「单 exe + 运行期零依赖」（竞品对比 §4.3 第 4 条），为一张净值曲线引一个绘图
//! crate 等于把这条优势换掉。这里只做「定点数 → 坐标 → 拼字符串」：纯函数、确定性、**无外部
//! 资源引用**——不写 `<image>` / `<use href>`，内联 SVG 不带 `xmlns`（HTML5 内联不需要它，
//! 带它就等于往产物里塞一个 `http://` 字样，门禁按「出现 http 即判红」防意外外链）。
//!
//! 颜色口径按中国市场约定：**涨=红、跌=绿**。成交点上「买」记红、「卖」记绿。
//!
//! 三张图都从 `BacktestArtifacts` 已落盘的三份产物派生，不新增任何指标算式：
//! 净值/回撤取自 `*.equity.csv`，成交点取自 `*.fills.csv`，月度收益由净值序列按自然月归并。

use std::fmt::Write as _;

/// 画布逻辑尺寸（`viewBox` 坐标系）；HTML 里用 CSS 宽度自适应，缩放不变形。
const W: i64 = 720;
const H: i64 = 240;
/// 左右/上下留白：给刻度文字留位置。
const PAD_X: i64 = 52;
const PAD_Y: i64 = 24;

const RED: &str = "#c0392b";
const GREEN: &str = "#1e8449";
const GRID: &str = "#e6e6e6";
const AXIS: &str = "#9aa0a6";
const INK: &str = "#202124";
const MUTED: &str = "#5f6368";

/// 定点钱（1e9 刻度）→ 两位小数的元字符串。整数部分与小数部分各自取绝对值，
/// 免得负数印成 `-0.xx` 这种读起来像零的东西。`raw` 是 `SCALE` 刻度的整数。
pub(crate) fn money(raw: i128) -> String {
    const SCALE: i128 = 1_000_000_000;
    let negative = raw < 0;
    let magnitude = raw.unsigned_abs();
    let units = magnitude / SCALE as u128;
    let cents = (magnitude % SCALE as u128) / 10_000_000;
    format!("{}{}.{:02}", if negative { "-" } else { "" }, units, cents)
}

/// 把 `value` 从 `[min,max]` 线性映射到 `[lo,hi]` 的像素坐标（用于 y 轴，值大在上）。
fn map(value: i128, min: i128, max: i128, lo: i64, hi: i64) -> i64 {
    if max <= min {
        return (lo + hi) / 2;
    }
    let ratio = (value - min) as f64 / (max - min) as f64;
    (lo as f64 + (hi - lo) as f64 * ratio).round() as i64
}

/// 第 `index` 个样本的 x 坐标（在 `[PAD_X, W-PAD_X]` 上等距铺开）。
fn x_at(index: usize, count: usize) -> i64 {
    let span = W - 2 * PAD_X;
    if count <= 1 {
        return PAD_X + span / 2;
    }
    PAD_X + (span as f64 * index as f64 / (count - 1) as f64).round() as i64
}

/// 一段「无数据」占位 SVG：给的是形状而不是空白，读者一眼能看出是没数据而非渲染失败。
fn empty_svg(label: &str) -> String {
    format!(
        "<svg viewBox=\"0 0 {W} {H}\" role=\"img\" aria-label=\"{label}\">\
         <rect x=\"0\" y=\"0\" width=\"{W}\" height=\"{H}\" fill=\"#fafafa\"/>\
         <text x=\"{cx}\" y=\"{cy}\" text-anchor=\"middle\" fill=\"{MUTED}\" \
         font-size=\"14\">无{label}数据</text></svg>",
        cx = W / 2,
        cy = H / 2,
    )
}

/// 净值曲线 + 最大回撤区间标注。空/单点序列退化成占位或一个点，不画假的趋势线。
pub(crate) fn equity_svg(equity: &[i128]) -> String {
    if equity.is_empty() {
        return empty_svg("权益");
    }
    let min = *equity.iter().min().unwrap_or(&0);
    let max = *equity.iter().max().unwrap_or(&0);
    let top = PAD_Y;
    let bottom = H - PAD_Y;
    let mut svg = String::new();
    let _ = write!(
        svg,
        "<svg viewBox=\"0 0 {W} {H}\" role=\"img\" aria-label=\"权益曲线\">\
         <rect x=\"0\" y=\"0\" width=\"{W}\" height=\"{H}\" fill=\"#ffffff\"/>"
    );
    // 三档横向网格 + 顶/中/底刻度（顶=最高权益、底=最低权益）。
    for (fraction, label) in [
        (0.0_f64, money(max)),
        (0.5, money((min + max) / 2)),
        (1.0, money(min)),
    ] {
        let y = top + ((bottom - top) as f64 * fraction).round() as i64;
        let _ = write!(
            svg,
            "<line x1=\"{PAD_X}\" y1=\"{y}\" x2=\"{x2}\" y2=\"{y}\" stroke=\"{GRID}\"/>\
             <text x=\"{tx}\" y=\"{ty}\" text-anchor=\"end\" fill=\"{MUTED}\" \
             font-size=\"11\">{label}</text>",
            x2 = W - PAD_X,
            tx = PAD_X - 6,
            ty = y + 4,
        );
    }
    // 最大回撤：从峰值到其后的最低点，画一条竖向区间带 + 一句标注。
    if let Some((peak, trough)) = max_drawdown_span(equity) {
        let x1 = x_at(peak, equity.len());
        let x2 = x_at(trough, equity.len());
        let _ = write!(
            svg,
            "<rect x=\"{x1}\" y=\"{top}\" width=\"{width}\" height=\"{height}\" \
             fill=\"{RED}\" fill-opacity=\"0.08\"/>",
            width = (x2 - x1).max(1),
            height = bottom - top,
        );
    }
    // 曲线本体：末值不低于首值记红（涨），否则记绿（跌）。
    let rising = equity.last().copied().unwrap_or(0) >= equity.first().copied().unwrap_or(0);
    let stroke = if rising { RED } else { GREEN };
    let points = equity
        .iter()
        .enumerate()
        .map(|(index, value)| {
            format!(
                "{},{}",
                x_at(index, equity.len()),
                map(*value, min, max, bottom, top)
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let _ = write!(
        svg,
        "<polyline points=\"{points}\" fill=\"none\" stroke=\"{stroke}\" stroke-width=\"1.6\"/>"
    );
    // 首/末两点的圆点：让「从哪开始、到哪结束」有锚。
    for (index, value) in [
        (0usize, equity[0]),
        (equity.len() - 1, equity[equity.len() - 1]),
    ] {
        let _ = write!(
            svg,
            "<circle cx=\"{cx}\" cy=\"{cy}\" r=\"2.6\" fill=\"{stroke}\"/>",
            cx = x_at(index, equity.len()),
            cy = map(value, min, max, bottom, top),
        );
    }
    svg.push_str("</svg>");
    svg
}

/// 找最大回撤的 `(峰值下标, 谷值下标)`：峰值取「到谷值之前的最高点」，与
/// `metrics.max_drawdown_bps` 的口径一致（峰值必须在谷值之前，不能取谷值之后的反弹）。
fn max_drawdown_span(equity: &[i128]) -> Option<(usize, usize)> {
    let mut peak = 0usize;
    let mut best: Option<(usize, usize, i128)> = None;
    for (index, value) in equity.iter().enumerate() {
        if *value > equity[peak] {
            peak = index;
        }
        let drop = equity[peak] - *value;
        if drop > 0 && best.is_none_or(|(_, _, current)| drop > current) {
            best = Some((peak, index, drop));
        }
    }
    best.map(|(peak, trough, _)| (peak, trough))
}

/// 成交点图：价格轴上的买/卖标记。`fills` = `(ts, qty_raw, price_raw)`；`qty_raw > 0` 记买（红），
/// `< 0` 记卖（绿）。空成交**不画任何三角**——0 成交时画出假买卖点比没有图更糟。
pub(crate) fn fills_svg(fills: &[(u64, i128, i128)]) -> String {
    if fills.is_empty() {
        return empty_svg("成交");
    }
    let prices: Vec<i128> = fills.iter().map(|(_, _, price)| *price).collect();
    let min = *prices.iter().min().unwrap_or(&0);
    let max = *prices.iter().max().unwrap_or(&0);
    let top = PAD_Y;
    let bottom = H - PAD_Y;
    let mut svg = String::new();
    let _ = write!(
        svg,
        "<svg viewBox=\"0 0 {W} {H}\" role=\"img\" aria-label=\"成交点\">\
         <rect x=\"0\" y=\"0\" width=\"{W}\" height=\"{H}\" fill=\"#ffffff\"/>\
         <line x1=\"{PAD_X}\" y1=\"{bottom}\" x2=\"{x2}\" y2=\"{bottom}\" stroke=\"{AXIS}\"/>\
         <text x=\"{tx}\" y=\"{ty}\" text-anchor=\"end\" fill=\"{MUTED}\" \
         font-size=\"11\">{top_label}</text>\
         <text x=\"{tx}\" y=\"{by}\" text-anchor=\"end\" fill=\"{MUTED}\" \
         font-size=\"11\">{bottom_label}</text>",
        x2 = W - PAD_X,
        tx = PAD_X - 6,
        ty = top + 4,
        by = bottom + 4,
        top_label = money(max),
        bottom_label = money(min),
    );
    for (index, (_, qty, price)) in fills.iter().enumerate() {
        let cx = x_at(index, fills.len());
        let cy = map(*price, min, max, bottom, top);
        let (color, direction) = if *qty >= 0 { (RED, -1) } else { (GREEN, 1) };
        // 上三角=买（顶点朝上），下三角=卖（顶点朝下）。
        let (a, b, c) = if direction < 0 {
            (
                format!("{cx},{}", cy - 6),
                format!("{},{}", cx - 4, cy + 3),
                format!("{},{}", cx + 4, cy + 3),
            )
        } else {
            (
                format!("{cx},{}", cy + 6),
                format!("{},{}", cx - 4, cy - 3),
                format!("{},{}", cx + 4, cy - 3),
            )
        };
        let _ = write!(svg, "<polygon points=\"{a} {b} {c}\" fill=\"{color}\"/>");
    }
    svg.push_str("</svg>");
    svg
}

/// 月度收益热力表。`monthly` = `(年, 月, 收益 bps)`；正=红、负=绿、缺=浅灰。
/// 行按年份升序、列固定 1..12，所以同输入必然得到同布局。
pub(crate) fn monthly_svg(monthly: &[(i32, u32, i64)]) -> String {
    if monthly.is_empty() {
        return empty_svg("月度收益");
    }
    let years: Vec<i32> = {
        let mut set: Vec<i32> = monthly.iter().map(|(year, _, _)| *year).collect();
        set.sort_unstable();
        set.dedup();
        set
    };
    let cols = 12usize;
    let cell = 44i64;
    let gap = 4i64;
    let label_w = 44i64;
    let head_h = 20i64;
    let rows = years.len() as i64;
    let width = label_w + cols as i64 * (cell + gap);
    let height = head_h + rows * (cell + gap);
    let peak = monthly
        .iter()
        .map(|(_, _, bps)| bps.unsigned_abs())
        .max()
        .unwrap_or(1)
        .max(1);
    let mut svg = String::new();
    let _ = write!(
        svg,
        "<svg viewBox=\"0 0 {width} {height}\" role=\"img\" aria-label=\"月度收益\">\
         <rect x=\"0\" y=\"0\" width=\"{width}\" height=\"{height}\" fill=\"#ffffff\"/>"
    );
    for month in 1..=cols {
        let x = label_w + (month as i64 - 1) * (cell + gap) + cell / 2;
        let _ = write!(
            svg,
            "<text x=\"{x}\" y=\"14\" text-anchor=\"middle\" fill=\"{MUTED}\" \
             font-size=\"11\">{month}</text>"
        );
    }
    for (row, year) in years.iter().enumerate() {
        let y = head_h + row as i64 * (cell + gap);
        let _ = write!(
            svg,
            "<text x=\"{tx}\" y=\"{ty}\" text-anchor=\"end\" fill=\"{MUTED}\" \
             font-size=\"11\">{year}</text>",
            tx = label_w - 6,
            ty = y + cell / 2 + 4,
        );
        for month in 1..=cols {
            let x = label_w + (month as i64 - 1) * (cell + gap);
            let found = monthly
                .iter()
                .find(|(m_year, m_month, _)| *m_year == *year && *m_month == month as u32);
            let (fill, label, ink) = match found {
                Some((_, _, bps)) => {
                    let alpha = 0.15 + 0.75 * (*bps as f64).abs() / peak as f64;
                    let color = if *bps >= 0 { RED } else { GREEN };
                    (
                        color.to_string(),
                        format!("{:.1}", *bps as f64 / 100.0),
                        format!("fill-opacity=\"{alpha:.2}\""),
                    )
                }
                None => (
                    GRID.to_string(),
                    String::new(),
                    "fill-opacity=\"1\"".to_string(),
                ),
            };
            let _ = write!(
                svg,
                "<rect x=\"{x}\" y=\"{y}\" width=\"{cell}\" height=\"{cell}\" \
                 fill=\"{fill}\" {ink}/>"
            );
            if !label.is_empty() {
                let _ = write!(
                    svg,
                    "<text x=\"{cx}\" y=\"{cy}\" text-anchor=\"middle\" fill=\"{INK}\" \
                     font-size=\"10\">{label}</text>",
                    cx = x + cell / 2,
                    cy = y + cell / 2 + 4,
                );
            }
        }
    }
    svg.push_str("</svg>");
    svg
}

/// epoch 毫秒 → `(年, 月)`。用 Howard Hinnant 的 `civil_from_days`（无 chrono 依赖），
/// 对负时间戳也用 `div_euclid` 保证落在正确的自然月。
pub(crate) fn year_month(ts_ms: i64) -> (i32, u32) {
    let days = ts_ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year as i32, month as u32)
}
