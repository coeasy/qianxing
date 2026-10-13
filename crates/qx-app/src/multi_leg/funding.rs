use super::MultiLegSignalBucket;

/// Assign executed funding cashflows to the signal bucket whose realized
/// position was open at the settlement time.
pub fn multi_leg_apply_funding(
    buckets: &mut [MultiLegSignalBucket],
    cashflows: &[(u64, i128)],
) -> Result<(), String> {
    let mut previous_ts = None;
    for (ts, amount) in cashflows {
        if previous_ts.is_some_and(|previous| *ts < previous) {
            return Err("资金费现金流必须按时间排序".into());
        }
        previous_ts = Some(*ts);
        let bucket = buckets
            .iter_mut()
            .rev()
            .find(|bucket| bucket.ts <= *ts && bucket.realized_standing_after_raw != 0)
            .ok_or_else(|| format!("资金费时间戳 {ts} 没有对应的已成交持仓桶"))?;
        bucket.funding_cost_raw = bucket
            .funding_cost_raw
            // Bucket is positive for an expense; Ledger cashflow uses the inverse sign.
            .checked_sub(*amount)
            .ok_or_else(|| "资金费信号桶累计溢出".to_string())?;
    }
    Ok(())
}
