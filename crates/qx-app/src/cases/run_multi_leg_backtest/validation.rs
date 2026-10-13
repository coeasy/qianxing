use super::{invalid, BarFrame, MultiLegBacktestSpec};
use crate::AppError;

pub(super) fn validate_custom_sizing(
    spec: &MultiLegBacktestSpec,
    has_custom_strategy: bool,
) -> Result<(), AppError> {
    if has_custom_strategy
        && (spec.strategy.target_notional_raw.is_some()
            || spec.strategy.target_available_margin_bps.is_some()
            || spec.strategy.target_volatility_bps.is_some()
            || spec.strategy.target_kelly_fraction_bps.is_some())
    {
        return Err(invalid(
            "自定义多腿策略不能声明仅由 Rust 内置策略执行的 sizing 参数；请由 callback 显式生成两腿数量",
        ));
    }
    Ok(())
}

pub(super) fn read_frame(path: &str, label: &str) -> Result<BarFrame, AppError> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| AppError::from_io(&format!("读取{label} BarFrame {path}"), &error))?;
    BarFrame::from_json(&payload)
        .map_err(|error| invalid(format!("{label} BarFrame 无效: {error:?}")))
}
