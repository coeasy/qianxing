//! 运行时配置里"哪一个策略块声明了某项"的唯一问法（V11 R1 / R11）。
//!
//! 一条声明可以落在 legacy `strategy` 块，也可以落在 `strategies[]` 的任意一项。入口
//! 要判断"这份配置里有没有承不了的字段"，只看前者就等于留一条"写进列表就绕过闸门"的路径；
//! 两个入口（A 股规则、撮合模型）各自扫一遍就会扫出两套覆盖范围，所以这里只留一份。

use super::*;

/// 哪一个策略块声明了这个字段：标签沿用 `runtime_check` 的口径（`strategy` /
/// `strategy[{id}]`），让体检与拒绝指认的是同一条声明。
pub(crate) fn first_strategy_block_declaring(
    config_path: &Path,
    declares: fn(&StrategyRuntimeConfig) -> bool,
) -> Result<Option<String>, String> {
    let config = read_runtime_config(config_path)?;
    let mut blocks = vec![("strategy".to_string(), &config.strategy)];
    blocks.extend(config.strategies.iter().map(|strategy| {
        (
            format!(
                "strategy[{}]",
                strategy.id.as_deref().unwrap_or("<missing-id>")
            ),
            strategy,
        )
    }));
    Ok(blocks
        .into_iter()
        .find(|(_, strategy)| declares(strategy))
        .map(|(label, _)| label))
}

/// 实例块里那份声明会不会被这条入口读到：命令行回测的读点（`fill_model`、
/// `initial_cash_raw`、`risk_rules`、`product`、`margin_mode`、`allow_short`）取的都是顶层
/// `strategy.<字段>`，而 `config validate` 逐块体检时把 `strategies[]` 也算在内。校验点头、
/// 跑起来用的是另一份——这正是 V11 N5 给 `cost_rules_path` 记下的那道断层，这里把同一条
/// 判据铺到其余六格。
///
/// 只在命令行回测侧判：`strategy backtest` 那条链逐块装配（`strategy_config.strategy =
/// strategy`），实例声明在那儿是真的生效，搬到 `config validate` 反而会把那份合法配置拒掉。
///
/// `declared` 交回的是"这份声明的写法"而不是值本身：六格的类型各异（字符串、定点数、
/// 枚举、规则集），而这里只问两件事——有没有声明、两边写的是不是同一份。
pub(crate) fn unapplied_strategy_declaration(
    config: &RuntimeConfig,
    field: &str,
    declared: fn(&StrategyRuntimeConfig) -> Option<String>,
) -> Result<(), String> {
    let applied = declared(&config.strategy);
    for strategy in &config.strategies {
        let Some(rendered) = declared(strategy) else {
            continue;
        };
        if applied.as_ref() == Some(&rendered) {
            continue;
        }
        let applied_note = match &applied {
            Some(value) => format!("顶层当前生效的是 {value}"),
            None => format!("顶层没有声明 {field}，当前生效的是内核默认口径"),
        };
        return Err(format!(
            "strategies[{}] 声明的 {field} {rendered} 不会被应用：命令行回测入口只读顶层 \
             `strategy.{field}`（{applied_note}）；请把这条声明挪到顶层，或从实例里删掉它",
            strategy.id.as_deref().unwrap_or("<missing-id>")
        ));
    }
    Ok(())
}

/// 本模块第二族判据：一条声明在这条链上**根本没有落点**，与"落点只读顶层"不同——
/// 后者挪到顶层就能生效，前者挪到哪里都不会。`leverage` 与 `position_mode` 属于后者之外的
/// 那一类：内置策略链派生不出 `OrderPolicy`，进内核的订单一律带
/// `OrderPolicy::default()`（Cash / OneWay / 1x），而内核**确实**按这份政策记初始保证金与
/// 双向持仓（`qx-xingban/src/backtest.rs` 的 `product_policy`）。于是"配了 5x、跑出 1x 的
/// 权益与 result_hash"不会在产物里留下任何痕迹（V11 R4-5，与 R11 拒绝 `fill_model` 同判据）。
type ProductPolicyField = (&'static str, fn(&StrategyRuntimeConfig) -> bool);
const PRODUCT_POLICY_FIELDS: [ProductPolicyField; 2] = [
    ("leverage", |strategy| strategy.leverage.is_some()),
    ("position_mode", |strategy| strategy.position_mode.is_some()),
];

/// 哪一个策略块声明了这两格产品政策中的哪一格：覆盖范围与
/// [`first_strategy_block_declaring`] 同一份（顶层与 `strategies[]` 都算，只查顶层等于留一条
/// "写进列表就绕过闸门"的路径，V11 R1）。
fn declared_product_policy(config: &RuntimeConfig) -> Option<(String, &'static str)> {
    let mut blocks = vec![("strategy".to_string(), &config.strategy)];
    blocks.extend(config.strategies.iter().map(|strategy| {
        (
            format!(
                "strategy[{}]",
                strategy.id.as_deref().unwrap_or("<missing-id>")
            ),
            strategy,
        )
    }));
    blocks.into_iter().find_map(|(label, strategy)| {
        PRODUCT_POLICY_FIELDS
            .iter()
            .find(|(_, declared)| declared(strategy))
            .map(|(field, _)| (label, *field))
    })
}

/// 这条链会把声明的产品政策带进内核吗？不会时当场拒，而不是收下配置再静默丢掉。
///
/// `config` 是**这一轮真正装配的那一份**：`strategy backtest` 逐块克隆配置（并把
/// `strategies` 清空），所以从磁盘重读只会判到别的策略块上——那是 N5 记过的同一道断层。
pub(crate) fn reject_unapplied_product_policy(
    config: &RuntimeConfig,
    entry: &str,
) -> Result<(), String> {
    let Some((label, field)) = declared_product_policy(config) else {
        return Ok(());
    };
    Err(format!(
        "{entry} 不接受 {label}.{field}：这条链的订单一律带 OrderPolicy::default()（Cash / \
         OneWay / 1x）进内核，而初始保证金与双向持仓正是按那份政策记账——收下声明再丢掉，等于让 \
         配置替这轮回测宣称一个从未生效过的杠杆。需要杠杆口径请改用 strategy backtest 的跨语言策略 \
         分支，它把声明的政策逐张装进订单；不需要就把这一格从配置里删掉"
    ))
}

/// 磁盘版读点：命令行回测入口只拿到 `--config` 路径，读法与 `configured_fill_model` 同一处。
pub(crate) fn reject_configured_product_policy(
    config_path: Option<&Path>,
    entry: &str,
) -> Result<(), String> {
    let Some(path) = config_path else {
        return Ok(());
    };
    reject_unapplied_product_policy(&read_runtime_config(path)?, entry)
}
