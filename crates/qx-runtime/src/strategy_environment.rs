//! Strategy worker environment allowlist and credential boundary.

use std::collections::BTreeMap;

const INHERITED_STRATEGY_ENV: [&str; 5] = ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"];
const SENSITIVE_ENV_MARKERS: [&str; 6] = [
    "SECRET",
    "TOKEN",
    "PASSWORD",
    "API_KEY",
    "PRIVATE_KEY",
    "CREDENTIAL",
];

/// Validate configured environment entries before they are accepted or passed to a worker.
pub fn validate_strategy_external_env(configured: &BTreeMap<String, String>) -> Result<(), String> {
    for (key, value) in configured {
        if key.trim().is_empty()
            || key.contains('=')
            || value
                .chars()
                .any(|character| matches!(character, '\n' | '\r'))
        {
            return Err(format!("策略 worker 环境变量 {key} 含非法键值"));
        }
        let upper = key.to_ascii_uppercase();
        if SENSITIVE_ENV_MARKERS
            .iter()
            .any(|marker| upper.contains(marker))
        {
            return Err(format!(
                "策略 worker 环境变量 {key} 可能包含交易凭证，已拒绝"
            ));
        }
    }
    Ok(())
}

/// Build the exact environment visible to an external strategy process.
///
/// Parent environment values are restricted to executable/runtime essentials. Caller supplied
/// entries are then added after passing the same validator used by RuntimeConfig admission.
pub fn strategy_child_environment(
    configured: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    validate_strategy_external_env(configured)?;
    let mut environment = BTreeMap::new();
    for key in INHERITED_STRATEGY_ENV {
        if let Ok(value) = std::env::var(key) {
            environment.insert(key.to_string(), value);
        }
    }
    environment.extend(configured.clone());
    Ok(environment)
}
