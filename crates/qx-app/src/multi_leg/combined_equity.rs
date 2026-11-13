//! 多腿组合权益曲线与回撤归约。

use crate::report_analysis::{exact_equity_max_drawdown_bps_v1, exact_equity_total_return_bps_v1};
use qx_core::ExactDecimal;

/// 两腿合并后的组合权益事实。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiLegCombinedEquity {
    pub equity_raw: Vec<i128>,
    pub initial_cash_raw: i128,
    pub final_equity_raw: i128,
    pub max_drawdown_bps: u32,
}

/// Scale-aware combined equity facts for two independently funded Bar legs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiLegCombinedExactEquity {
    pub equity_exact: Vec<ExactDecimal>,
    pub initial_cash_exact: ExactDecimal,
    pub final_equity_exact: ExactDecimal,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
}

/// Sum independently marked exact leg curves and recompute portfolio return and drawdown.
pub fn multi_leg_combined_exact_equity_metrics(
    primary_equity: &[ExactDecimal],
    reference_equity: &[ExactDecimal],
    primary_initial_cash: ExactDecimal,
    reference_initial_cash: ExactDecimal,
    expected_len: usize,
) -> Result<MultiLegCombinedExactEquity, String> {
    if primary_equity.len() != expected_len
        || reference_equity.len() != expected_len
        || expected_len == 0
    {
        return Err("多腿 exact 合并权益长度与对齐 BarFrame 不一致或为空".into());
    }
    let initial_cash_exact = primary_initial_cash
        .checked_add(reference_initial_cash)
        .ok_or("多腿 exact 初始权益求和溢出")?;
    if !initial_cash_exact.is_positive() {
        return Err("多腿 exact 初始权益必须为正".into());
    }
    let equity_exact = primary_equity
        .iter()
        .zip(reference_equity)
        .map(|(primary, reference)| {
            primary
                .checked_add(*reference)
                .ok_or("多腿 exact 合并权益溢出")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let final_equity_exact = *equity_exact.last().ok_or("多腿 exact 合并权益曲线为空")?;
    let return_bps = exact_equity_total_return_bps_v1(initial_cash_exact, final_equity_exact)?;
    let drawdown_curve = std::iter::once(initial_cash_exact)
        .chain(equity_exact.iter().copied())
        .collect::<Vec<_>>();
    let max_drawdown_bps = exact_equity_max_drawdown_bps_v1(&drawdown_curve)?;
    Ok(MultiLegCombinedExactEquity {
        equity_exact,
        initial_cash_exact,
        final_equity_exact,
        return_bps,
        max_drawdown_bps,
    })
}

/// 按时间点合并双腿权益，并从共同初始权益计算组合最大回撤。
pub fn multi_leg_combined_equity_metrics(
    primary_equity: &[i128],
    reference_equity: &[i128],
    primary_initial_cash_raw: i128,
    reference_initial_cash_raw: i128,
    expected_len: usize,
) -> Result<MultiLegCombinedEquity, String> {
    let initial_cash_raw = primary_initial_cash_raw
        .checked_add(reference_initial_cash_raw)
        .ok_or("组合初始权益求和溢出")?;
    let equity_raw = primary_equity
        .iter()
        .zip(reference_equity)
        .map(|(primary, reference)| primary.checked_add(*reference).ok_or("多腿合并权益溢出"))
        .collect::<Result<Vec<_>, _>>()?;
    if equity_raw.len() != expected_len
        || reference_equity.len() != expected_len
        || primary_equity.len() != expected_len
    {
        return Err("多腿合并权益长度与对齐 BarFrame 不一致".into());
    }
    let final_equity_raw = equity_raw.last().copied().ok_or("多腿合并权益曲线为空")?;
    let mut peak = initial_cash_raw;
    let mut max_drawdown_bps = 0_u32;
    for equity in &equity_raw {
        peak = peak.max(*equity);
        if peak > 0 {
            let drawdown = peak
                .checked_sub(*equity)
                .ok_or("组合回撤计算溢出")?
                .checked_mul(10_000)
                .ok_or("组合回撤基点计算溢出")?
                .checked_div(peak)
                .ok_or("组合回撤分母无效")?
                .clamp(0, 10_000) as u32;
            max_drawdown_bps = max_drawdown_bps.max(drawdown);
        }
    }
    Ok(MultiLegCombinedEquity {
        equity_raw,
        initial_cash_raw,
        final_equity_raw,
        max_drawdown_bps,
    })
}

#[cfg(test)]
mod tests {
    use super::multi_leg_combined_exact_equity_metrics;
    use qx_core::ExactDecimal;

    #[test]
    fn independent_leg_composition_keeps_subnano_return_and_drawdown() {
        let decimal = |value: &str| ExactDecimal::parse(value).expect("valid exact decimal");
        let metrics = multi_leg_combined_exact_equity_metrics(
            &[
                decimal("0.000000001"),
                decimal("0.0000000009"),
                decimal("0.000000001"),
            ],
            &[
                decimal("0.000000001"),
                decimal("0.000000001"),
                decimal("0.000000001"),
            ],
            decimal("0.0000000009"),
            decimal("0.000000001"),
            3,
        )
        .expect("combine exact independent leg curves");

        assert_eq!(metrics.equity_exact[0], decimal("0.000000002"));
        assert_eq!(metrics.final_equity_exact, decimal("0.000000002"));
        assert_eq!(metrics.return_bps, 526);
        assert_eq!(metrics.max_drawdown_bps, 500);
    }
}
