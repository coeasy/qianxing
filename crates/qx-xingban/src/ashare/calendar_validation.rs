//! Validation for the public A-share trading-calendar model contract.

use serde_json::Value;

/// Validate the same strict ordering and non-empty constraints exposed by the Python calendar model.
///
/// This is intentionally distinct from rule loading, which can normalize legacy calendars by
/// sorting and deduplicating their dates before constructing a runtime schedule.
pub fn validate_ashare_calendar_json(payload: &str) -> Result<(), String> {
    let document: Value =
        serde_json::from_str(payload).map_err(|error| format!("交易日历 JSON 无效: {error}"))?;
    let calendar_id = document
        .get("calendar_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "calendar_id and trading_days are required".to_owned())?;
    let trading_days = document
        .get("trading_days")
        .and_then(Value::as_array)
        .filter(|days| !days.is_empty())
        .ok_or_else(|| "calendar_id and trading_days are required".to_owned())?;
    if calendar_id.trim().is_empty() {
        return Err("calendar_id and trading_days are required".to_owned());
    }
    let days = trading_days
        .iter()
        .map(|day| {
            day.as_str()
                .ok_or_else(|| "trading_days must be strictly sorted".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if days.windows(2).any(|window| window[0] >= window[1]) {
        return Err("trading_days must be strictly sorted".to_owned());
    }
    let Some(sessions) = document.get("sessions") else {
        return Ok(());
    };
    let sessions = sessions
        .as_array()
        .ok_or_else(|| "calendar session start must be before end".to_owned())?;
    for session in sessions {
        let parts = session
            .as_array()
            .filter(|parts| parts.len() == 2)
            .ok_or_else(|| "calendar session start must be before end".to_owned())?;
        let start = parts[0]
            .as_str()
            .ok_or_else(|| "calendar session start must be before end".to_owned())?;
        let end = parts[1]
            .as_str()
            .ok_or_else(|| "calendar session start must be before end".to_owned())?;
        if start >= end {
            return Err("calendar session start must be before end".to_owned());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_ashare_calendar_json;

    #[test]
    fn matches_the_public_calendar_model_validation_contract() {
        let valid = r#"{"calendar_id":"cn","trading_days":["2024-01-02","2024-01-03"],"sessions":[["09:30","11:30"],["13:00","15:00"]]}"#;
        assert!(validate_ashare_calendar_json(valid).is_ok());
        assert_eq!(
            validate_ashare_calendar_json(
                r#"{"calendar_id":"cn","trading_days":["2024-01-03","2024-01-02"]}"#
            )
            .unwrap_err(),
            "trading_days must be strictly sorted"
        );
        assert_eq!(
            validate_ashare_calendar_json(
                r#"{"calendar_id":"cn","trading_days":["2024-01-02"],"sessions":[["15:00","09:30"]]}"#
            )
            .unwrap_err(),
            "calendar session start must be before end"
        );
    }
}
