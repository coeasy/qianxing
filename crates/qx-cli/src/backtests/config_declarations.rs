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
