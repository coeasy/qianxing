//! A 股数据契约的 JSON 字段读取工具：整数/日期/PIT 时间戳取值、未知字段守卫与动作类型解析。
//!
//! 从 `ashare.rs` 纯搬家拆出：只负责把 `serde_json` 值安全地读成契约字段，
//! 不做任何市场规则判定。

use super::*;

pub(crate) fn json_i128(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<i128, String> {
    let Some(value) = object.get(field) else {
        return Ok(0);
    };
    if let Some(number) = value.as_i64() {
        return Ok(i128::from(number));
    }
    if let Some(text) = value.as_str() {
        return text
            .parse::<i128>()
            .map_err(|error| format!("{field} 必须是整数: {error}"));
    }
    Err(format!("{field} 必须是整数"))
}

pub(crate) fn json_optional_i128(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<i128>, String> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    json_i128(object, field).map(Some)
}

pub(crate) fn json_optional_date(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<u64>, String> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let text = value
        .as_str()
        .ok_or_else(|| format!("{field} 必须是 YYYY-MM-DD 日期字符串"))?;
    date_to_shanghai_midnight_ms(text)
        .map(Some)
        .map_err(|error| format!("{field} 非法: {error}"))
}

pub(crate) fn json_i128_or(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    default: i128,
) -> Result<i128, String> {
    if object.get(field).is_none() {
        Ok(default)
    } else {
        json_i128(object, field)
    }
}

/// 读取文档顶层 `schema_version`；缺省视为 `0`（旧格式兼容分支）。
pub(crate) fn json_schema_version(
    object: &serde_json::Map<String, serde_json::Value>,
    label: &str,
) -> Result<u32, String> {
    match object.get("schema_version") {
        None => Ok(0),
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| format!("{label} schema_version 必须是非负整数")),
    }
}

/// 严格模式的未知字段守卫：Python 侧新增字段而 Rust 尚未识别时必须报错，
/// 而不是把契约漂移降级成“静默丢掉一列”。
pub(crate) fn reject_unknown_fields(
    object: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
    label: &str,
) -> Result<(), String> {
    let mut unknown: Vec<&str> = object
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .map(|key| key.as_str())
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort_unstable();
    Err(format!("{label} schema_version>=1 含未知字段: {unknown:?}"))
}

/// `published_at` 等 PIT 时间字段：允许 epoch 毫秒整数或 ISO-8601 文本。
pub(crate) fn json_optional_timestamp(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<u64>, String> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    timestamp_to_ms(value).map(Some)
}

/// 原始字段快照：只接受对象或 `null`，严格模式禁止写入非对象残留值。
pub(crate) fn json_raw_payload(
    object: &serde_json::Map<String, serde_json::Value>,
    strict: bool,
) -> Result<Option<serde_json::Value>, String> {
    let Some(value) = object.get("raw_payload") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    if strict && !value.is_object() {
        return Err("raw_payload 必须是对象".into());
    }
    Ok(Some(value.clone()))
}

/// 把 JSON 值解释为 epoch 毫秒时间戳。
pub(crate) fn timestamp_to_ms(value: &serde_json::Value) -> Result<u64, String> {
    if let Some(number) = value.as_u64() {
        return Ok(number);
    }
    if let Some(number) = value.as_i64() {
        return u64::try_from(number).map_err(|_| "时间戳不能早于 Unix epoch".to_string());
    }
    let text = value
        .as_str()
        .ok_or_else(|| "时间必须是 epoch 毫秒或 ISO-8601 字符串".to_string())?;
    timestamp_text_to_ms(text)
}

pub(crate) fn parse_json_action_type(value: &str) -> Option<AshareCorporateActionType> {
    Some(match value {
        "cash_dividend" | "dividend" => AshareCorporateActionType::CashDividend,
        "bonus_share" => AshareCorporateActionType::BonusShare,
        "capital_transfer" => AshareCorporateActionType::CapitalTransfer,
        "rights_issue" => AshareCorporateActionType::RightsIssue,
        "rights_issue_expiry" | "rights_expiry" => AshareCorporateActionType::RightsIssueExpiry,
        "new_share_issue" => AshareCorporateActionType::NewShareIssue,
        "repurchase" => AshareCorporateActionType::Repurchase,
        "convertible_bond_issue" => AshareCorporateActionType::ConvertibleBondIssue,
        "convertible_bond_interest" | "bond_interest" => {
            AshareCorporateActionType::ConvertibleBondInterest
        }
        "convertible_bond_redemption" | "bond_redemption" => {
            AshareCorporateActionType::ConvertibleBondRedemption
        }
        "convertible_bond_call" | "bond_call" => AshareCorporateActionType::ConvertibleBondCall,
        "convertible_bond_put" | "bond_put" => AshareCorporateActionType::ConvertibleBondPut,
        "convertible_bond_conversion" => AshareCorporateActionType::ConvertibleBondConversion,
        "suspension" => AshareCorporateActionType::Suspension,
        "capital_change" => AshareCorporateActionType::CapitalChange,
        "unknown" => AshareCorporateActionType::Unknown,
        _ => return None,
    })
}
