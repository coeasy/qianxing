//! `--config` 的 A 股段在 Bar 回测链上的唯一读法：一条链接管、两条链明确拒绝（V11 Q61）。

use crate::{first_strategy_block_declaring, read_example_json};
use std::path::Path;

pub(crate) use qx_app::AshareBacktestBinding;

/// A 股规则快照的唯一读取读法：加载、合并公司行为与交易日历并校验。
///
/// 入参是**已按运行时配置目录解析过**的路径三元组。`rules_path` 缺位即 `Ok(None)`；
/// 只配了 actions/calendar 则报错 —— 静默丢掉公司行为或交易日历，等于让回测看不见分红与停牌。
pub(crate) fn ashare_backtest_binding(
    rules_path: Option<&str>,
    actions_path: Option<&str>,
    calendar_path: Option<&str>,
    instrument: &str,
) -> Result<Option<AshareBacktestBinding>, String> {
    let rules_payload = rules_path
        .map(|path| read_example_json(Path::new(path), " A 股规则快照"))
        .transpose()?;
    let actions_payload = actions_path
        .map(|path| read_example_json(Path::new(path), " A 股公司行为 JSON"))
        .transpose()?;
    let calendar_payload = calendar_path
        .map(|path| read_example_json(Path::new(path), " A 股交易日历 JSON"))
        .transpose()?;
    qx_app::bind_ashare_backtest_rules(
        rules_path.zip(rules_payload.as_deref()),
        actions_payload.as_deref(),
        calendar_payload.as_deref(),
        instrument,
    )
    .map_err(|error| error.message().to_string())
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
    if let Some(label) = first_strategy_block_declaring(path, |strategy| {
        strategy.ashare_rules_path.is_some()
            || strategy.ashare_actions_path.is_some()
            || strategy.ashare_calendar_path.is_some()
    })? {
        return Err(format!(
            "{entry} 不接受 {label}.ashare_rules_path（含 ashare_actions_path / ashare_calendar_path）：\
             {reason}。需要 A 股规则口径请改用 strategy backtest 或 backtest builtin"
        ));
    }
    Ok(())
}
