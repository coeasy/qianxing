use super::{MultiLegLegFacts, MultiLegLegIntegrity, MultiLegSignalBucket};
use qx_zhenlu::SpreadGroupAttribution;

/// Fold group attribution once, with checked sums for every additive metric.
/// Margin is a peak, not a sum: both legs share the same account exposure.
pub fn multi_leg_group_totals(
    groups: &[SpreadGroupAttribution],
) -> Result<(i128, i128, i128, i128, i128), String> {
    groups.iter().try_fold(
        (0_i128, 0_i128, 0_i128, 0_i128, 0_i128),
        |mut acc, group| {
            acc.0 = acc
                .0
                .checked_add(group.total_fees_raw)
                .ok_or_else(|| "多腿组级费用合计溢出".to_string())?;
            acc.1 = acc
                .1
                .checked_add(group.total_turnover_raw)
                .ok_or_else(|| "多腿组级成交额合计溢出".to_string())?;
            acc.2 = acc
                .2
                .checked_add(group.total_funding_raw)
                .ok_or_else(|| "多腿组级资金费合计溢出".to_string())?;
            acc.3 = acc
                .3
                .checked_add(group.total_filled_qty_raw)
                .ok_or_else(|| "多腿组级成交量合计溢出".to_string())?;
            acc.4 = acc.4.max(group.total_margin_raw);
            Ok(acc)
        },
    )
}

/// Net cost in basis points of turnover; undefined turnover remains `None`.
pub fn multi_leg_cost_bps(net_cost_raw: i128, turnover_raw: i128) -> Result<Option<i64>, String> {
    if turnover_raw <= 0 {
        return Ok(None);
    }
    let scaled = net_cost_raw
        .checked_mul(10_000)
        .ok_or_else(|| format!("多腿单位成本折算溢出: net_cost_raw={net_cost_raw}"))?;
    let ratio = scaled / turnover_raw;
    i64::try_from(ratio).map(Some).map_err(|_| {
        format!(
            "多腿单位成本超出 i64 可印范围，拒绝输出该值: \
             net_cost_raw={net_cost_raw} turnover_raw={turnover_raw} ratio_raw={ratio}"
        )
    })
}

/// Planned versus filled quantity facts with checked aggregate arithmetic.
pub fn multi_leg_leg_integrity(
    leg: &MultiLegLegFacts<'_>,
    buckets: &[MultiLegSignalBucket],
) -> Result<MultiLegLegIntegrity, String> {
    let planned_qty_raw = buckets.iter().try_fold(0_i128, |sum, bucket| {
        sum.checked_add(bucket.planned_qty_raw)
            .ok_or_else(|| format!("{} 腿计划成交量合计溢出", leg.label))
    })?;
    let filled_qty_raw = buckets.iter().try_fold(0_i128, |sum, bucket| {
        sum.checked_add(bucket.filled_qty_raw)
            .ok_or_else(|| format!("{} 腿实际成交量合计溢出", leg.label))
    })?;
    let unfilled_planned_qty_raw = planned_qty_raw
        .checked_sub(filled_qty_raw)
        .ok_or_else(|| format!("{} 腿未成交计划量溢出", leg.label))?;
    let rejections = super::rejection_facts(leg.event_log);
    Ok(MultiLegLegIntegrity {
        label: leg.label,
        instrument: leg.instrument.to_string(),
        rejected_orders: super::rejection_count(&rejections),
        rejection_reasons: rejections,
        vetoed_signal_ts: buckets
            .iter()
            .filter(|bucket| bucket.filled_qty_raw == 0)
            .map(|bucket| bucket.ts)
            .collect(),
        unfilled_planned_qty_raw: unfilled_planned_qty_raw.to_string(),
        planned_qty_raw: planned_qty_raw.to_string(),
        filled_qty_raw: filled_qty_raw.to_string(),
    })
}
