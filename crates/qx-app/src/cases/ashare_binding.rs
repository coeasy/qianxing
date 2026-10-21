use crate::error::{AppError, AppErrorCategory};
use qx_xingban::AshareRuleConfig;

/// Loaded A-share rules and the source path retained for run provenance.
#[derive(Clone, Debug)]
pub struct AshareBacktestBinding {
    pub rules: AshareRuleConfig,
    pub rules_path: String,
}

/// Load and validate the A-share rule snapshot, optional action events and bounded trading calendar.
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
    if rules.trading_days.is_empty() && rules.trading_timestamps.is_empty() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "A 股回测必须提供交易日历 trading_days 或显式 trading_timestamps；拒绝按任意日期可交易处理",
        ));
    }
    if rules.session_windows.is_empty() && rules.trading_timestamps.is_empty() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "A 股回测必须提供交易时段 sessions 或显式 trading_timestamps；拒绝按全天时段可交易处理",
        ));
    }
    Ok(Some(AshareBacktestBinding {
        rules,
        rules_path: path.to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::bind_ashare_backtest_rules;

    const RULES: &str = r#"{"enabled":true}"#;
    const CALENDAR_DAYS_ONLY: &str = r#"{"calendar_id":"cn-test","trading_days":["2024-06-03"]}"#;
    const CALENDAR_WITH_SESSIONS: &str = r#"{"calendar_id":"cn-test","trading_days":["2024-06-03"],"sessions":[["09:30","11:30"],["13:00","15:00"]]}"#;

    #[test]
    fn enabled_rules_fail_closed_without_bounded_calendar_and_sessions() {
        let missing_calendar =
            bind_ashare_backtest_rules(Some(("rules.json", RULES)), None, None, "000001.SZSE")
                .expect_err("缺交易日清单时不能按任意日期可交易运行");
        assert!(missing_calendar.to_string().contains("必须提供交易日历"));

        let missing_sessions = bind_ashare_backtest_rules(
            Some(("rules.json", RULES)),
            None,
            Some(CALENDAR_DAYS_ONLY),
            "000001.SZSE",
        )
        .expect_err("缺交易时段时不能按全天可交易运行");
        assert!(missing_sessions.to_string().contains("必须提供交易时段"));

        let binding = bind_ashare_backtest_rules(
            Some(("rules.json", RULES)),
            None,
            Some(CALENDAR_WITH_SESSIONS),
            "000001.SZSE",
        )
        .expect("有界交易日历和时段必须可用");
        assert_eq!(binding.unwrap().rules.trading_days.len(), 1);
    }
}
