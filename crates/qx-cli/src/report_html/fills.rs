//! Bounded reader for the supported fills CSV contracts used by HTML and XLSX reports.

use super::escape_html;
use crate::report_readout::READOUT_ABSENT;
use qx_core::ExactDecimal;
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// One verified fill projection used by the SVG, HTML table, and XLSX report.
pub(crate) struct FillPoint {
    pub(crate) ts: u64,
    pub(crate) order_id: u64,
    pub(crate) leg: Option<String>,
    pub(crate) qty_raw: i128,
    pub(crate) price_raw: i128,
    pub(crate) fee_raw: Option<i128>,
    pub(crate) qty_exact: Option<String>,
    pub(crate) price_exact: Option<String>,
    pub(crate) exact_fees_json: Option<String>,
    pub(crate) account_id: Option<String>,
    pub(crate) strategy_id: String,
    pub(crate) signal_id: Option<String>,
    pub(crate) intent_id: Option<String>,
    pub(crate) venue_id: Option<String>,
    pub(crate) venue_order_id: Option<String>,
}

pub(super) fn fills_table(
    fills: &[FillPoint],
    fill_total: u64,
    order_event_links: Option<&BTreeMap<u64, u64>>,
) -> String {
    let mut html = String::from(
        "<section class=\"fills\" id=\"report-fills\"><h2>成交明细</h2><p class=\"fills-note\">成交事实只展示来源文件明确提供的字段。成交产物不含买卖方向，不按数量正负推断买卖方向或风险决定。存在校验通过的事件流时，可按订单 ID 跳转到订单提交/撮合事件。精确数量、价格和手续费只在来源产物明确提供时显示；raw 字段保留作兼容投影。</p><label class=\"fill-search\">筛选时间/订单/账户/策略/腿 <input id=\"fill-filter\" type=\"search\" placeholder=\"输入订单号、账户、策略 ID 或腿名\"></label><span id=\"fill-count\" aria-live=\"polite\"></span><div class=\"fill-scroll\"><table id=\"fill-table\"><thead><tr><th>时间戳</th><th>订单 ID</th><th>组合腿</th><th>事件链</th><th>账户</th><th>策略 ID</th><th>信号 ID</th><th>意图 ID</th><th>Venue</th><th>Venue 订单 ID</th><th>数量 raw</th><th>数量 exact</th><th>价格 raw</th><th>价格 exact</th><th>费用 raw</th><th>精确手续费 JSON</th></tr></thead><tbody>",
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
            "<tr data-search=\"{} {} {} {} {} {} {} {} {} {} {} {}\"><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            fill.order_id,
            fill.ts,
            escape_html(fill.account_id.as_deref().unwrap_or("")),
            strategy,
            escape_html(fill.signal_id.as_deref().unwrap_or("")),
            escape_html(fill.intent_id.as_deref().unwrap_or("")),
            escape_html(fill.venue_id.as_deref().unwrap_or("")),
            escape_html(fill.venue_order_id.as_deref().unwrap_or("")),
            leg,
            escape_html(fill.qty_exact.as_deref().unwrap_or("")),
            escape_html(fill.price_exact.as_deref().unwrap_or("")),
            escape_html(fill.exact_fees_json.as_deref().unwrap_or("")),
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
            escape_html(fill.qty_exact.as_deref().unwrap_or(READOUT_ABSENT)),
            fill.price_raw,
            escape_html(fill.price_exact.as_deref().unwrap_or(READOUT_ABSENT)),
            fill.fee_raw
                .map_or_else(|| READOUT_ABSENT.to_string(), |value| value.to_string()),
            escape_html(fill.exact_fees_json.as_deref().unwrap_or(READOUT_ABSENT))
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

/// Read one of the supported qx-app fills artifact contracts.
pub(crate) fn read_fills_csv(path: &Path) -> Result<Vec<FillPoint>, String> {
    read_fills_csv_window(path, None).map(|(rows, _)| rows)
}

pub(crate) fn read_fills_csv_tail(
    path: &Path,
    limit: usize,
) -> Result<(Vec<FillPoint>, u64), String> {
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
    let exact = header
        == "order_id,ts,qty_raw,price_raw,fee_raw,account_id,strategy_id,signal_id,intent_id,venue_id,venue_order_id,qty_exact,price_exact,exact_fees_json";
    let multi_leg = header
        == "leg,order_id,ts,qty_raw,price_raw,fee_raw,account_id,strategy_id,signal_id,intent_id,venue_id,venue_order_id";
    let multi_leg_exact = header
        == "leg,order_id,ts,qty_raw,price_raw,fee_raw,account_id,strategy_id,signal_id,intent_id,venue_id,venue_order_id,qty_exact,price_exact,exact_fees_json";
    let legacy = header == "ts,order_id,qty_raw,price_raw,fee_raw,strategy_id";
    let oldest = header == "order_id,ts,qty_raw,price_raw";
    if !exact && !extended && !multi_leg_exact && !multi_leg && !legacy && !oldest {
        return Err(format!(
            "成交明细 {} 表头不属于已登记的 4 列、6 列、11 列、12 列、14 列或 15 列契约",
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
        let cols = parse_csv_record(&line).map_err(|reason| {
            format!(
                "成交明细 {} 第 {} 行 CSV 格式非法：{reason}",
                path.display(),
                line_index + 2
            )
        })?;
        let expected_columns = if multi_leg_exact {
            15
        } else if multi_leg {
            12
        } else if exact {
            14
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
        ) = if multi_leg || multi_leg_exact {
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
        } else if exact || extended {
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
        let optional = |index: usize| (!cols[index].is_empty()).then(|| cols[index].clone());
        let (qty_exact, price_exact, exact_fees_json) = if exact || multi_leg_exact {
            let exact_offset = if multi_leg_exact { 12 } else { 11 };
            let qty = optional(exact_offset);
            let price = optional(exact_offset + 1);
            if qty.is_some() != price.is_some() {
                return Err(format!(
                    "成交明细 {} 第 {} 行精确数量和价格必须同时存在",
                    path.display(),
                    line_index + 2
                ));
            }
            for (value, field) in [
                (qty.as_deref(), "qty_exact"),
                (price.as_deref(), "price_exact"),
            ] {
                if let Some(value) = value {
                    ExactDecimal::parse(value).map_err(|reason| {
                        format!(
                            "成交明细 {} 第 {} 行 {field} 非法：{reason}",
                            path.display(),
                            line_index + 2
                        )
                    })?;
                }
            }
            let fees = optional(exact_offset + 2);
            if let Some(fees) = fees.as_deref() {
                let value: serde_json::Value = serde_json::from_str(fees).map_err(|error| {
                    format!(
                        "成交明细 {} 第 {} 行 exact_fees_json 非法：{error}",
                        path.display(),
                        line_index + 2
                    )
                })?;
                if !value.is_array() {
                    return Err(format!(
                        "成交明细 {} 第 {} 行 exact_fees_json 必须是 JSON 数组",
                        path.display(),
                        line_index + 2
                    ));
                }
            }
            (qty, price, fees)
        } else {
            (None, None, None)
        };
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
            leg: (multi_leg || multi_leg_exact).then(|| cols[0].clone()),
            qty_raw: parse(qty_index, "qty_raw")?,
            price_raw: parse(price_index, "price_raw")?,
            fee_raw: fee_index.map(|index| parse(index, "fee_raw")).transpose()?,
            qty_exact,
            price_exact,
            exact_fees_json,
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

/// Parse one RFC 4180 record. Fill rows are newline-delimited; quoted cells may contain
/// commas and escaped quotes (notably the JSON fee breakdown) but not embedded newlines.
fn parse_csv_record(record: &str) -> Result<Vec<String>, &'static str> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = record.chars().peekable();
    let mut quoted = false;
    let mut closed_quote = false;
    while let Some(character) = chars.next() {
        if quoted {
            match character {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => {
                    quoted = false;
                    closed_quote = true;
                }
                _ => field.push(character),
            }
        } else {
            match character {
                ',' => {
                    fields.push(std::mem::take(&mut field));
                    closed_quote = false;
                }
                '"' if field.is_empty() && !closed_quote => quoted = true,
                _ if closed_quote => return Err("结束引号后只能出现逗号或记录结尾"),
                '"' => return Err("未加引号的字段包含双引号"),
                _ => field.push(character),
            }
        }
    }
    if quoted {
        return Err("带引号的字段未闭合");
    }
    fields.push(field);
    Ok(fields)
}
