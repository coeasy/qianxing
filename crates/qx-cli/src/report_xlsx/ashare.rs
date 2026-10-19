use super::{t, Cell};
use serde_json::Value;

pub(super) fn analysis_rows(analysis: &Value) -> Result<Vec<Vec<Cell>>, String> {
    let mut rows = vec![vec![
        t("kind"),
        t("instrument"),
        t("currency"),
        t("effective_ts"),
        t("total_shares_raw"),
        t("free_float_shares_raw"),
        t("action_kinds"),
        t("rights_entitlement_raw"),
        t("cash_dividend_entitlement_raw"),
        t("convertible_bond_interest_entitlement_raw"),
        t("source"),
    ]];
    for snapshot in rows_at(analysis, "issuer_capital_snapshots")? {
        rows.push(vec![
            t("issuer_capital"),
            scalar(snapshot, "instrument")?,
            t("—"),
            scalar(snapshot, "effective_ts")?,
            scalar(snapshot, "total_shares_raw")?,
            optional_scalar(snapshot, "free_float_shares_raw"),
            t("—"),
            t("—"),
            t("—"),
            t("—"),
            scalar(snapshot, "source")?,
        ]);
    }
    if let Some(snapshot) = analysis
        .get("issuer_capital_at_end")
        .filter(|value| !value.is_null())
    {
        rows.push(vec![
            t("issuer_capital_at_end"),
            scalar(snapshot, "instrument")?,
            t("—"),
            scalar(snapshot, "effective_ts")?,
            scalar(snapshot, "total_shares_raw")?,
            optional_scalar(snapshot, "free_float_shares_raw"),
            t("—"),
            t("—"),
            t("—"),
            t("—"),
            scalar(snapshot, "source")?,
        ]);
    }
    for entitlement in rows_at(analysis, "entitlements")? {
        let kinds = entitlement
            .get("action_kinds")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .unwrap_or_default();
        rows.push(vec![
            t("entitlement"),
            scalar(entitlement, "instrument")?,
            scalar(entitlement, "currency")?,
            t("—"),
            t("—"),
            t("—"),
            t(kinds),
            optional_scalar(entitlement, "rights_entitlement_raw"),
            optional_scalar(entitlement, "cash_dividend_entitlement_raw"),
            optional_scalar(entitlement, "convertible_bond_interest_entitlement_raw"),
            t("—"),
        ]);
    }
    Ok(rows)
}

fn rows_at<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("A 股分析缺少 {key}"))
}

fn scalar(value: &Value, key: &str) -> Result<Cell, String> {
    value
        .get(key)
        .filter(|value| !value.is_null())
        .ok_or_else(|| format!("A 股分析缺少 {key}"))
        .and_then(|value| match value {
            Value::String(text) => Ok(t(text)),
            Value::Number(number) => Ok(t(number.to_string())),
            _ => Err(format!("A 股分析字段 {key} 必须是标量")),
        })
}

fn optional_scalar(value: &Value, key: &str) -> Cell {
    value.get(key).filter(|value| !value.is_null()).map_or_else(
        || t("—"),
        |value| {
            t(value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string))
        },
    )
}
