use crate::error::{AppError, AppErrorCategory};
use qx_xingban::AshareRuleConfig;

/// Loaded A-share rules and the source path retained for run provenance.
#[derive(Clone, Debug)]
pub struct AshareBacktestBinding {
    pub rules: AshareRuleConfig,
    pub rules_path: String,
}

/// Load and validate the A-share rule snapshot and its optional data components.
///
/// The caller resolves relative paths before invoking this application-layer binder.
pub fn bind_ashare_backtest_rules(
    rules_source: Option<(&str, &str)>,
    actions_json: Option<&str>,
    calendar_json: Option<&str>,
    instrument: &str,
) -> Result<Option<AshareBacktestBinding>, AppError> {
    if (actions_json.is_some() || calendar_json.is_some()) && rules_source.is_none() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "配置 ashare_actions_path 或 ashare_calendar_path 时必须同时配置 ashare_rules_path",
        ));
    }
    let Some((path, payload)) = rules_source else {
        return Ok(None);
    };
    let mut rules: AshareRuleConfig = serde_json::from_str(payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("A 股规则快照 JSON 无效 {path}: {error}"),
        )
    })?;
    if let Some(actions) = actions_json {
        rules
            .apply_corporate_actions_json(instrument, actions)
            .map_err(|error| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("A 股公司行为 JSON 非法: {error}"),
                )
            })?;
    }
    if let Some(calendar) = calendar_json {
        rules.apply_calendar_json(calendar).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("A 股交易日历 JSON 非法: {error}"),
            )
        })?;
    }
    rules.validate().map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("A 股规则快照非法: {error}"),
        )
    })?;
    if !rules.enabled {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "配置 ashare_rules_path 后 enabled 必须为 true",
        ));
    }
    Ok(Some(AshareBacktestBinding {
        rules,
        rules_path: path.to_string(),
    }))
}
