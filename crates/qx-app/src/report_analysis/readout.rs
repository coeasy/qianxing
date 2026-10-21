//! Shared summary JSON accessors used by report projections and presentation adapters.

use serde_json::Value;

pub fn summary_block<'a>(summary: &'a Value, key: &str) -> Option<&'a Value> {
    summary.get(key).filter(|value| !value.is_null())
}

pub fn summary_number(summary: &Value, pointer: &str) -> Option<i128> {
    match summary.pointer(pointer)? {
        Value::Null => None,
        Value::String(text) => text.trim().parse::<i128>().ok(),
        value @ Value::Number(_) => value.to_string().parse::<i128>().ok(),
        _ => None,
    }
}

pub fn summary_text(summary: &Value, pointer: &str) -> Option<String> {
    match summary.pointer(pointer)? {
        Value::String(text) => Some(text.clone()),
        Value::Null => None,
        value @ Value::Number(_) => Some(value.to_string()),
        _ => None,
    }
}
