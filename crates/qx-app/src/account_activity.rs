//! Bounded, identity-scoped account activity projections.

use crate::{AppError, AppErrorCategory};
use qx_core::{Event, EventKind, EventMetadata};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashSet, VecDeque};

pub const ACCOUNT_ACTIVITY_REPORT_SCHEMA_VERSION_V1: u32 = 1;
pub const MAX_ACCOUNT_ACTIVITY_EVENT_PAGE_V1: usize = 300;
pub const MAX_ACCOUNT_ACTIVITY_SNAPSHOT_ROWS_V1: usize = 500;
pub const DEFAULT_ACCOUNT_ACTIVITY_EVENT_PAGE_V1: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountActivityReportReadOptionsV1 {
    #[serde(default)]
    pub after: Option<u64>,
    #[serde(default)]
    pub before: Option<u64>,
    #[serde(default)]
    pub client_order_id: Option<u64>,
    /// Exact opaque venue-assigned order identity (case-sensitive).
    #[serde(default)]
    pub exchange_order_id: Option<String>,
    /// Exact strategy run identity; only events carrying this context match.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Exact strategy identity; can be combined with `run_id` or order filters.
    #[serde(default)]
    pub strategy_id: Option<String>,
    /// Exact correlation anchor; when it identifies an order event, include all retained facts
    /// for that same client order so the submit/ack/fill/ledger lifecycle can be inspected.
    #[serde(default)]
    pub correlation_id: Option<String>,
    #[serde(default = "default_event_limit")]
    pub limit: usize,
}

