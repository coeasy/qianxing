//! Deterministic merge and conflict reporting for normalized A-share corporate actions.

use serde_json::{Map, Value};
use std::collections::BTreeMap;

const COMPARABLE_FIELDS: &[&str] = &[
    "cash_dividend_raw",
    "share_ratio_num",
    "share_ratio_den",
    "record_date",
    "payment_date",
    "rights_issue_price_raw",
    "rights_issue_ratio_num",
    "rights_issue_ratio_den",
    "issue_price_raw",
    "conversion_price_raw",
    "conversion_ratio_num",
    "conversion_ratio_den",
    "rights_instrument",
    "subscription_qty_raw",
    "rights_expiry_qty_raw",
    "repurchase_qty_raw",
    "repurchase_price_raw",
    "convertible_bond_instrument",
    "conversion_target_instrument",
    "conversion_qty_raw",
    "conversion_target_qty_raw",
];

fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

fn is_empty(value: &Value) -> bool {
    value.is_null()
        || value.as_i64() == Some(0)
        || value.as_u64() == Some(0)
        || value.as_i64() == Some(1)
        || value.as_u64() == Some(1)
        || value.as_str() == Some("")
}

fn action_key(action: &Value) -> Result<(String, String, String), String> {
    let read = |name: &str| {
        action
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("corporate action {name} must be a string"))
    };
    Ok((read("instrument")?, read("ex_date")?, read("action_type")?))
}

fn merge_sources(sources: Vec<Vec<Value>>) -> Result<Value, String> {
    let mut merged: BTreeMap<(String, String, String), Value> = BTreeMap::new();
    let mut conflicts = Vec::new();

    for actions in sources {
        for action in actions {
            let key = action_key(&action)?;
            let Some(current) = merged.get_mut(&key) else {
                merged.insert(key, action);
                continue;
            };

            let mut updates = Map::new();
            for name in COMPARABLE_FIELDS {
                let left = field(current, name);
                let right = field(&action, name);
                let left_empty = is_empty(left);
                let right_empty = is_empty(right);
                if left_empty && !right_empty {
                    updates.insert((*name).to_owned(), right.clone());
                } else if !right_empty && left != right {
                    conflicts.push(serde_json::json!({
                        "instrument": key.0,
                        "ex_date": key.1,
                        "action_type": key.2,
                        "field": name,
                        "left": left,
                        "right": right,
                        "left_source": field(current, "source"),
                        "right_source": field(&action, "source"),
                    }));
                }
            }

            let mut raw_payload = field(current, "raw_payload")
                .as_object()
                .cloned()
                .unwrap_or_default();
            if let Some(right_raw) = field(&action, "raw_payload").as_object() {
                raw_payload.extend(right_raw.clone());
            }
            updates.insert("raw_payload".to_owned(), Value::Object(raw_payload));

            let mut sources = Vec::new();
            for source in [field(current, "source"), field(&action, "source")] {
                if let Some(text) = source.as_str().filter(|text| !text.is_empty()) {
                    if !sources.iter().any(|existing: &String| existing == text) {
                        sources.push(text.to_owned());
                    }
                }
            }
            updates.insert("source".to_owned(), Value::String(sources.join("+")));

            if let Some(object) = current.as_object_mut() {
                object.extend(updates);
            } else {
                return Err("corporate action must be a JSON object".to_owned());
            }
        }
    }

    let actions = merged.into_values().collect::<Vec<_>>();
    Ok(serde_json::json!({ "actions": actions, "conflicts": conflicts }))
}

/// Merge normalized rows by `(instrument, ex_date, action_type)` and return all field conflicts.
pub fn reconcile_corporate_actions_json(payload: &str) -> Result<String, String> {
    let sources: Vec<Vec<Value>> =
        serde_json::from_str(payload).map_err(|error| error.to_string())?;
    let result = merge_sources(sources)?;
    serde_json::to_string(&result).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::reconcile_corporate_actions_json;
    use serde_json::{json, Value};

    #[test]
    fn merges_missing_fields_and_reports_conflicts_in_source_order() {
        let payload = json!([
            [{
                "instrument":"000001.SZSE", "ex_date":"2024-06-03", "action_type":"dividend",
                "cash_dividend_raw":0, "share_ratio_num":1, "share_ratio_den":1,
                "source":"akshare", "raw_payload":{"a":1}
            }],
            [
                {
                    "instrument":"000001.SZSE", "ex_date":"2024-06-03", "action_type":"dividend",
                    "cash_dividend_raw":100000000, "share_ratio_num":1, "share_ratio_den":1,
                    "source":"baostock", "raw_payload":{"b":2}
                },
                {
                    "instrument":"000001.SZSE", "ex_date":"2024-06-03", "action_type":"dividend",
                    "cash_dividend_raw":200000000, "share_ratio_num":1, "share_ratio_den":1,
                    "source":"tdx", "raw_payload":{}
                }
            ]
        ]);
        let result: Value =
            serde_json::from_str(&reconcile_corporate_actions_json(&payload.to_string()).unwrap())
                .unwrap();
        assert_eq!(result["actions"].as_array().unwrap().len(), 1);
        assert_eq!(result["actions"][0]["cash_dividend_raw"], 100000000);
        assert_eq!(result["actions"][0]["source"], "akshare+baostock+tdx");
        assert_eq!(result["actions"][0]["raw_payload"], json!({"a":1,"b":2}));
        assert_eq!(result["conflicts"].as_array().unwrap().len(), 1);
        assert_eq!(result["conflicts"][0]["left"], 100000000);
        assert_eq!(result["conflicts"][0]["right"], 200000000);
    }

    #[test]
    fn rejects_rows_without_the_canonical_action_key() {
        let error =
            reconcile_corporate_actions_json(r#"[[{"instrument":"000001.SZSE"}]]"#).unwrap_err();
        assert!(error.contains("ex_date must be a string"));
    }
}
