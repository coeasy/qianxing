//! CLI presentation for shared service backtest principal bindings.

pub(crate) use qx_service::backtest_bindings::{
    account_principal_note, reject_split_account_principal, BacktestAccountBase,
    BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE, DEFAULT_BACKTEST_INITIAL_CASH,
};
#[cfg(test)]
pub(crate) use qx_service::backtest_bindings::{
    BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE, BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE,
};
#[cfg(test)]
pub(crate) use qx_service::backtest_bindings::{
    BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE, BACKTEST_ACCOUNT_BASE_REGISTRY_SOURCE,
};

#[cfg(test)]
pub(crate) fn backtest_initial_cash(declared: Option<i128>) -> Result<BacktestAccountBase, String> {
    qx_service::backtest_bindings::backtest_initial_cash(declared)
}

pub(crate) fn backtest_account_base_note(base: BacktestAccountBase) -> String {
    format!(
        "initial_cash_raw={} account_base_source={}",
        base.cash.raw(),
        base.source
    )
}