impl Default for AccountActivityReportReadOptionsV1 {
    fn default() -> Self {
        Self {
            after: None,
            before: None,
            client_order_id: None,
            exchange_order_id: None,
            run_id: None,
            strategy_id: None,
            correlation_id: None,
            limit: DEFAULT_ACCOUNT_ACTIVITY_EVENT_PAGE_V1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountActivityReportV1 {
    pub schema_version: u32,
    pub identity: AccountActivityIdentityV1,
    pub as_of_age_ms: Option<u64>,
    pub balances: Value,
    pub positions: AccountActivityRowsV1,
    pub orders: AccountActivityRowsV1,
    pub fills: AccountActivityRowsV1,
    pub transfers: AccountActivityRowsV1,
    pub events: AccountActivityEventPageV1,
    pub reconciliation: Value,
    pub completeness: AccountActivityCompletenessV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountActivityIdentityV1 {
    pub account_id: String,
    pub venue_id: String,
    pub portfolio_id: String,
    pub snapshot_id: u64,
    pub snapshot_event_seq: u64,
    pub snapshot_state_hash: String,
    pub snapshot_as_of_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountActivityRowsV1 {
    pub total: u64,
    pub returned: usize,
    pub truncated: bool,
    pub rows: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountActivityEventPageV1 {
    /// Number of rows available after applying the optional order/run/strategy filters.
    pub available_total: u64,
    #[serde(default)]
    pub client_order_id: Option<u64>,
    #[serde(default)]
    pub exchange_order_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub strategy_id: Option<String>,
    #[serde(default)]
    pub correlation_id: Option<String>,
    pub oldest_seq: Option<u64>,
    pub latest_seq: Option<u64>,
    pub requested_after: Option<u64>,
    pub requested_before: Option<u64>,
    pub limit: usize,
    pub has_older: bool,
    pub has_newer: bool,
    /// Pass this value as `before` to read the preceding page.
    pub previous_before: Option<u64>,
    /// Pass this value as `after` to read the following page.
    pub next_after: Option<u64>,
    pub rows: Vec<AccountActivityEventRowV1>,
}

/// Account-scoped event row with stable identity and order/run pivots for lifecycle tracing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountActivityEventRowV1 {
    #[serde(default)]
    pub event_id: String,
    pub seq: u64,
    pub ts: u64,
    pub prio: u8,
    pub kind: EventKind,
    pub receive_time: u64,
    pub engine_time: u64,
    pub source_seq: u64,
    pub correlation_id: String,
    pub metadata: EventMetadata,
    pub client_order_id: Option<u64>,
    pub exchange_order_id: Option<String>,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub strategy_id: String,
}

impl AccountActivityEventRowV1 {
    fn from_event(account_id: &str, venue_id: &str, event: &Event) -> Self {
        let (client_order_id, exchange_order_id) = order_ids_for_event(event);
        let context = &event.metadata.context;
        Self {
            event_id: format!(
                "account-event:v1:{}:{}:{}:{}:{}",
                account_id.len(),
                account_id,
                venue_id.len(),
                venue_id,
                event.seq
            ),
            seq: event.seq,
            ts: event.ts,
            prio: event.prio,
            kind: event.kind.clone(),
            receive_time: event.receive_time,
            engine_time: event.engine_time,
            source_seq: event.source_seq,
            correlation_id: event.correlation_id.clone(),
            metadata: event.metadata.clone(),
            client_order_id,
            exchange_order_id,
            run_id: context.run_id.clone(),
            strategy_id: context.strategy_id.clone(),
        }
    }
}

fn order_ids_for_event(event: &Event) -> (Option<u64>, Option<String>) {
    let (client_order_id, exchange_order_id) = match &event.kind {
        EventKind::OrderSubmitted { order }
        | EventKind::OrderAmended { order, .. }
        | EventKind::OrderAmendmentPending { order, .. } => (Some(order.client_id), None),
        EventKind::OrderRiskDecision { decision } => (Some(decision.client_order_id), None),
        EventKind::ExecutionCapabilityDecision { decision } => {
            (Some(decision.client_order_id), None)
        }
        EventKind::Submit { client_order_id }
        | EventKind::Accepted {
            client_order_id, ..
        }
        | EventKind::Rejected {
            client_order_id, ..
        }
        | EventKind::CancelPending { client_order_id }
        | EventKind::Cancelled { client_order_id }
        | EventKind::ReconcileRequired { client_order_id } => (Some(*client_order_id), None),
        EventKind::Filled { fill } => (Some(fill.order_id), fill.venue_order_id.clone()),
        EventKind::FeeAdjustment { order_id, .. } => (Some(*order_id), None),
        EventKind::LedgerApplied { entry } => (entry.order_id, None),
        EventKind::OrderStatusReconciled {
            client_order_id,
            venue_order_id,
            ..
        } => (Some(*client_order_id), venue_order_id.clone()),
        _ => (None, None),
    };
    let exchange_order_id = exchange_order_id.or_else(|| match &event.kind {
        EventKind::Accepted { venue_order_id, .. } => venue_order_id.clone(),
        _ => None,
    });
    (client_order_id, exchange_order_id)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountActivityCompletenessV1 {
    pub snapshot_available: bool,
    /// The report describes the projection's retained log and does not infer venue history
    /// before that log's first row.
    pub event_history_scope: String,
    /// Counts over the complete retained account EventLog, independent of page filters.
    /// `with_non_account_run_id` excludes the runtime's synthetic `account:<id>` scope.
    #[serde(default)]
    pub event_context_coverage: AccountActivityEventContextCoverageV1,
    pub equity_reported: bool,
    pub available_reported: bool,
    pub margin_reported: bool,
    pub realized_pnl_reported: bool,
    pub unrealized_pnl_reported: bool,
    pub fees_reported: bool,
    pub funding_reported: bool,
    pub reconciliation_reported: bool,
    pub positions_truncated: bool,
    pub orders_truncated: bool,
    pub fills_truncated: bool,
    pub transfers_truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountActivityEventContextCoverageV1 {
    pub total_events: u64,
    pub with_correlation_id: u64,
    pub with_non_account_run_id: u64,
    pub with_strategy_id: u64,
    pub with_client_order_id: u64,
    pub with_exchange_order_id: u64,
}

impl AccountActivityEventContextCoverageV1 {
    fn observe(&mut self, event: &Event) {
        self.total_events = self.total_events.saturating_add(1);
        if !event.correlation_id.trim().is_empty() {
            self.with_correlation_id = self.with_correlation_id.saturating_add(1);
        }
        let context = &event.metadata.context;
        if !context.run_id.trim().is_empty() && !context.run_id.starts_with("account:") {
            self.with_non_account_run_id = self.with_non_account_run_id.saturating_add(1);
        }
        if !context.strategy_id.trim().is_empty() {
            self.with_strategy_id = self.with_strategy_id.saturating_add(1);
        }
        let (client_order_id, exchange_order_id) = order_ids_for_event(event);
        if client_order_id.is_some() {
            self.with_client_order_id = self.with_client_order_id.saturating_add(1);
        }
        if exchange_order_id.is_some() {
            self.with_exchange_order_id = self.with_exchange_order_id.saturating_add(1);
        }
    }
}

/// Build one account activity page from a canonical account snapshot and its scoped event log.
///
/// The caller owns account/venue authorization and validates the snapshot hash before passing
/// the canonical `AccountSnapshot::to_json()` document here. This reader validates the public
/// wire identity, bounds all returned collections, and never aggregates money across currencies.
pub fn read_account_activity_report_v1(
    snapshot: &Value,
    events: &[Event],
    options: &AccountActivityReportReadOptionsV1,
    now_ms: u64,
) -> Result<AccountActivityReportV1, AppError> {
    validate_options(options)?;
    let header = snapshot
        .get("header")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("账户快照缺少 header 对象"))?;
    if snapshot.get("protocol").and_then(Value::as_str) != Some("QIANXING_ACCOUNT") {
        return Err(invalid("账户活动报告只接受 QIANXING_ACCOUNT 快照"));
    }
    let identity = AccountActivityIdentityV1 {
        account_id: non_empty_header(header, "account_id")?,
        venue_id: non_empty_header(header, "venue_id")?,
        portfolio_id: non_empty_header(header, "portfolio_id")?,
        snapshot_id: header_u64(header, "snapshot_id")?,
        snapshot_event_seq: header_u64(header, "event_seq")?,
        snapshot_state_hash: header_u64(header, "state_hash")?.to_string(),
        snapshot_as_of_ms: header_u64(header, "as_of")?,
    };

    let mut previous_seq = None;
    let mut event_context_coverage = AccountActivityEventContextCoverageV1::default();
    for event in events {
        if previous_seq.is_some_and(|previous| event.seq <= previous) {
            return Err(invariant("账户投影 EventLog 序号不是严格递增"));
        }
        previous_seq = Some(event.seq);
        event_context_coverage.observe(event);
    }
    let selected_page = select_event_page(events, options)?;
    let event_rows = selected_page.rows;
    let first_seq = event_rows.first().map(|event| event.seq);
    let last_seq = event_rows.last().map(|event| event.seq);
    let page_rows = event_rows
        .iter()
        .map(|event| {
            AccountActivityEventRowV1::from_event(&identity.account_id, &identity.venue_id, event)
        })
        .collect::<Vec<_>>();

    let balances = object_fields(
        snapshot,
        &[
            "cash_raw",
            "cash_exact",
            "equity_raw",
            "available_raw",
            "margin_raw",
            "frozen_raw",
            "realized_pnl_raw",
            "unrealized_pnl_raw",
            "fees_raw",
            "fees_by_currency_raw",
            "funding_raw",
        ],
        "账户余额",
    )?;
    let positions = snapshot_rows(snapshot, "positions", "instrument")?;
    let orders = snapshot_rows(snapshot, "orders", "order_id")?;
    let fills = snapshot_rows(snapshot, "fills", "ts")?;
    let transfers = snapshot_rows(snapshot, "transfers", "ts")?;
    let reconciliation = snapshot
        .get("reconcile")
        .cloned()
        .ok_or_else(|| invariant("账户快照缺少 reconcile 状态"))?;

    let page_oldest = first_seq;
    let page_newest = last_seq;
    Ok(AccountActivityReportV1 {
        schema_version: ACCOUNT_ACTIVITY_REPORT_SCHEMA_VERSION_V1,
        as_of_age_ms: now_ms.checked_sub(identity.snapshot_as_of_ms),
        identity,
        balances: Value::Object(balances),
        positions: positions.clone(),
        orders: orders.clone(),
        fills: fills.clone(),
        transfers: transfers.clone(),
        events: AccountActivityEventPageV1 {
            available_total: selected_page.total,
            client_order_id: options.client_order_id,
            exchange_order_id: options.exchange_order_id.clone(),
            run_id: options.run_id.clone(),
            strategy_id: options.strategy_id.clone(),
            correlation_id: options.correlation_id.clone(),
            oldest_seq: selected_page.oldest_seq,
            latest_seq: selected_page.latest_seq,
            requested_after: options.after,
            requested_before: options.before,
            limit: options.limit,
            has_older: selected_page.has_older,
            has_newer: selected_page.has_newer,
            previous_before: selected_page.has_older.then_some(page_oldest).flatten(),
            next_after: if selected_page.has_newer {
                page_newest.or_else(|| options.before.map(|before| before.saturating_sub(1)))
            } else {
                None
            },
            rows: page_rows,
        },
        reconciliation: Value::Object(
            reconciliation
                .as_object()
                .cloned()
                .ok_or_else(|| invariant("账户快照 reconcile 必须是 JSON 对象"))?,
        ),
        completeness: AccountActivityCompletenessV1 {
            snapshot_available: true,
            event_history_scope: "retained_account_projection_event_log".into(),
            event_context_coverage,
            equity_reported: has_non_null(snapshot, "equity_raw"),
            available_reported: has_non_null(snapshot, "available_raw"),
            margin_reported: has_non_null(snapshot, "margin_raw"),
            realized_pnl_reported: has_non_null(snapshot, "realized_pnl_raw"),
            unrealized_pnl_reported: has_non_null(snapshot, "unrealized_pnl_raw"),
            fees_reported: has_non_null(snapshot, "fees_raw")
                || has_non_empty_object(snapshot, "fees_by_currency_raw"),
            funding_reported: has_non_null(snapshot, "funding_raw"),
            reconciliation_reported: reconciliation_has_report(&reconciliation),
            positions_truncated: positions.truncated,
            orders_truncated: orders.truncated,
            fills_truncated: fills.truncated,
            transfers_truncated: transfers.truncated,
        },
    })
}

fn validate_options(options: &AccountActivityReportReadOptionsV1) -> Result<(), AppError> {
    if !(1..=MAX_ACCOUNT_ACTIVITY_EVENT_PAGE_V1).contains(&options.limit) {
        return Err(invalid(format!(
            "账户活动事件 limit 必须在 1 到 {MAX_ACCOUNT_ACTIVITY_EVENT_PAGE_V1} 之间"
        )));
    }
    if options.after.is_some() && options.before.is_some() {
        return Err(invalid("账户活动 after 与 before 不能同时提供"));
    }
    if options
        .exchange_order_id
        .as_ref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(invalid("账户活动 exchange_order_id 不能为空"));
    }
    if options
        .run_id
        .as_ref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(invalid("账户活动 run_id 不能为空"));
    }
    if options
        .strategy_id
        .as_ref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(invalid("账户活动 strategy_id 不能为空"));
    }
    if options
        .correlation_id
        .as_ref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(invalid("账户活动 correlation_id 不能为空"));
    }
    Ok(())
}

struct SelectedEventPage<'a> {
    rows: Vec<&'a Event>,
    total: u64,
    oldest_seq: Option<u64>,
    latest_seq: Option<u64>,
    has_older: bool,
    has_newer: bool,
}

fn select_event_page<'a>(
    events: &'a [Event],
    options: &AccountActivityReportReadOptionsV1,
) -> Result<SelectedEventPage<'a>, AppError> {
    let Some(oldest) = events.first().map(|event| event.seq) else {
        if options.after.is_some_and(|after| after > 0)
            || options.before.is_some_and(|before| before > 0)
        {
            return Err(conflict("账户事件游标超出当前空 EventLog"));
        }
        return Ok(SelectedEventPage {
            rows: Vec::new(),
            total: 0,
            oldest_seq: None,
            latest_seq: None,
            has_older: false,
            has_newer: false,
        });
    };
    let latest = events.last().map(|event| event.seq).unwrap_or_default();
    let mut total = 0_u64;
    let mut oldest_matching = None;
    let mut latest_matching = None;
    // Venue IDs are usually present on acknowledgement/fill events only. Join them back to
    // the local order identity so a venue-order search also returns later fee/ledger facts.
    let exchange_order_client_ids = options.exchange_order_id.as_deref().map(|expected| {
        events
            .iter()
            .filter_map(|event| {
                let (client_order_id, exchange_order_id) = order_ids_for_event(event);
                (exchange_order_id.as_deref() == Some(expected))
                    .then_some(client_order_id)
                    .flatten()
            })
            .collect::<HashSet<_>>()
    });
    // A command's registration event may carry the caller's stable correlation ID while venue
    // response facts carry a more specific per-order correlation. Join the anchor back to that
    // order so the filter exposes its full retained lifecycle, including fills and ledger facts.
    let correlation_client_ids = options.correlation_id.as_deref().map(|expected| {
        events
            .iter()
            .filter(|event| event.correlation_id == expected)
            .filter_map(|event| order_ids_for_event(event).0)
            .collect::<HashSet<_>>()
    });
    let is_match = |event: &Event| {
        let (client_order_id, exchange_order_id) = order_ids_for_event(event);
        options
            .client_order_id
            .is_none_or(|expected| client_order_id == Some(expected))
            && options
                .run_id
                .as_deref()
                .is_none_or(|expected| event.metadata.context.run_id == expected)
            && options
                .strategy_id
                .as_deref()
                .is_none_or(|expected| event.metadata.context.strategy_id == expected)
            && options.correlation_id.as_deref().is_none_or(|expected| {
                event.correlation_id == expected
                    || client_order_id.is_some_and(|client_order_id| {
                        correlation_client_ids
                            .as_ref()
                            .is_some_and(|ids| ids.contains(&client_order_id))
                    })
            })
            && options.exchange_order_id.as_deref().is_none_or(|expected| {
                exchange_order_id.as_deref() == Some(expected)
                    || client_order_id.is_some_and(|client_order_id| {
                        exchange_order_client_ids
                            .as_ref()
                            .is_some_and(|ids| ids.contains(&client_order_id))
                    })
            })
    };
    if let Some(after) = options.after {
        if after > latest {
            return Err(conflict("账户事件 after 游标超前于当前 EventLog"));
        }
        if after.saturating_add(1) < oldest {
            return Err(conflict("账户事件 after 游标早于当前保留 EventLog"));
        }
        let mut page = Vec::with_capacity(options.limit);
        let mut has_newer = false;
        for event in events.iter() {
            if !is_match(event) {
                continue;
            }
            total += 1;
            oldest_matching.get_or_insert(event.seq);
            latest_matching = Some(event.seq);
            if event.seq <= after {
                continue;
            }
            if page.len() < options.limit {
                page.push(event);
            } else {
                has_newer = true;
            }
        }
        let has_older = page
            .first()
            .zip(oldest_matching)
            .is_some_and(|(first, oldest)| oldest < first.seq);
        return Ok(SelectedEventPage {
            rows: page,
            total,
            oldest_seq: oldest_matching,
            latest_seq: latest_matching,
            has_older,
            has_newer,
        });
    }
    if let Some(before) = options.before {
        if before > latest.saturating_add(1) {
            return Err(conflict("账户事件 before 游标超前于当前 EventLog"));
        }
        let mut page = VecDeque::with_capacity(options.limit);
        let mut has_newer = false;
        let mut matching_before = 0_u64;
        for event in events.iter() {
            if !is_match(event) {
                continue;
            }
            total += 1;
            oldest_matching.get_or_insert(event.seq);
            latest_matching = Some(event.seq);
            if event.seq >= before {
                has_newer = true;
            } else {
                matching_before += 1;
                if page.len() == options.limit {
                    page.pop_front();
                }
                page.push_back(event);
            }
        }
        let has_older = matching_before > options.limit as u64;
        return Ok(SelectedEventPage {
            rows: page.into_iter().collect(),
            total,
            oldest_seq: oldest_matching,
            latest_seq: latest_matching,
            has_older,
            has_newer,
        });
    }
    let mut page = VecDeque::with_capacity(options.limit);
    for event in events.iter() {
        if !is_match(event) {
            continue;
        }
        total += 1;
        oldest_matching.get_or_insert(event.seq);
        latest_matching = Some(event.seq);
        if page.len() == options.limit {
            page.pop_front();
        }
        page.push_back(event);
    }
    let has_older = total > options.limit as u64;
    Ok(SelectedEventPage {
        rows: page.into_iter().collect(),
        total,
        oldest_seq: oldest_matching,
        latest_seq: latest_matching,
        has_older,
        has_newer: false,
    })
}

