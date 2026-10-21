use super::*;

pub fn multi_leg_leg_buckets(
    leg: &MultiLegLegFacts<'_>,
    funding_bps: i64,
) -> Result<Vec<MultiLegSignalBucket>, String> {
    let mut buckets: Vec<MultiLegSignalBucket> = Vec::new();
    let mut standing = 0_i128;
    for (ts, target) in leg.targets {
        let delta = target
            .checked_sub(standing)
            .ok_or_else(|| format!("{} 腿目标仓位溢出", leg.label))?;
        if delta != 0 {
            let price_raw = multi_leg_bar_close_raw(leg.bars, *ts)
                .ok_or_else(|| format!("{} 腿在 ts={ts} 之前没有可见 Bar", leg.label))?;
            buckets.push(MultiLegSignalBucket {
                ts: *ts,
                side: if delta > 0 { Side::Buy } else { Side::Sell },
                planned_qty_raw: delta
                    .checked_abs()
                    .ok_or_else(|| format!("{} 腿计划数量绝对值溢出", leg.label))?,
                price_raw,
                filled_qty_raw: 0,
                turnover_raw: 0,
                fees_raw: 0,
                funding_cost_raw: 0,
                realized_standing_after_raw: 0,
            });
        }
        standing = *target;
    }

    let mut cursor = 0usize;
    let mut remaining = buckets
        .iter()
        .map(|bucket| bucket.planned_qty_raw)
        .collect::<Vec<_>>();
    for fill in leg.fills {
        if fill.fee.raw() < 0 {
            return Err(format!(
                "{} 腿存在负费用成交，多腿归因要求费用非负",
                leg.label
            ));
        }
        let mut qty = fill.qty.raw();
        if qty <= 0 {
            return Err(format!("{} 腿成交数量必须为正", leg.label));
        }
        let mut qty_left = qty;
        let mut fee_left = fill.fee.raw();
        while qty > 0 {
            while cursor < buckets.len() && remaining[cursor] == 0 {
                cursor += 1;
            }
            if cursor >= buckets.len() {
                return Err(format!("{} 腿成交数量超出信号计划，无法归因", leg.label));
            }
            let alloc = remaining[cursor].min(qty);
            remaining[cursor] -= alloc;
            qty -= alloc;
            let share = if alloc == qty_left {
                fee_left
            } else {
                fee_left
                    .checked_mul(alloc)
                    .and_then(|value| value.checked_div(qty_left))
                    .ok_or_else(|| format!("{} 腿成交费用按 FIFO 分摊溢出", leg.label))?
            };
            fee_left = fee_left
                .checked_sub(share)
                .ok_or_else(|| format!("{} 腿待分摊费用溢出", leg.label))?;
            qty_left -= alloc;
            let notional = match leg.spec {
                Some(spec) => spec
                    .notional(alloc, fill.price.raw())
                    .map_err(|error| format!("{} 腿成交额名义额非法: {error:?}", leg.label))?,
                None => alloc
                    .checked_mul(fill.price.raw())
                    .and_then(|value| value.checked_div(qx_core::SCALE))
                    .ok_or_else(|| format!("{} 腿现货成交额溢出", leg.label))?,
            };
            let bucket = &mut buckets[cursor];
            bucket.filled_qty_raw = bucket
                .filled_qty_raw
                .checked_add(alloc)
                .ok_or_else(|| format!("{} 腿桶成交量溢出", leg.label))?;
            bucket.turnover_raw = bucket
                .turnover_raw
                .checked_add(notional)
                .ok_or_else(|| format!("{} 腿桶成交额溢出", leg.label))?;
            bucket.fees_raw = bucket
                .fees_raw
                .checked_add(share)
                .ok_or_else(|| format!("{} 腿桶费用溢出", leg.label))?;
        }
    }

    let mut realized = 0_i128;
    for bucket in &mut buckets {
        let signed_qty = match bucket.side {
            Side::Buy => bucket.filled_qty_raw,
            Side::Sell => bucket
                .filled_qty_raw
                .checked_neg()
                .ok_or_else(|| format!("{} 腿卖出成交量符号溢出", leg.label))?,
        };
        realized = realized
            .checked_add(signed_qty)
            .ok_or_else(|| format!("{} 腿实际持仓累计溢出", leg.label))?;
        bucket.realized_standing_after_raw = realized;
    }

    if funding_bps != 0 {
        if let Some(spec) = leg.spec.filter(|spec| spec.product.is_derivative()) {
            for index in 0..buckets.len() {
                let standing_raw = buckets[index].realized_standing_after_raw;
                if standing_raw == 0 {
                    continue;
                }
                let start_ts = buckets[index].ts;
                let hold_end = buckets
                    .get(index + 1)
                    .map(|later| later.ts)
                    .unwrap_or(leg.clock_end);
                if hold_end <= start_ts {
                    continue;
                }
                let duration_ms = hold_end - start_ts;
                let absolute_standing = standing_raw
                    .checked_abs()
                    .ok_or_else(|| format!("{} 腿资金费持仓绝对值溢出", leg.label))?;
                let notional = spec
                    .notional(absolute_standing, buckets[index].price_raw)
                    .map_err(|error| format!("{} 腿资金费名义额非法: {error:?}", leg.label))?;
                let funding = notional
                    .checked_mul(i128::from(funding_bps))
                    .and_then(|value| value.checked_mul(i128::from(duration_ms)))
                    .and_then(|value| {
                        value.checked_div(10_000 * i128::from(MULTI_LEG_FUNDING_INTERVAL_MS))
                    })
                    .ok_or_else(|| format!("{} 腿资金费估算溢出", leg.label))?;
                buckets[index].funding_cost_raw = if standing_raw > 0 {
                    funding
                } else {
                    funding
                        .checked_neg()
                        .ok_or_else(|| format!("{} 腿资金费符号转换溢出", leg.label))?
                };
            }
        }
    }
    Ok(buckets)
}

/// Realized position from fills visible at `ts`; planned target positions are excluded.
pub fn multi_leg_realized_standing_at(
    buckets: &[MultiLegSignalBucket],
    ts: u64,
) -> Result<i128, String> {
    buckets
        .iter()
        .filter(|bucket| bucket.ts <= ts)
        .try_fold(0_i128, |standing, bucket| {
            let qty = match bucket.side {
                Side::Buy => bucket.filled_qty_raw,
                Side::Sell => bucket
                    .filled_qty_raw
                    .checked_neg()
                    .ok_or_else(|| "多腿卖出成交量符号溢出".to_string())?,
            };
            standing
                .checked_add(qty)
                .ok_or_else(|| "多腿实际持仓累计溢出".to_string())
        })
}
