//! 多腿回测的单腿定资：一条腿的账户现金必须按**它自己的**下单量与价格尺度算出来。
//!
//! 定资口径只写在这里一份：`multi-builtin` 的每个腿入口都调本模块，不再各自抄一个
//! "拍脑袋的账户余额"。V11 Q0e 把算不出来的情形从"静默截断到 `i64::MAX`"改成报错——
//! 截断等于把"这条腿根本买不起"伪装成"回测跑通了、零成交"，是 §4.18 的同一类失真。

use super::*;

/// 多腿回测单腿账户的现金下限（整单位），与装配默认账户同尺度。
pub(crate) const MULTI_LEG_ACCOUNT_CASH_FLOOR: i128 = 100_000;

/// 一条腿跑完其信号计划所需的峰值现金（整单位）。
///
/// 套利策略的目标仓位被 `quantity` 钉在 ±quantity，所以任一时刻该腿占用的现金上界是
/// "2 × quantity × 本腿全帧最高价"，再加同一名义额按生效 taker 费率折算的手续费余量。
/// 全程 checked 运算：算得溢出说明这个夹具在该尺度下根本买不起自己的计划，必须报错，
/// 而不是截断成"能表示的最大值"后让回测以零成交静默通过。
pub(crate) fn multi_leg_leg_cash_required(
    quantity: i64,
    bars: &[Bar],
    taker_bp: i64,
    label: &str,
) -> Result<i128, String> {
    let max_price_whole = bars
        .iter()
        .map(|bar| bar.high / qx_core::SCALE)
        .max()
        .unwrap_or(0);
    let peak_notional = i128::from(quantity)
        .checked_mul(max_price_whole)
        .and_then(|value| value.checked_mul(2))
        .ok_or_else(|| format!("{label} 腿计划名义额溢出，无法为回测账户定资"))?;
    let fee_headroom = peak_notional
        .checked_mul(i128::from(taker_bp))
        .and_then(|value| value.checked_div(10_000))
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| format!("{label} 腿手续费余量溢出"))?;
    Ok(peak_notional
        .checked_add(fee_headroom)
        .ok_or_else(|| format!("{label} 腿所需现金溢出"))?
        .max(MULTI_LEG_ACCOUNT_CASH_FLOOR))
}

/// 同上，但落到账户能表示的 `Money`；超出上限时给出带数值的可诊断错误。
pub(crate) fn multi_leg_leg_cash(
    quantity: i64,
    bars: &[Bar],
    taker_bp: i64,
    label: &str,
) -> Result<Money, String> {
    let required = multi_leg_leg_cash_required(quantity, bars, taker_bp, label)?;
    i64::try_from(required)
        .map(Money::from_i64)
        .map_err(|_| {
            format!(
                "{label} 腿按 {quantity} 单位 × 本腿全帧最高价所需现金 {required} 超出账户可表示上限（{}）；该计划在此尺度下无法被任何账户买得起，请降低 quantity 或修正夹具价格尺度",
                i64::MAX
            )
        })
}
