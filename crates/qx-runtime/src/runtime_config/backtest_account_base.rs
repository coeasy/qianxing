//! Backtest account principal resolution from the shared runtime configuration.

use super::RuntimeConfig;

/// The configured amount's provenance; display strings remain owned by the artifact contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BacktestPrincipalSource {
    BuiltinDefault,
    StrategyConfig,
    AccountRegistry,
    StrategyAndPaper,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BacktestPrincipal {
    pub initial_cash_raw: i128,
    pub source: BacktestPrincipalSource,
}

/// Historical default retained for runs whose runtime config omits initial_cash_raw.
pub const DEFAULT_BACKTEST_INITIAL_CASH: i64 = 100_000;

const STRATEGY_PRINCIPAL_SITE: &str = "strategy.initial_cash_raw";

fn account_principal_declarations(config: &RuntimeConfig) -> Vec<(String, i128)> {
    let mut declared = Vec::new();
    if let Some(raw) = config.strategy.initial_cash_raw {
        declared.push((STRATEGY_PRINCIPAL_SITE.to_string(), raw));
    }
    declared.extend(config.workers.iter().filter_map(|worker| {
        worker
            .paper_initial_cash_raw
            .map(|raw| (format!("worker[{}]", worker.id), raw))
    }));
    declared
}

fn paper_declares_same_principal(config: &RuntimeConfig) -> bool {
    let Some(shared) = config.strategy.initial_cash_raw else {
        return false;
    };
    account_principal_declarations(config)
        .iter()
        .any(|(site, raw)| site != STRATEGY_PRINCIPAL_SITE && *raw == shared)
}

/// Reject a runtime whose backtest denominator and Paper funding disagree.
pub fn reject_split_account_principal(config: &RuntimeConfig) -> Result<(), String> {
    if !config.accounts.is_empty()
        && (!config.strategies.is_empty()
            || config
                .strategy
                .account_id
                .as_deref()
                .and_then(|account_id| config.account(account_id))
                .is_some())
    {
        return Ok(());
    }
    let Some(shared) = config.strategy.initial_cash_raw else {
        return Ok(());
    };
    let conflicting = account_principal_declarations(config)
        .iter()
        .filter(|(site, raw)| site != STRATEGY_PRINCIPAL_SITE && *raw != shared)
        .map(|(site, raw)| format!("{site}={raw}"))
        .collect::<Vec<_>>();
    if conflicting.is_empty() {
        return Ok(());
    }
    Err(format!(
        "这份运行时配置声明了两份互不相等的账户本金：{STRATEGY_PRINCIPAL_SITE}={shared} vs {}。\
         回测按前一个数记收益率分母与可用现金，Paper 按后一个数入账初始资金，两个数不等时同一份\
         配置会跑出两套账。请删掉一处声明，或把它们改成同一个数。",
        conflicting.join(" vs ")
    ))
}

/// Resolve one positive, fixed-point backtest principal and its source.
pub fn backtest_initial_cash(declared: Option<i128>) -> Result<BacktestPrincipal, String> {
    let Some(raw) = declared else {
        return Ok(BacktestPrincipal {
            initial_cash_raw: qx_core::Money::from_i64(DEFAULT_BACKTEST_INITIAL_CASH).raw(),
            source: BacktestPrincipalSource::BuiltinDefault,
        });
    };
    if raw <= 0 {
        return Err(format!(
            "strategy.initial_cash_raw={raw} 不是正的本金：收益率拿它做分母、风控拿它当可用现金，\
             零或负数账户上跑出来的数字没有任何含义。请给出正数，或删掉这一格改用默认本金 {DEFAULT_BACKTEST_INITIAL_CASH}"
        ));
    }
    Ok(BacktestPrincipal {
        initial_cash_raw: raw,
        source: BacktestPrincipalSource::StrategyConfig,
    })
}

/// Resolve the account registry/config principal after checking cross-surface declarations.
pub fn account_base_from_config(config: &RuntimeConfig) -> Result<BacktestPrincipal, String> {
    reject_split_account_principal(config)?;
    let account_base = config
        .strategy
        .account_id
        .as_deref()
        .and_then(|account_id| config.account(account_id))
        .and_then(|account| account.initial_cash_raw);
    let mut base = if let Some(raw) = account_base {
        let mut base = backtest_initial_cash(Some(raw))?;
        base.source = BacktestPrincipalSource::AccountRegistry;
        base
    } else {
        backtest_initial_cash(config.strategy.initial_cash_raw)?
    };
    if base.source == BacktestPrincipalSource::StrategyConfig
        && paper_declares_same_principal(config)
    {
        base.source = BacktestPrincipalSource::StrategyAndPaper;
    }
    Ok(base)
}

/// Paper-side note uses the same runtime principal declarations as the backtest guard.
pub fn account_principal_note(config: &RuntimeConfig) -> Option<String> {
    let shared = config.strategy.initial_cash_raw?;
    let sites = account_principal_declarations(config)
        .into_iter()
        .filter(|(_, raw)| *raw == shared)
        .map(|(site, _)| site)
        .collect::<Vec<_>>();
    (sites.len() > 1).then(|| {
        format!(
            "[Paper · Account] 账户本金两处声明一致: initial_cash_raw={shared} ({})",
            sites.join(", ")
        )
    })
}
