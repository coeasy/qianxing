//! `--config` 的 A 股段在 Bar 回测链上的唯一读法：一条链接管、两条链明确拒绝（V11 Q61）。

use super::*;

/// `--config` 的 A 股段在 Bar 回测链上的绑定：规则快照本身，加上它自带的那套费率模型。
///
/// 两者必须成对出现。规则快照里的 `commission_bp`/`stamp_duty_bp` 就是该标的的真实费率；
/// 只接规则不接费率会让 T+1、整手与涨跌停生效而费用退回成本文件的 2/5bp，
/// 于是产物里的成本来源成了假话。
pub(crate) struct AshareBacktestBinding {
    pub(crate) rules: AshareRuleConfig,
    pub(crate) fee: Box<dyn FeeModel>,
    /// 规则快照路径，写进 `cost_source`，让"费率被谁顶掉了"在产物里可读。
    pub(crate) rules_path: String,
}

/// A 股规则快照的唯一装配读法：加载、合并公司行为与交易日历、校验、取自带费率。
///
/// 入参是**已按运行时配置目录解析过**的路径三元组。`rules_path` 缺位即 `Ok(None)`；
/// 只配了 actions/calendar 则报错 —— 静默丢掉公司行为或交易日历，等于让回测看不见分红与停牌。
pub(crate) fn ashare_backtest_binding(
    rules_path: Option<&str>,
    actions_path: Option<&str>,
    calendar_path: Option<&str>,
    instrument: &str,
) -> Result<Option<AshareBacktestBinding>, String> {
    if (actions_path.is_some() || calendar_path.is_some()) && rules_path.is_none() {
        return Err(
            "配置 ashare_actions_path 或 ashare_calendar_path 时必须同时配置 ashare_rules_path"
                .into(),
        );
    }
    let Some(path) = rules_path else {
        return Ok(None);
    };
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取 A 股规则快照失败 {path}: {error}"))?;
    let mut rules: AshareRuleConfig = serde_json::from_str(&payload)
        .map_err(|error| format!("A 股规则快照 JSON 无效 {path}: {error}"))?;
    if let Some(actions_path) = actions_path {
        let actions_payload = std::fs::read_to_string(actions_path)
            .map_err(|error| format!("读取 A 股公司行为 JSON 失败 {actions_path}: {error}"))?;
        rules
            .apply_corporate_actions_json(instrument, &actions_payload)
            .map_err(|error| format!("A 股公司行为 JSON 非法: {error}"))?;
    }
    if let Some(calendar_path) = calendar_path {
        let calendar_payload = std::fs::read_to_string(calendar_path)
            .map_err(|error| format!("读取 A 股交易日历 JSON 失败 {calendar_path}: {error}"))?;
        rules
            .apply_calendar_json(&calendar_payload)
            .map_err(|error| format!("A 股交易日历 JSON 非法: {error}"))?;
    }
    rules
        .validate()
        .map_err(|error| format!("A 股规则快照非法: {error}"))?;
    if !rules.enabled {
        return Err("配置 ashare_rules_path 后 enabled 必须为 true".into());
    }
    let fee: Box<dyn FeeModel> = Box::new(AShareFeeModel {
        commission_bp: rules.commission_bp,
        min_commission: rules.min_commission,
        stamp_duty_bp: rules.stamp_duty_bp,
        transfer_fee_bp: rules.transfer_fee_bp,
    });
    Ok(Some(AshareBacktestBinding {
        rules,
        fee,
        rules_path: path.to_string(),
    }))
}

/// 只有 `--config` 路径的命令行型入口（`backtest builtin` / `ccxt-builtin`）怎么取 A 股段：
/// 从配置里读那三个路径并按配置目录解析，再交给唯一的那份装配读法。
pub(crate) fn configured_ashare_binding(
    config_path: Option<&Path>,
    instrument: &str,
) -> Result<Option<AshareBacktestBinding>, String> {
    let Some(path) = config_path else {
        return Ok(None);
    };
    let strategy = &read_runtime_config(path)?.strategy;
    let resolved = |configured: Option<&String>| {
        configured.map(|value| {
            resolve_runtime_relative_path(path, value)
                .to_string_lossy()
                .into_owned()
        })
    };
    ashare_backtest_binding(
        resolved(strategy.ashare_rules_path.as_ref()).as_deref(),
        resolved(strategy.ashare_actions_path.as_ref()).as_deref(),
        resolved(strategy.ashare_calendar_path.as_ref()).as_deref(),
        instrument,
    )
}

/// 承不了 A 股规则的入口必须当场拒绝，而不是收下配置再静默丢掉（V11 Q61）。
pub(crate) fn reject_ashare_rules_config(
    config_path: Option<&Path>,
    entry: &str,
    reason: &str,
) -> Result<(), String> {
    let Some(path) = config_path else {
        return Ok(());
    };
    let strategy = &read_runtime_config(path)?.strategy;
    if strategy.ashare_rules_path.is_some()
        || strategy.ashare_actions_path.is_some()
        || strategy.ashare_calendar_path.is_some()
    {
        return Err(format!(
            "{entry} 不接受 strategy.ashare_rules_path（含 ashare_actions_path / ashare_calendar_path）：\
             {reason}。需要 A 股规则口径请改用 strategy backtest 或 backtest builtin"
        ));
    }
    Ok(())
}
