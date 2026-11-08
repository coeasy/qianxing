//! Verified EventLog loading and bounded offline event-trace projection.

use super::{escape_html, READOUT_ABSENT};
use qx_core::{Event, EventKind, EventLog};
use std::fmt::Write as _;
use std::path::Path;

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

pub(super) const MAX_INLINE_TRACE_EVENTS: usize = 5_000;

pub(super) fn event_trace_section(events: &[Event], trace_link: Option<&str>) -> String {
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

pub(crate) fn event_kind_identity(kind: &EventKind) -> (&'static str, Option<u64>) {
    match kind {
        EventKind::MarketQuote { .. } => ("MarketQuote", None),
        EventKind::MarketOrderBook { .. } => ("MarketOrderBook", None),
        EventKind::AccountBalanceSnapshot { .. } => ("AccountBalanceSnapshot", None),
        EventKind::AccountPositionSnapshot { .. } => ("AccountPositionSnapshot", None),
        EventKind::FundingRateSnapshot { .. } => ("FundingRateSnapshot", None),
        EventKind::AccountCashflow { .. } => ("AccountCashflow", None),
        EventKind::OrderSubmitted { order } => ("OrderSubmitted", Some(order.client_id)),
        EventKind::StrategyDecision { .. } => ("StrategyDecision", None),
        EventKind::OrderRiskDecision { decision } => {
            ("OrderRiskDecision", Some(decision.client_order_id))
        }
        EventKind::ExecutionCapabilityDecision { decision } => (
            "ExecutionCapabilityDecision",
            Some(decision.client_order_id),
        ),
        EventKind::Submit { client_order_id } => ("Submit", Some(*client_order_id)),
        EventKind::Accepted {
            client_order_id, ..
        } => ("Accepted", Some(*client_order_id)),
        EventKind::Rejected {
            client_order_id, ..
        } => ("Rejected", Some(*client_order_id)),
        EventKind::CancelPending { client_order_id } => ("CancelPending", Some(*client_order_id)),
        EventKind::Cancelled { client_order_id } => ("Cancelled", Some(*client_order_id)),
        EventKind::Filled { fill } => ("Filled", Some(fill.order_id)),
        EventKind::FeeAdjustment { order_id, .. } => ("FeeAdjustment", Some(*order_id)),
        EventKind::LedgerApplied { .. } => ("LedgerApplied", None),
        EventKind::ReconcileRequired { client_order_id } => {
            ("ReconcileRequired", Some(*client_order_id))
        }
        EventKind::OrderStatusReconciled {
            client_order_id, ..
        } => ("OrderStatusReconciled", Some(*client_order_id)),
        EventKind::Settle => ("Settle", None),
    }
}