fn snapshot_rows(
    snapshot: &Value,
    field: &str,
    sort_key: &str,
) -> Result<AccountActivityRowsV1, AppError> {
    let object = snapshot
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| invariant(format!("账户快照 {field} 必须是 JSON 对象")))?;
    let mut rows = object.values().cloned().collect::<Vec<_>>();
    if sort_key == "ts" {
        rows.sort_by_key(|row| {
            row.get(sort_key)
                .and_then(Value::as_u64)
                .unwrap_or_default()
        });
    }
    let total = rows.len() as u64;
    let truncated = rows.len() > MAX_ACCOUNT_ACTIVITY_SNAPSHOT_ROWS_V1;
    if truncated {
        rows.drain(..rows.len() - MAX_ACCOUNT_ACTIVITY_SNAPSHOT_ROWS_V1);
    }
    Ok(AccountActivityRowsV1 {
        total,
        returned: rows.len(),
        truncated,
        rows,
    })
}

fn object_fields(
    source: &Value,
    fields: &[&str],
    label: &str,
) -> Result<Map<String, Value>, AppError> {
    fields.iter().try_fold(Map::new(), |mut out, field| {
        let value = match source.get(*field) {
            Some(value) => value.clone(),
            None if matches!(*field, "cash_exact" | "fees_by_currency_raw") => {
                Value::Object(Map::new())
            }
            None => return Err(invariant(format!("{label}缺少字段 {field}"))),
        };
        out.insert((*field).to_owned(), value);
        Ok(out)
    })
}

