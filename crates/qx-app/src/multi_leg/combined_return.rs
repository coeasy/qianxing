/// Weighted combined return for the two independent accounts in a multi-builtin run.
/// Each leg contributes its actual initial cash; averaging leg-level returns would assign
/// equal weight to accounts with different capital bases.
pub fn multi_leg_combined_return_bps(legs: [(&str, i128, i128); 2]) -> Result<i64, String> {
    let mut pnl_raw = 0_i128;
    let mut funded_raw = 0_i128;
    for (label, initial_raw, final_raw) in legs {
        let leg_pnl = final_raw
            .checked_sub(initial_raw)
            .ok_or_else(|| format!("{label} 腿权益差值溢出"))?;
        pnl_raw = pnl_raw
            .checked_add(leg_pnl)
            .ok_or_else(|| format!("多腿组合盈亏合计溢出（{label} 腿）"))?;
        funded_raw = funded_raw
            .checked_add(initial_raw)
            .ok_or_else(|| format!("多腿组合本金合计溢出（{label} 腿）"))?;
    }
    if funded_raw <= 0 {
        return Err(format!(
            "多腿组合本金合计必须为正才能度量收益，当前为 {funded_raw}"
        ));
    }
    let bps = pnl_raw
        .checked_mul(10_000)
        .and_then(|value| value.checked_div(funded_raw))
        .ok_or_else(|| "多腿组合收益折算 bps 溢出".to_string())?;
    i64::try_from(bps).map_err(|_| format!("多腿组合收益 {bps} bps 超出可表示范围"))
}
