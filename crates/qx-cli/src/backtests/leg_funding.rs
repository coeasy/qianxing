//! 多腿回测的腿级资金口径：单腿定资，以及"这条腿按什么产品记账"的规格闸门。
//!
//! 定资口径只写在这里一份：`multi-builtin` 的每个腿入口都调本模块，不再各自抄一个
//! "拍脑袋的账户余额"。V11 Q0e 把算不出来的情形从"静默截断到 `i64::MAX`"改成报错——
//! 截断等于把"这条腿根本买不起"伪装成"回测跑通了、零成交"，是 §4.18 的同一类失真。
//!
//! 规格闸门（V11 Q58）管的是另一头：名义额、保证金与资金费全部取自 market spec，
//! 缺 spec 的腿一律按现货乘数 1 记账。这个默认对现货是对的，对衍生品却会把成本静默
//! 记成 0，而"是不是衍生品"这件事只有 spec 说得了——所以声称要算资金费时，必须先让
//! 每条腿把话说清楚。

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

/// 命令行回测入口读 `strategy.product` 的落点（V11 Q58）。
///
/// 产品形态只有两处事实来源：该标的的 market spec，或运行时配置里声明的 `strategy.product`。
/// 前者是位置参数、可以整体不给，所以"没给 spec 的那条链到底算不算衍生品"只能由这一份声明
/// 回答。与 [`super::backtest_risk_binding`] / [`super::configured_fill_model`] 同构：给了
/// `--config` 就必须认它声明的口径，否则同一份配置在四条 Bar 链上会得到不同的记账前提。
pub(crate) fn configured_instrument_product(
    config_path: Option<&Path>,
) -> Result<Option<TradingProduct>, String> {
    Ok(match config_path {
        Some(path) => read_runtime_config(path)?.strategy.product,
        None => None,
    })
}

/// 多腿链的规格闸门：先问清"每条腿按什么产品记账"，再开始跑腿级回测。
///
/// 归因侧的两笔钱都只在衍生品腿上计提（`multi_leg_leg_buckets` 的资金费、
/// `multi_leg_leg_margin` 的保证金），而产品形态只有两个事实来源：该腿的 market spec，
/// 或 `--config` 里声明的 `strategy.product`。两者都没有时按现货乘数 1 记账 —— 对现货
/// 正确，对衍生品等于把名义额、保证金和资金费全部静默记成 0。V11 Q58 之前这里写着一条
/// "主腿衍生品必须提供 market spec"的守卫，但它判的是 `spec.is_some() && path.is_none()`，
/// 而 spec 只可能从 path 解析出来，因此永不成立；这条把它换成真会红的那一份。
pub(crate) fn multi_leg_spec_guard(
    legs: [(&str, Option<&TradingInstrumentSpec>); 2],
    funding_bps: i64,
    declared_product: Option<TradingProduct>,
) -> Result<(), String> {
    let (primary_label, primary_spec) = legs[0];
    if let Some(product) = declared_product.filter(|product| product.is_derivative()) {
        if primary_spec.is_none() {
            return Err(format!(
                "主腿衍生品（strategy.product={product:?}）多腿回测必须提供 {primary_label} 腿的 \
                 market spec：缺它时名义额按现货乘数 1 记账，保证金与资金费也无从计提"
            ));
        }
    }
    if funding_bps == 0 {
        return Ok(());
    }
    for (label, spec) in legs {
        if spec.is_none() {
            return Err(format!(
                "资金费只能按该腿 market spec 的名义额计提：{label} 腿没有 market spec，既算不出金额，\
                 也判不出它是现货还是衍生品。请补该腿的 spec 文件，或把 --funding-bps 设为 0"
            ));
        }
    }
    if legs
        .iter()
        .all(|(_, spec)| !spec.is_some_and(|spec| spec.product.is_derivative()))
    {
        let products = legs
            .iter()
            .map(|(label, spec)| format!("{label}={:?}", spec.map(|spec| spec.product)))
            .collect::<Vec<_>>()
            .join(" ");
        return Err(format!(
            "没有一条腿是衍生品（{products}），--funding-bps={funding_bps} 无处计提：资金费只向持仓\
             衍生品合约的一方收取，把这条声明落到现货腿上等于造一笔不存在的成本"
        ));
    }
    Ok(())
}
