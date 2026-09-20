//! 成本规则加载：手续费与延迟配置、命令行 `--costs` 开关与回测位置参数。

use super::*;

pub(crate) fn load_cost_rules(path: Option<&Path>) -> Result<ExecutionCostRules, String> {
    match path {
        Some(path) => ExecutionCostRules::load(path)
            .map_err(|error| format!("加载执行成本规则失败 {}: {error}", path.display())),
        None => Ok(ExecutionCostRules::default()),
    }
}

/// 回测命令的 `--costs <path>` / `--costs=<path>`。
///
/// 成本规则做成开关而不是尾随位置参数：`[market-spec.json]` 和 `[quantity]`
/// 都可选，若再排一个可选位置参数，只想改费率的调用方就得补一个假的 spec。
pub(crate) fn costs_flag_value(argv: &[String]) -> Option<PathBuf> {
    let mut index = 2;
    while index < argv.len() {
        let argument = argv[index].as_str();
        if let Some(value) = argument.strip_prefix("--costs=") {
            return Some(PathBuf::from(value));
        }
        if argument == "--costs" {
            return argv.get(index + 1).map(PathBuf::from);
        }
        index += 1;
    }
    None
}

/// 剥掉 `--costs` 开关及其取值后的位置参数序列，使开关能出现在任意位置而不会
/// 被当成 `[market-spec.json]`。前两项是占位，下标与 `argv[n]` 保持一致。
pub(crate) fn backtest_positional_args(argv: &[String]) -> Vec<String> {
    let mut positional = vec![String::new(), String::new()];
    let mut skip = false;
    for (index, argument) in argv.iter().cloned().enumerate() {
        if index < 2 {
            continue;
        }
        if skip {
            skip = false;
            continue;
        }
        if argument == "--costs" {
            skip = true;
            continue;
        }
        if argument.starts_with("--costs=") {
            continue;
        }
        positional.push(argument);
    }
    positional
}

/// Paper 成交计费的费用来源，与 [`run_single_strategy_backtest`] 同一优先级：
/// A 股规则快照自带佣金与印花税，否则使用执行成本规则。两个执行平面必须得出
/// 同一条成交的成本，否则 Paper 验证过的收益在回测里不成立。
pub(crate) fn paper_fee_model(
    config: &qx_runtime::RuntimeConfig,
) -> Result<Box<dyn FeeModel + Send>, String> {
    if let Some(path) = config.strategy.ashare_rules_path.as_deref() {
        let payload = std::fs::read_to_string(path)
            .map_err(|error| format!("读取 A 股规则快照失败 {path}: {error}"))?;
        let rules: AshareRuleConfig = serde_json::from_str(&payload)
            .map_err(|error| format!("A 股规则快照 JSON 无效 {path}: {error}"))?;
        rules
            .validate()
            .map_err(|error| format!("A 股规则快照非法: {error}"))?;
        if !rules.enabled {
            return Err("配置 ashare_rules_path 后 enabled 必须为 true".into());
        }
        return Ok(Box::new(AShareFeeModel {
            commission_bp: rules.commission_bp,
            min_commission: rules.min_commission,
            stamp_duty_bp: rules.stamp_duty_bp,
            transfer_fee_bp: rules.transfer_fee_bp,
        }));
    }
    Ok(load_cost_rules(config.strategy.cost_rules_path.as_deref().map(Path::new))?.fee_model())
}
