use super::{invalid, invariant, AppError};
use qx_core::Fill;
use qx_core::{Fnv1a, InstrumentId, Ledger, TradingInstrumentSpec};
use qx_guanxing::Bar;

pub(super) struct LegReturnInput<'a> {
    pub(super) ledger: &'a Ledger,
    pub(super) account_id: &'a str,
    pub(super) instrument: &'a InstrumentId,
    pub(super) spec: &'a TradingInstrumentSpec,
    pub(super) bars: &'a [Bar],
    pub(super) fills: &'a [Fill],
    pub(super) funding_raw: i128,
    pub(super) initial_cash_raw: i128,
}

pub(super) fn leg_return_bps(input: LegReturnInput<'_>) -> Result<i32, AppError> {
    let LegReturnInput {
        ledger,
        account_id,
        instrument,
        spec,
        bars,
        fills,
        funding_raw,
        initial_cash_raw,
    } = input;
    let position = ledger.position_for(account_id, instrument);
    let unrealized = if position.quantity.raw() == 0 {
        0
    } else {
        let mark = bars
            .last()
            .map(|bar| bar.close)
            .ok_or_else(|| invalid("多腿最终估值缺少 Bar"))?;
        spec.unrealized_pnl(position.quantity.raw(), position.average_entry.raw(), mark)
            .map_err(invalid)?
    };
    let fees = fills
        .iter()
        .try_fold(0_i128, |sum, fill| sum.checked_add(fill.fee.raw()))
        .ok_or_else(|| invalid("多腿腿级费用累计溢出"))?;
    let pnl = position
        .realized_pnl
        .raw()
        .checked_add(unrealized)
        .and_then(|value| value.checked_sub(fees))
        .and_then(|value| value.checked_sub(funding_raw))
        .ok_or_else(|| invalid("多腿腿级盈亏溢出"))?;
    if initial_cash_raw <= 0 {
        return Err(invalid("多腿腿级收益分母必须为正"));
    }
    pnl.checked_mul(10_000)
        .and_then(|value| value.checked_div(initial_cash_raw))
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid("多腿腿级收益无法表示"))
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
