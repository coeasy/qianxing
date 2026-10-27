//! Bounded, identity-scoped account activity projections.

use crate::{AppError, AppErrorCategory};
use qx_core::Event;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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
    #[serde(default = "default_event_limit")]
    pub limit: usize,
}

impl Default for AccountActivityReportReadOptionsV1 {
    fn default() -> Self {
        Self {
            after: None,
            before: None,
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
    /// Number of rows currently available in this account projection's EventLog.
    pub available_total: u64,
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
    pub rows: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountActivityCompletenessV1 {
    pub snapshot_available: bool,
    /// The report describes the projection's retained log and does not infer venue history
    /// before that log's first row.
    pub event_history_scope: String,
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
    for event in events {
        if previous_seq.is_some_and(|previous| event.seq <= previous) {
            return Err(invariant("账户投影 EventLog 序号不是严格递增"));
        }
        previous_seq = Some(event.seq);
    }
    let (event_rows, has_older, has_newer) = select_event_page(events, options)?;
    let first_seq = event_rows.first().map(|event| event.seq);
    let last_seq = event_rows.last().map(|event| event.seq);
    let page_rows = event_rows
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| invariant(format!("账户事件序列化失败: {error}")))?;

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
            available_total: events.len() as u64,
            oldest_seq: events.first().map(|event| event.seq),
            latest_seq: events.last().map(|event| event.seq),
            requested_after: options.after,
            requested_before: options.before,
            limit: options.limit,
            has_older,
            has_newer,
            previous_before: has_older.then_some(page_oldest).flatten(),
            next_after: has_newer.then_some(page_newest).flatten(),
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
    Ok(())
}

fn select_event_page<'a>(
    events: &'a [Event],
    options: &AccountActivityReportReadOptionsV1,
) -> Result<(Vec<&'a Event>, bool, bool), AppError> {
    let Some(oldest) = events.first().map(|event| event.seq) else {
        if options.after.is_some_and(|after| after > 0)
            || options.before.is_some_and(|before| before > 0)
        {
            return Err(conflict("账户事件游标超出当前空 EventLog"));
        }
        return Ok((Vec::new(), false, false));
    };
    let latest = events.last().map(|event| event.seq).unwrap_or_default();
    if let Some(after) = options.after {
        if after > latest {
            return Err(conflict("账户事件 after 游标超前于当前 EventLog"));
        }
        if after.saturating_add(1) < oldest {
            return Err(conflict("账户事件 after 游标早于当前保留 EventLog"));
        }
        let matching = events
            .iter()
            .filter(|event| event.seq > after)
            .collect::<Vec<_>>();
        let has_newer = matching.len() > options.limit;
        let page: Vec<&Event> = matching.iter().take(options.limit).copied().collect();
        let has_older = page
            .first()
            .is_some_and(|first| events.iter().any(|event| event.seq < first.seq));
        return Ok((page, has_older, has_newer));
    }
    if let Some(before) = options.before {
        if before > latest.saturating_add(1) {
            return Err(conflict("账户事件 before 游标超前于当前 EventLog"));
        }
        let matching = events
            .iter()
            .filter(|event| event.seq < before)
            .collect::<Vec<_>>();
        let start = matching.len().saturating_sub(options.limit);
        let page = matching[start..].to_vec();
        let has_older = start > 0;
        let has_newer = page
            .last()
            .is_some_and(|last| events.iter().any(|event| event.seq > last.seq));
        return Ok((page, has_older, has_newer));
    }
    let start = events.len().saturating_sub(options.limit);
    let page = events[start..].iter().collect::<Vec<_>>();
    Ok((page, start > 0, false))
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
            .map(|seq| Event::new(seq, seq, 0, EventKind::Settle))
            .collect()
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
        assert_eq!(first.completeness.fees_reported, true);
        assert_eq!(first.completeness.equity_reported, false);

        let previous = read_account_activity_report_v1(
            &snapshot(),
            &events,
            &AccountActivityReportReadOptionsV1 {
                after: None,
                before: Some(3),
                limit: 1,
            },
            200,
        )
        .unwrap();
        assert_eq!(previous.events.rows[0]["seq"], 2);
        assert!(previous.events.has_older);
        assert!(previous.events.has_newer);
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
        assert_eq!(
            read_account_activity_report_v1(
                &snapshot(),
                &events,
                &AccountActivityReportReadOptionsV1 {
                    after: Some(5),
                    before: Some(4),
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
