use super::{parse_json_action_type, AshareCorporateActionType, AshareRuleConfig};
use serde_json::Value;

/// Validate one or more public company-action rows through the rule-domain path.
/// Suspension rows are projected to a no-op action for numeric validation because
/// the run loader stores their timestamps in a separate halt schedule.
pub fn validate_corporate_action_json(payload: &str) -> Result<(), String> {
    let mut document: Value =
        serde_json::from_str(payload).map_err(|error| format!("公司行为 JSON 无效: {error}"))?;
    let instrument = document
        .get("instrument")
        .and_then(Value::as_str)
        .ok_or_else(|| "A 股公司行为 JSON 缺少 instrument".to_owned())?
        .to_owned();
    let actions = document
        .get_mut("actions")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "公司行为 actions 必须是数组".to_owned())?;
    for (index, action) in actions.iter_mut().enumerate() {
        let kind = action
            .get("action_type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let parsed = parse_json_action_type(kind)
            .ok_or_else(|| format!("公司行为 actions[{index}] action_type 非法: {kind}"))?;
        if parsed == AshareCorporateActionType::Suspension {
            action["action_type"] = Value::String("unknown".to_owned());
        }
    }
    let validation_payload = serde_json::to_string(&document)
        .map_err(|error| format!("公司行为 JSON 无法编码: {error}"))?;
    AshareRuleConfig::default()
        .apply_corporate_actions_json(&instrument, &validation_payload)
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::validate_corporate_action_json;

    #[test]
    fn validates_business_fields_including_suspension_rows() {
        let valid = r#"{"instrument":"000001.SZSE","actions":[{"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"cash_dividend","share_ratio_num":1,"share_ratio_den":1,"cash_dividend_raw":1}]}"#;
        assert!(validate_corporate_action_json(valid).is_ok());
        let invalid_suspension = r#"{"instrument":"000001.SZSE","actions":[{"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"suspension","cash_dividend_raw":-1}]}"#;
        assert!(validate_corporate_action_json(invalid_suspension).is_err());
        let invalid_type = r#"{"instrument":"000001.SZSE","actions":[{"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"not-a-real-action"}]}"#;
        assert!(validate_corporate_action_json(invalid_type).is_err());
    }
}