fn non_empty_header(header: &Map<String, Value>, field: &str) -> Result<String, AppError> {
    header
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invariant(format!("账户快照 header.{field} 无效")))
}

fn header_u64(header: &Map<String, Value>, field: &str) -> Result<u64, AppError> {
    header
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| invariant(format!("账户快照 header.{field} 无效")))
}

fn has_non_null(source: &Value, field: &str) -> bool {
    source.get(field).is_some_and(|value| !value.is_null())
}

fn has_non_empty_object(source: &Value, field: &str) -> bool {
    source
        .get(field)
        .and_then(Value::as_object)
        .is_some_and(|value| !value.is_empty())
}

fn reconciliation_has_report(reconcile: &Value) -> bool {
    reconcile
        .get("last_reconcile_ts")
        .is_some_and(|value| !value.is_null())
        || reconcile
            .get("discrepancy_count")
            .is_some_and(|value| !value.is_null())
}

fn default_event_limit() -> usize {
    DEFAULT_ACCOUNT_ACTIVITY_EVENT_PAGE_V1
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn conflict(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::Conflict, message)
}

fn invariant(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{Event, EventKind};

    fn snapshot() -> Value {
        serde_json::json!({
            "protocol": "QIANXING_ACCOUNT",
            "schema_version": 1,
            "header": {
                "snapshot_id": 7,
                "account_id": "primary",
                "portfolio_id": "crypto",
                "venue_id": "OKX",
                "trading_day": "2026-10-25",
                "as_of": 100,
                "event_seq": 3,
                "state_hash": 99
            },
            "cash_raw": {"USDT": "1000000000"},
            "cash_exact": {"USDT": "1"},
            "equity_raw": null,
            "available_raw": "0",
            "margin_raw": null,
            "frozen_raw": null,
            "realized_pnl_raw": "0",
            "unrealized_pnl_raw": null,
            "fees_raw": null,
            "fees_by_currency_raw": {"BNB": "12"},
            "funding_raw": null,
            "positions": {"BTC-USDT.OKX": {"instrument": "BTC-USDT.OKX"}},
            "orders": {},
            "fills": {},
            "transfers": {},
            "reconcile": {"last_reconcile_ts": null, "discrepancy_count": null, "recovery_state": ""}
        })
    }

    fn events() -> Vec<Event> {
        (1..=5)
            .map(|seq| {
                let mut event = Event::new(seq, seq, 0, EventKind::Settle);
                if seq % 2 == 0 {
                    event.metadata.context.run_id = "run-1".into();
                    event.metadata.context.strategy_id = "strategy-a".into();
                } else if seq == 3 {
                    event.metadata.context.run_id = "run-2".into();
                    event.metadata.context.strategy_id = "strategy-b".into();
                }
                event
            })
            .collect()
    }

    #[test]
    fn run_and_strategy_filters_are_exact_and_compose_with_order_filters() {
        let mut events = events();
        events[0].kind = EventKind::ExecutionCapabilityDecision {
            decision: qx_core::ExecutionCapabilityDecisionFact {
                schema_version: qx_core::EXECUTION_CAPABILITY_DECISION_SCHEMA_VERSION,
                kind: qx_core::ExecutionCapabilityDecisionKind::Submit,
                decision_id: "a".repeat(64),
                command_id: Some(11),
                order_group_id: None,
                client_order_id: 77,
                account_id: "primary".into(),
                run_id: Some("run-1".into()),
                strategy_id: Some("strategy-a".into()),
                signal_id: Some(8),
                intent_id: Some(77),
                venue_id: "OKX".into(),
                order_fingerprint: "b".repeat(64),
                catalog_fingerprint: "c".repeat(64),
                market_rule_fingerprint: "d".repeat(64),
                preflight_json: r#"{"schema_version":1}"#.into(),
            },
        };
        events[0].metadata.context.run_id = "run-1".into();
        events[0].metadata.context.strategy_id = "strategy-a".into();
        events[0].metadata.context.signal_id = "8".into();
        events[0].metadata.context.intent_id = "77".into();
        for index in [1, 3] {
            events[index].kind = EventKind::Accepted {
                client_order_id: 77,
                venue_order_id: None,
            };
        }
        let report = read_account_activity_report_v1(
            &snapshot(),
            &events,
            &AccountActivityReportReadOptionsV1 {
                run_id: Some("run-1".into()),
                strategy_id: Some("strategy-a".into()),
                client_order_id: Some(77),
                ..AccountActivityReportReadOptionsV1::default()
            },
            200,
        )
        .unwrap();
        assert_eq!(report.events.available_total, 3);
        assert_eq!(report.events.run_id.as_deref(), Some("run-1"));
        assert_eq!(report.events.strategy_id.as_deref(), Some("strategy-a"));
        assert_eq!(
            report
                .events
                .rows
                .iter()
                .map(|row| row.seq)
                .collect::<Vec<_>>(),
            [1, 2, 4]
        );
        assert_eq!(report.events.rows[0].run_id, "run-1");
        assert_eq!(report.events.rows[0].strategy_id, "strategy-a");

        let empty = read_account_activity_report_v1(
            &snapshot(),
            &events,
            &AccountActivityReportReadOptionsV1 {
                run_id: Some("run-1".into()),
                strategy_id: Some("strategy-b".into()),
                ..AccountActivityReportReadOptionsV1::default()
            },
            200,
        )
        .unwrap();
        assert!(empty.events.rows.is_empty());
        assert_eq!(empty.events.available_total, 0);
    }

    #[test]
    fn report_preserves_unknown_money_and_returns_bidirectional_bounded_event_pages() {
        let events = events();
        let first = read_account_activity_report_v1(
            &snapshot(),
            &events,
            &AccountActivityReportReadOptionsV1 {
                after: Some(1),
                before: None,
                client_order_id: None,
                exchange_order_id: None,
                run_id: None,
                strategy_id: None,
                correlation_id: None,
                limit: 2,
            },
            200,
        )
        .unwrap();
        assert_eq!(first.identity.account_id, "primary");
        assert_eq!(first.identity.venue_id, "OKX");
        assert_eq!(first.as_of_age_ms, Some(100));
        assert_eq!(first.balances["equity_raw"], Value::Null);
        assert_eq!(first.balances["available_raw"], "0");
        assert_eq!(first.events.rows.len(), 2);
        assert_eq!(first.events.next_after, Some(3));
        assert!(first.events.has_older);
        assert!(first.completeness.fees_reported);
        assert!(!first.completeness.equity_reported);

        let previous = read_account_activity_report_v1(
            &snapshot(),
            &events,
            &AccountActivityReportReadOptionsV1 {
                after: None,
                before: Some(3),
                client_order_id: None,
                exchange_order_id: None,
                run_id: None,
                strategy_id: None,
                correlation_id: None,
                limit: 1,
            },
            200,
        )
        .unwrap();
        assert_eq!(previous.events.rows[0].seq, 2);
        assert!(previous.events.has_older);
        assert!(previous.events.has_newer);
    }

    #[test]
    fn account_event_rows_expose_stable_order_and_run_trace_keys() {
        let mut accepted = Event::new(
            8,
            120,
            1,
            EventKind::Accepted {
                client_order_id: 42,
                venue_order_id: Some("venue-9001".into()),
            },
        );
        accepted.correlation_id = "corr-abc".into();
        accepted.metadata.context.run_id = "run-7".into();
        accepted.metadata.context.strategy_id = "momentum".into();

        let report = read_account_activity_report_v1(
            &snapshot(),
            &[accepted.clone()],
            &AccountActivityReportReadOptionsV1::default(),
            200,
        )
        .unwrap();
        let row = &report.events.rows[0];
        assert_eq!(row.event_id, "account-event:v1:7:primary:3:OKX:8");
        assert_eq!(row.client_order_id, Some(42));
        assert_eq!(row.exchange_order_id.as_deref(), Some("venue-9001"));
        assert_eq!(row.run_id, "run-7");
        assert_eq!(row.strategy_id, "momentum");
        assert_eq!(row.correlation_id, "corr-abc");
        assert_eq!(report.completeness.event_context_coverage.total_events, 1);
        assert_eq!(
            report
                .completeness
                .event_context_coverage
                .with_correlation_id,
            1
        );
        assert_eq!(
            report
                .completeness
                .event_context_coverage
                .with_non_account_run_id,
            1
        );
        assert_eq!(
            report.completeness.event_context_coverage.with_strategy_id,
            1
        );
        assert_eq!(
            report
                .completeness
                .event_context_coverage
                .with_client_order_id,
            1
        );
        assert_eq!(
            report
                .completeness
                .event_context_coverage
                .with_exchange_order_id,
            1
        );
        assert_eq!(
            serde_json::to_value(row).unwrap()["kind"]["Accepted"]["venue_order_id"],
            "venue-9001"
        );
    }

    #[test]
    fn account_scope_is_not_counted_as_a_strategy_run_context() {
        let mut event = Event::new(1, 10, 0, EventKind::Settle);
        event.metadata.context.run_id = "account:primary".into();
        event.metadata.context.strategy_id = "external-account-state".into();
        let report = read_account_activity_report_v1(
            &snapshot(),
            &[event],
            &AccountActivityReportReadOptionsV1::default(),
            200,
        )
        .unwrap();
        assert_eq!(report.completeness.event_context_coverage.total_events, 1);
        assert_eq!(
            report
                .completeness
                .event_context_coverage
                .with_non_account_run_id,
            0
        );
        // This reports field presence only; it does not assert this sentinel is a strategy.
        assert_eq!(
            report.completeness.event_context_coverage.with_strategy_id,
            1
        );
    }

    #[test]
    fn context_coverage_remains_optional_for_older_v1_reports() {
        let report = read_account_activity_report_v1(
            &snapshot(),
            &[],
            &AccountActivityReportReadOptionsV1::default(),
            200,
        )
        .unwrap();
        let mut completeness = serde_json::to_value(report.completeness).unwrap();
        completeness
            .as_object_mut()
            .unwrap()
            .remove("event_context_coverage");
        let decoded: AccountActivityCompletenessV1 = serde_json::from_value(completeness).unwrap();
        assert_eq!(
            decoded.event_context_coverage,
            AccountActivityEventContextCoverageV1::default()
        );
    }

    #[test]
    fn account_activity_correlation_anchor_expands_to_the_same_order_lifecycle() {
        let mut events = events();
        events[1].kind = EventKind::Submit {
            client_order_id: 77,
        };
        events[1].correlation_id = "order-submit-77".into();
        events[2].kind = EventKind::Accepted {
            client_order_id: 77,
            venue_order_id: Some("remote-77".into()),
        };
        events[2].correlation_id = "order-submit-77:venue:OKX:77:accepted".into();
        events[3].kind = EventKind::Filled {
            fill: qx_core::Fill {
                order_id: 77,
                venue_order_id: Some("remote-77".into()),
                ..qx_core::Fill::default()
            },
        };
        events[3].correlation_id = "order-submit-77:venue:OKX:77:fill:77".into();
        events[4].kind = EventKind::LedgerApplied {
            entry: qx_core::LedgerEntry {
                id: 1,
                account_id: "primary".into(),
                currency: "USDT".into(),
                kind: qx_core::LedgerEntryKind::TradePosition,
                amount: qx_core::Money::ZERO,
                instrument: None,
                quantity: qx_core::Quantity::ZERO,
                price: None,
                order_id: Some(77),
                ts: 5,
                multiplier: 1,
                position_side: None,
                leverage: None,
                exact_amounts: None,
            },
        };
        events[4].correlation_id = "ledger:77".into();
        let report = read_account_activity_report_v1(
            &snapshot(),
            &events,
            &AccountActivityReportReadOptionsV1 {
                correlation_id: Some("order-submit-77".into()),
                ..AccountActivityReportReadOptionsV1::default()
            },
            200,
        )
        .unwrap();
        assert_eq!(report.events.available_total, 4);
        assert_eq!(
            report.events.correlation_id.as_deref(),
            Some("order-submit-77")
        );
        assert_eq!(
            report
                .events
                .rows
                .iter()
                .map(|row| row.seq)
                .collect::<Vec<_>>(),
            [2, 3, 4, 5]
        );
        assert_eq!(report.events.rows[0].correlation_id, "order-submit-77");
    }

    #[test]
    fn account_activity_filters_by_client_order_before_cursor_pagination() {
        let mut events = events();
        events[0].kind = EventKind::Accepted {
            client_order_id: 77,
            venue_order_id: Some("remote-77".into()),
        };
        events[1].kind = EventKind::OrderRiskDecision {
            decision: qx_core::OrderRiskDecisionFact {
                client_order_id: 77,
                instrument: qx_core::InstrumentId::parse("BTC/USDT.OKX").unwrap(),
                strategy_id: Some("momentum".into()),
                signal_id: None,
                intent_id: None,
                allowed: true,
                violations: Vec::new(),
                projected_position_raw: None,
                projected_margin_raw: None,
                reference_price_raw: None,
                rule_set_version: "test".into(),
            },
        };
        events[2].kind = EventKind::Filled {
            fill: qx_core::Fill {
                order_id: 77,
                venue_order_id: Some("remote-77".into()),
                ..qx_core::Fill::default()
            },
        };
        events[3].kind = EventKind::LedgerApplied {
            entry: qx_core::LedgerEntry {
                id: 1,
                account_id: "primary".into(),
                currency: "USDT".into(),
                kind: qx_core::LedgerEntryKind::TradePosition,
                amount: qx_core::Money::ZERO,
                instrument: None,
                quantity: qx_core::Quantity::ZERO,
                price: None,
                order_id: Some(77),
                ts: 4,
                multiplier: 1,
                position_side: None,
                leverage: None,
                exact_amounts: None,
            },
        };
        events[4].kind = EventKind::Cancelled {
            client_order_id: 77,
        };

        let first = read_account_activity_report_v1(
            &snapshot(),
            &events,
            &AccountActivityReportReadOptionsV1 {
                after: Some(0),
                before: None,
                client_order_id: Some(77),
                exchange_order_id: None,
                run_id: None,
                strategy_id: None,
                correlation_id: None,
                limit: 1,
            },
            200,
        )
        .unwrap();
        assert_eq!(first.events.available_total, 5);
        assert_eq!(first.events.client_order_id, Some(77));
        assert_eq!(first.events.rows[0].seq, 1);
        assert_eq!(first.events.next_after, Some(1));
        assert!(first.events.has_newer);

        let next = read_account_activity_report_v1(
            &snapshot(),
            &events,
            &AccountActivityReportReadOptionsV1 {
                after: Some(1),
                before: None,
                client_order_id: Some(77),
                exchange_order_id: None,
                run_id: None,
                strategy_id: None,
                correlation_id: None,
                limit: 1,
            },
            200,
        )
        .unwrap();
        assert_eq!(next.events.rows[0].seq, 2);
        assert!(next.events.has_older);

        let previous = read_account_activity_report_v1(
            &snapshot(),
            &events,
            &AccountActivityReportReadOptionsV1 {
                after: None,
                before: Some(2),
                client_order_id: Some(77),
                exchange_order_id: None,
                run_id: None,
                strategy_id: None,
                correlation_id: None,
                limit: 1,
            },
            200,
        )
        .unwrap();
        assert_eq!(previous.events.rows[0].seq, 1);
        assert!(previous.events.has_newer);

        let before_first = read_account_activity_report_v1(
            &snapshot(),
            &events,
            &AccountActivityReportReadOptionsV1 {
                after: None,
                before: Some(1),
                client_order_id: Some(77),
                exchange_order_id: None,
                run_id: None,
                strategy_id: None,
                correlation_id: None,
                limit: 1,
            },
            200,
        )
        .unwrap();
        assert!(before_first.events.rows.is_empty());
        assert!(before_first.events.has_newer);
        assert_eq!(before_first.events.next_after, Some(0));

        let ledger = read_account_activity_report_v1(
            &snapshot(),
            &events,
            &AccountActivityReportReadOptionsV1 {
                after: Some(3),
                before: None,
                client_order_id: Some(77),
                exchange_order_id: None,
                run_id: None,
                strategy_id: None,
                correlation_id: None,
                limit: 1,
            },
            200,
        )
        .unwrap();
        assert_eq!(ledger.events.rows[0].seq, 4);
        assert!(matches!(
            &ledger.events.rows[0].kind,
            EventKind::LedgerApplied { .. }
        ));

        let unmatched = read_account_activity_report_v1(
            &snapshot(),
            &events,
            &AccountActivityReportReadOptionsV1 {
                after: Some(0),
                before: None,
                client_order_id: Some(999),
                exchange_order_id: None,
                run_id: None,
                strategy_id: None,
                correlation_id: None,
                limit: 1,
            },
            200,
        )
        .unwrap();
        assert!(unmatched.events.rows.is_empty());
        assert_eq!(unmatched.events.available_total, 0);

        let venue_order = read_account_activity_report_v1(
            &snapshot(),
            &events,
            &AccountActivityReportReadOptionsV1 {
                after: None,
                before: None,
                client_order_id: None,
                exchange_order_id: Some("remote-77".into()),
                run_id: None,
                strategy_id: None,
                correlation_id: None,
                limit: 10,
            },
            200,
        )
        .unwrap();
        assert_eq!(venue_order.events.available_total, 5);
        assert_eq!(
            venue_order.events.exchange_order_id.as_deref(),
            Some("remote-77")
        );
        assert_eq!(
            venue_order
                .events
                .rows
                .iter()
                .map(|row| row.seq)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        let venue_order_next_page = read_account_activity_report_v1(
            &snapshot(),
            &events,
            &AccountActivityReportReadOptionsV1 {
                after: Some(3),
                before: None,
                client_order_id: None,
                exchange_order_id: Some("remote-77".into()),
                run_id: None,
                strategy_id: None,
                correlation_id: None,
                limit: 1,
            },
            200,
        )
        .unwrap();
        assert_eq!(venue_order_next_page.events.rows[0].seq, 4);
        assert!(venue_order_next_page.events.has_older);
        assert!(venue_order_next_page.events.has_newer);
    }

    #[test]
    fn report_rejects_wrong_identity_cursor_conflicts_and_unbounded_limits() {
        let events = events();
        let mut wrong = snapshot();
        wrong["protocol"] = Value::String("OTHER".into());
        assert_eq!(
            read_account_activity_report_v1(
                &wrong,
                &events,
                &AccountActivityReportReadOptionsV1::default(),
                200
            )
            .unwrap_err()
            .category(),
            AppErrorCategory::InvalidInput
        );
        for options in [
            AccountActivityReportReadOptionsV1 {
                run_id: Some("  ".into()),
                ..AccountActivityReportReadOptionsV1::default()
            },
            AccountActivityReportReadOptionsV1 {
                strategy_id: Some("".into()),
                ..AccountActivityReportReadOptionsV1::default()
            },
        ] {
            assert_eq!(
                read_account_activity_report_v1(&snapshot(), &events, &options, 200)
                    .unwrap_err()
                    .category(),
                AppErrorCategory::InvalidInput
            );
        }
        assert_eq!(
            read_account_activity_report_v1(
                &snapshot(),
                &events,
                &AccountActivityReportReadOptionsV1 {
                    after: Some(5),
                    before: Some(4),
                    client_order_id: None,
                    exchange_order_id: None,
                    run_id: None,
                    strategy_id: None,
                    correlation_id: None,
                    limit: 2,
                },
                200
            )
            .unwrap_err()
            .category(),
            AppErrorCategory::InvalidInput
        );
        assert_eq!(
            read_account_activity_report_v1(
                &snapshot(),
                &events,
                &AccountActivityReportReadOptionsV1 {
                    after: Some(6),
                    before: None,
                    client_order_id: None,
                    exchange_order_id: None,
                    run_id: None,
                    strategy_id: None,
                    correlation_id: None,
                    limit: 2,
                },
                200
            )
            .unwrap_err()
            .category(),
            AppErrorCategory::Conflict
        );
        assert_eq!(
            read_account_activity_report_v1(
                &snapshot(),
                &events,
                &AccountActivityReportReadOptionsV1 {
                    after: None,
                    before: None,
                    client_order_id: None,
                    exchange_order_id: None,
                    run_id: None,
                    strategy_id: None,
                    correlation_id: None,
                    limit: MAX_ACCOUNT_ACTIVITY_EVENT_PAGE_V1 + 1,
                },
                200
            )
            .unwrap_err()
            .category(),
            AppErrorCategory::InvalidInput
        );
    }
}
