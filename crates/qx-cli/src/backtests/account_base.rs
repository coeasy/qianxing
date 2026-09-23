//! 回测链的账户本金：常数、读法与来源标签（V11 Q72）。
//!
//! 本金不是一个可选的装饰数：`return_bps` 以它为分母，策略上下文与风控门读到的现金腿和
//! `available_margin_raw` 就是它，内核那句"买入成本（含预估手续费）超过账户可用现金"的拒单
//! 也由它判。在这份实现之前，它是散在三处的字面量（装配默认、`strategy backtest`、
//! `backtest book`），既不声明也不可见：把仓库自带夹具的 `quantity` 从 1 抬到 2 就零成交，
//! 而读者拿到的 `return_bps=0` 看起来像"这策略不赚不赔"。

use super::*;

/// 没人声明本金时，单腿回测按多少记账。**这一份常数是仓库里唯一一处**。
pub(crate) const DEFAULT_BACKTEST_INITIAL_CASH: i64 = 100_000;
/// 摘要与 stdout 共用的两格来源标签："没配" 与 "配了同一个数" 必须分得开，与
/// `execution_costs.source`、`fill_model.source` 是同一条理由。
pub(crate) const BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE: &str = "builtin-default";
pub(crate) const BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE: &str = "strategy-initial-cash";
/// 多腿链不用这一格：两条腿的本金各按本腿行情由 `multi_leg_leg_cash` 定资。
pub(crate) const BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE: &str = "multi-leg-funding-rule";

/// 一条回测链实际使用的账户本金，连同"这个数从哪来"。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BacktestAccountBase {
    pub(crate) cash: Money,
    pub(crate) source: &'static str,
}

/// 把 `strategy.initial_cash_raw` 折成回测用的本金，并交代来源。
///
/// 非正的声明报错而不是回落默认：把 0 或负数当成"没配"，念出来的就是"这策略不赚不赔"，
/// 而真相是"这笔计划没人给它算过钱"（与 Q71 撤掉折成 0 同一条纪律）。
pub(crate) fn backtest_initial_cash(declared: Option<i128>) -> Result<BacktestAccountBase, String> {
    let Some(raw) = declared else {
        return Ok(BacktestAccountBase {
            cash: Money::from_i64(DEFAULT_BACKTEST_INITIAL_CASH),
            source: BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE,
        });
    };
    if raw <= 0 {
        return Err(format!(
            "strategy.initial_cash_raw={raw} 不是正的本金：收益率拿它做分母、风控拿它当可用现金，\
             零或负数账户上跑出来的数字没有任何含义。请给出正数，或删掉这一格改用默认本金 {DEFAULT_BACKTEST_INITIAL_CASH}"
        ));
    }
    Ok(BacktestAccountBase {
        cash: Money::from_raw(raw),
        source: BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE,
    })
}

/// 读 `--config` 里声明的本金；没给配置文件就是"没人声明"。
pub(crate) fn configured_initial_cash_raw(
    config_path: Option<&Path>,
) -> Result<Option<i128>, String> {
    Ok(match config_path {
        Some(path) => read_runtime_config(path)?.strategy.initial_cash_raw,
        None => None,
    })
}

/// stdout 与摘要共用的同一格写法：定点整数 + 来源，中间不夹任何一方的猜测。
pub(crate) fn backtest_account_base_note(base: BacktestAccountBase) -> String {
    format!(
        "initial_cash_raw={} account_base_source={}",
        base.cash.raw(),
        base.source
    )
}
