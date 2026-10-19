use super::{n, t, Cell};
use serde_json::Value;

pub(super) fn execution_attempt_rows(attempts: &[Value]) -> Vec<Vec<Cell>> {
    let columns = [
        "ts",
        "order_id",
        "instrument",
        "side",
        "requested_qty_raw",
        "filled_qty_raw",
        "remaining_qty_raw",
        "price_raw",
        "displayed_qty_raw",
        "queue_ahead_before_raw",
        "queue_ahead_after_raw",
        "estimated_volume_raw",
        "displayed_reduction_raw",
        "queue_consumed_raw",
        "reason",
    ];
    let mut rows = vec![columns.iter().map(|column| t(*column)).collect()];
    rows.extend(attempts.iter().map(|attempt| {
        columns
            .iter()
            .map(|column| match attempt.get(*column) {
                Some(Value::String(value)) => t(value),
                Some(Value::Number(value)) => n(value.to_string()),
                Some(Value::Null) | None => t("—"),
                Some(value) => t(value.to_string()),
            })
            .collect()
    }));
    rows
}
pub(super) fn monthly_rows(monthly: &[(i32, u32, i64)]) -> Vec<Vec<Cell>> {
    let mut rows = vec![vec![t("year"), t("month"), t("return_bps")]];
    rows.extend(monthly.iter().map(|(year, month, returns)| {
        vec![
            n(year.to_string()),
            n(month.to_string()),
            n(returns.to_string()),
        ]
    }));
    rows
}
