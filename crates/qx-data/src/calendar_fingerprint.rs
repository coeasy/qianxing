//! Canonical identity for serialized trading-calendar bundle components.

use serde_json::Value;

/// Recompute a calendar component fingerprint and trading-day count from its JSON envelope.
///
/// Only `calendar_id`, `trading_days`, and `sessions` contribute to the identity. Legacy
/// documents without `sessions` use an empty session list, matching the v1 bundle contract.
pub fn calendar_component_fingerprint_from_json(payload: &str) -> Result<(String, u64), String> {
    let value: Value = serde_json::from_str(payload).map_err(|error| error.to_string())?;
    let calendar_id = value
        .get("calendar_id")
        .ok_or_else(|| "calendar 组件缺少 calendar_id".to_owned())?;
    let trading_days = value
        .get("trading_days")
        .and_then(Value::as_array)
        .ok_or_else(|| "calendar 组件缺少 trading_days 数组".to_owned())?;
    let sessions: &[Value] = match value.get("sessions") {
        None => &[],
        Some(value) => value
            .as_array()
            .ok_or_else(|| "calendar 组件的 sessions 必须是数组".to_owned())?,
    };
    // Preserve the long-standing canonical key order used by DatasetBundle identities.
    let canonical = format!(
        "{{\"calendar_id\":{},\"trading_days\":{},\"sessions\":{}}}",
        serde_json::to_string(calendar_id).map_err(|error| error.to_string())?,
        serde_json::to_string(trading_days).map_err(|error| error.to_string())?,
        serde_json::to_string(sessions).map_err(|error| error.to_string())?
    );
    Ok((
        crate::sha256_hex_bytes(canonical.as_bytes()),
        trading_days.len() as u64,
    ))
}

#[cfg(test)]
mod tests {
    use super::calendar_component_fingerprint_from_json;
    use serde_json::Value;
    use std::path::Path;

    fn fixture(name: &str) -> String {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python/tests/fixtures");
        std::fs::read_to_string(root.join(name)).unwrap()
    }

    fn pinned(name: &str) -> String {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python/tests/fixtures");
        std::fs::read_to_string(root.join(name))
            .unwrap()
            .trim()
            .to_owned()
    }

    #[test]
    fn calendar_identity_matches_the_shared_python_fixtures() {
        for (document, digest, expected_rows) in [
            (
                "calendar-component-v1.json",
                "calendar-component-v1.fingerprint",
                3,
            ),
            (
                "calendar-component-legacy.json",
                "calendar-component-legacy.fingerprint",
                2,
            ),
        ] {
            let (actual, rows) =
                calendar_component_fingerprint_from_json(&fixture(document)).unwrap();
            assert_eq!(actual, pinned(digest));
            assert_eq!(rows, expected_rows);
        }
    }

    #[test]
    fn calendar_identity_ignores_envelope_fields_and_rejects_bad_shape() {
        let source = fixture("calendar-component-v1.json");
        let mut value: Value = serde_json::from_str(&source).unwrap();
        let (baseline, _) = calendar_component_fingerprint_from_json(&source).unwrap();
        value["source"] = Value::String("another-provider".to_owned());
        assert_eq!(
            calendar_component_fingerprint_from_json(&value.to_string())
                .unwrap()
                .0,
            baseline
        );
        value["sessions"] = Value::String("09:30".to_owned());
        assert!(calendar_component_fingerprint_from_json(&value.to_string()).is_err());
    }
}
