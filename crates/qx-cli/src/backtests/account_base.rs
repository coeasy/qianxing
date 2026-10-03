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
/// 第四格：回测侧那格声明同时被 paper 侧的 `worker.paper_initial_cash_raw` 确认过（V12 R3）。
/// 它必须与"只有回测侧声明"分得开，否则读者看不出这份 runtime 的账户本金是被两处共用的。
pub(crate) const BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE: &str =
    "strategy-initial-cash+paper-worker-cash";
/// 回测侧那格的声明处名字，报错与来源标签共用它。
const STRATEGY_PRINCIPAL_SITE: &str = "strategy.initial_cash_raw";

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

/// stdout 与摘要共用的同一格写法：定点整数 + 来源，中间不夹任何一方的猜测。
pub(crate) fn backtest_account_base_note(base: BacktestAccountBase) -> String {
    format!(
        "initial_cash_raw={} account_base_source={}",
        base.cash.raw(),
        base.source
    )
}

/// 一份 runtime 里两处账户本金声明的登记处（V12 R3）。回测侧一格在前，paper 侧按 worker 逐个跟上。
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

/// paper 侧有没有把回测侧那格原数再声明一遍。
fn paper_declares_same_principal(config: &RuntimeConfig) -> bool {
    let Some(shared) = config.strategy.initial_cash_raw else {
        return false;
    };
    account_principal_declarations(config)
        .iter()
        .any(|(site, raw)| site != STRATEGY_PRINCIPAL_SITE && *raw == shared)
}

/// 同一份配置里的账户本金只能有一个口径（V12 R3 / §4.4）。
///
/// 回测拿 `strategy.initial_cash_raw` 做收益率分母与风控可用现金，Paper 拿
/// `worker.paper_initial_cash_raw` 入账初始资金，两侧过去互不知情：一份 runtime 同时写
/// 100,000 与 200,000 也能照常启动，使用者却以为"我声明了一份本金"。判据只有一条 ——
/// 回测侧说了 N，就没有别的格子能说 M≠N；两个 paper 账户各写各的数不在此列（那本来就是
/// 按账户定资，没有任何格子声称它是全局口径）。
pub(crate) fn reject_split_account_principal(config: &RuntimeConfig) -> Result<(), String> {
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

/// 回测入口真正用的本金，连同"这一格有没有被 paper 侧同一个数确认"。
pub(crate) fn account_base_from_config(
    config: &RuntimeConfig,
) -> Result<BacktestAccountBase, String> {
    reject_split_account_principal(config)?;
    let mut base = backtest_initial_cash(config.strategy.initial_cash_raw)?;
    if base.source == BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE && paper_declares_same_principal(config) {
        base.source = BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE;
    }
    Ok(base)
}

/// 与 [`account_base_from_config`] 同源，但只给"没读 config 的那条链"用：不给配置就是没声明。
pub(crate) fn configured_account_base(
    config_path: Option<&Path>,
) -> Result<BacktestAccountBase, String> {
    match config_path {
        Some(path) => account_base_from_config(&read_runtime_config(path)?),
        None => backtest_initial_cash(None),
    }
}

/// Paper 侧入账前那句"两处声明一致"：等值不等于不用说，读者要能看出这一轮压的钱是两处共用的。
pub(crate) fn account_principal_note(config: &RuntimeConfig) -> Option<String> {
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
