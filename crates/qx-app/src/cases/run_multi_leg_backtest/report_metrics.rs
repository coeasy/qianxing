use super::{invalid, invariant, AppError};
use qx_core::{ExactDecimal, Fill, Fnv1a, InstrumentId};

pub(super) fn leg_return_bps(
    exact_pnl: ExactDecimal,
    initial_cash_raw: i128,
) -> Result<i32, AppError> {
    if initial_cash_raw <= 0 {
        return Err(invalid("多腿腿级收益分母必须为正"));
    }
    let initial_cash = ExactDecimal::from_parts(initial_cash_raw, 9)
        .map_err(|error| invalid(format!("多腿精确初始资金无效: {error}")))?;
    crate::report_analysis::exact_ratio_bps_v1(exact_pnl, initial_cash).map_err(invalid)
}

pub(super) fn leg_return_bps_exact(
    exact_pnl: ExactDecimal,
    initial_cash_exact: ExactDecimal,
) -> Result<i32, AppError> {
    if !initial_cash_exact.is_positive() {
        return Err(invalid("多腿精确腿级收益分母必须为正"));
    }
    crate::report_analysis::exact_ratio_bps_v1(exact_pnl, initial_cash_exact).map_err(invalid)
}

pub(super) fn fill_result_hash(
    instrument: &InstrumentId,
    fills: &[Fill],
) -> Result<String, AppError> {
    let encoded = serde_json::to_string(fills)
        .map_err(|error| invariant(format!("多腿成交事实序列化失败: {error}")))?;
    let mut hasher = Fnv1a::new();
    hasher.write_text(&instrument.to_string());
    hasher.write_text(&encoded);
    Ok(format!("{:016x}", hasher.finish()))
}
