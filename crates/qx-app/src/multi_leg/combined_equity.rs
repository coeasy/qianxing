//! 多腿组合权益曲线与回撤归约。

/// 两腿合并后的组合权益事实。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiLegCombinedEquity {
    pub equity_raw: Vec<i128>,
    pub initial_cash_raw: i128,
    pub final_equity_raw: i128,
    pub max_drawdown_bps: u32,
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
