//! 多腿归因内核：信号桶、FIFO 成交归属、腿级保证金与残余费用分摊。
//!
//! 该模块只产出归因事实，不改变撮合结果，供多标的回测与实时投影复用。

use super::*;
/// 多腿回测中由单个信号时点触发的腿变化；成交按 FIFO 落入这些时点，
/// 从而把成交额/费用归属到真正触发它的信号组。
#[derive(Clone, Debug)]
pub(crate) struct MultiLegSignalBucket {
    ts: u64,
    side: Side,
    planned_qty_raw: i128,
    price_raw: i128,
    filled_qty_raw: i128,
    turnover_raw: i128,
    fees_raw: i128,
    funding_raw: i128,
    /// 按实际成交（而非信号计划）累计出来的持仓，用于保证金与资金费归因。
    /// 被风控或资金约束挡掉的信号成交为 0，不能按目标仓位收保证金。
    realized_standing_after_raw: i128,
}

/// 单腿归因输入：目标仓位时间序列、Bar、产品规格和该腿的成交报告。
pub(crate) struct MultiLegLegFacts<'a> {
    pub(crate) label: &'static str,
    pub(crate) instrument: &'a InstrumentId,
    pub(crate) targets: &'a BTreeMap<u64, i128>,
    pub(crate) bars: &'a [Bar],
    pub(crate) spec: Option<&'a TradingInstrumentSpec>,
    pub(crate) report: &'a qx_xingban::BacktestReport,
}

/// 资金费结算周期（8 小时）；持仓不足一个周期时按持有毫秒数比例计提。
pub(crate) const MULTI_LEG_FUNDING_INTERVAL_MS: u64 = 8 * 60 * 60 * 1_000;

/// 把待对账裸腿列表编码进产物 JSON。
pub(crate) fn multi_leg_pending_reconcile_json(
    items: &[MultiLegPendingReconcile],
) -> serde_json::Value {
    serde_json::json!({
        "policy": "mark-pending-reconcile-no-auto-close",
        "items": items.iter().map(|item| serde_json::json!({
            "leg": item.leg,
            "counterpart": item.counterpart,
            "ts": item.ts,
            "filled_qty_raw": item.filled_qty_raw,
            "turnover_raw": item.turnover_raw,
            "fees_raw": item.fees_raw,
            "counterpart_filled_qty_raw": item.counterpart_filled_qty_raw,
        })).collect::<Vec<_>>(),
    })
}

/// 把单腿计划与实际成交的差距编码进产物 JSON。
pub(crate) fn multi_leg_integrity_json(integrity: &MultiLegLegIntegrity) -> serde_json::Value {
    serde_json::json!({
        "label": integrity.label,
        "instrument": integrity.instrument,
        "planned_qty_raw": integrity.planned_qty_raw,
        "filled_qty_raw": integrity.filled_qty_raw,
        "unfilled_planned_qty_raw": integrity.unfilled_planned_qty_raw,
        "vetoed_signal_ts": integrity.vetoed_signal_ts,
        "rejected_orders": integrity.rejected_orders,
        "rejection_reasons": integrity
            .rejection_reasons
            .iter()
            .map(|(reason, count)| serde_json::json!({ "reason": reason, "count": count }))
            .collect::<Vec<_>>(),
    })
}

/// 把一行单腿成交诚实性事实展开成 `key=value` 序列。前缀是腿名，方便 CLI 输出与
/// 测试解析共用同一份口径；拒单原因里的空格换成 `_`，保证一个 token 一件事实。
pub(crate) fn multi_leg_integrity_summary(integrity: &MultiLegLegIntegrity) -> String {
    [
        ("planned_qty_raw", integrity.planned_qty_raw.clone()),
        ("filled_qty_raw", integrity.filled_qty_raw.clone()),
        (
            "unfilled_planned_qty_raw",
            integrity.unfilled_planned_qty_raw.clone(),
        ),
        (
            "vetoed_signals",
            integrity.vetoed_signal_ts.len().to_string(),
        ),
        ("rejected_orders", integrity.rejected_orders.to_string()),
        (
            "rejection_reasons",
            rejection_facts_line(&integrity.rejection_reasons),
        ),
    ]
    .into_iter()
    .map(|(key, value)| format!("{}_{}={}", integrity.label, key, value))
    .collect::<Vec<_>>()
    .join(" ")
}

pub(crate) fn multi_leg_bar_close_raw(bars: &[Bar], ts: u64) -> Option<i128> {
    bars.iter()
        .rev()
        .find(|bar| bar.ts <= ts)
        .map(|bar| bar.close)
}

pub(crate) fn multi_leg_leg_buckets(
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
                planned_qty_raw: delta.abs(),
                price_raw,
                filled_qty_raw: 0,
                turnover_raw: 0,
                fees_raw: 0,
                funding_raw: 0,
                realized_standing_after_raw: 0,
            });
        }
        standing = *target;
    }
    // FIFO 分配成交数量与费用；费用余数归入该笔成交覆盖的最后一个桶，
    // 保证组级合计与腿级 `fees_raw` 严格相等。
    let mut cursor = 0usize;
    let mut remaining = buckets
        .iter()
        .map(|bucket| bucket.planned_qty_raw)
        .collect::<Vec<_>>();
    for fill in &leg.report.fills {
        if fill.fee.raw() < 0 {
            return Err(format!(
                "{} 腿存在负费用成交，多腿归因要求费用非负",
                leg.label
            ));
        }
        let mut qty = fill.qty.raw();
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
                fee_left.saturating_mul(alloc) / qty_left
            };
            fee_left -= share;
            qty_left -= alloc;
            let bucket = &mut buckets[cursor];
            bucket.filled_qty_raw += alloc;
            // 成交额与 Bar 引擎同口径：现货按 qty*price/SCALE，衍生品按合约名义额。
            bucket.turnover_raw += match leg.spec {
                Some(spec) => spec
                    .notional(alloc, fill.price.raw())
                    .map_err(|error| format!("{} 腿成交额名义额非法: {error:?}", leg.label))?,
                None => qx_core::fee::notional(alloc, fill.price.raw()),
            };
            bucket.fees_raw += share;
        }
    }
    // 实际持仓序列：保证金与资金费只能对真正成交的仓位计费，被风控或资金
    // 约束挡掉的信号成交为 0，不能按目标仓位计费。
    let mut realized = 0_i128;
    for bucket in &mut buckets {
        realized += match bucket.side {
            Side::Buy => bucket.filled_qty_raw,
            Side::Sell => -bucket.filled_qty_raw,
        };
        bucket.realized_standing_after_raw = realized;
    }
    if funding_bps != 0 {
        // 资金费按实际持仓分段计提：持有到下一个信号或回测结束，按持有时长折算
        // 8 小时周期；多头支付为正，空头收取为负。
        //
        // 只向衍生品腿计提：现货没有资金费，给它挂一笔等于造一个不存在的成本。缺 spec
        // 的腿按现货口径跳过 —— 而"声称要资金费却缺 spec"已由 `multi_leg_spec_guard`
        // 在跑之前就拒掉，所以这里不会把一条衍生品腿的钱静默记成 0（V11 Q58）。
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
                    .unwrap_or(leg.report.clock_end);
                if hold_end <= start_ts {
                    continue;
                }
                let duration_ms = hold_end - start_ts;
                let notional = spec
                    .notional(standing_raw.abs(), buckets[index].price_raw)
                    .map_err(|error| format!("{} 腿资金费名义额非法: {error:?}", leg.label))?;
                let funding = notional
                    .saturating_mul(i128::from(funding_bps))
                    .saturating_mul(i128::from(duration_ms))
                    / (10_000 * i128::from(MULTI_LEG_FUNDING_INTERVAL_MS));
                buckets[index].funding_raw = if standing_raw > 0 { funding } else { -funding };
            }
        }
    }
    Ok(buckets)
}

/// 某个时点上该腿按实际成交累计的持仓（信号计划不等于已成交）。
pub(crate) fn multi_leg_realized_standing_at(buckets: &[MultiLegSignalBucket], ts: u64) -> i128 {
    buckets
        .iter()
        .filter(|bucket| bucket.ts <= ts)
        .map(|bucket| match bucket.side {
            Side::Buy => bucket.filled_qty_raw,
            Side::Sell => -bucket.filled_qty_raw,
        })
        .sum()
}

pub(crate) fn multi_leg_leg_margin(
    leg: &MultiLegLegFacts<'_>,
    buckets: &[MultiLegSignalBucket],
    ts: u64,
) -> Result<i128, String> {
    // 保证金只属于能用杠杆的产品：现货腿是全额现金买入，再按 notional 记一笔保证金就是把
    // 同一笔现金算两次。缺 spec 同样按现货口径 —— 衍生品缺 spec 那种组合已经被规格闸门拒掉。
    let Some(spec) = leg.spec.filter(|spec| spec.product.is_derivative()) else {
        return Ok(0);
    };
    let standing_raw = multi_leg_realized_standing_at(buckets, ts).abs();
    if standing_raw == 0 {
        return Ok(0);
    }
    let price_raw = multi_leg_bar_close_raw(leg.bars, ts)
        .ok_or_else(|| "多腿归因缺少保证金参考价".to_string())?;
    spec.initial_margin(standing_raw, price_raw, 1)
        .map_err(|error| format!("{} 腿初始保证金非法: {error:?}", leg.label))
}

/// 组级归因的完整产出：成交组、未配对残余，以及"一腿成交、另一腿落空"的待对账事实。
pub(crate) struct MultiLegAttribution {
    pub groups: Vec<SpreadGroupAttribution>,
    /// 未能成组的信号时点上，两条腿实际已成交的费用合计。
    pub residual_fees_raw: i128,
    /// 同上，按数量口径。
    pub residual_filled_qty_raw: i128,
    /// 跨腿失衡时点：本腿在该时点有真实成交、对手腿没有任何成交。
    ///
    /// 这里的数量与 `residual_*` 描述的是同一批成交，**不是**第二次计费；它存在的
    /// 唯一理由是 §6 Q0e 的要求——一腿被拒时必须对另一腿有显式动作，而不是静默把
    /// 裸敞口写进"某个组"里当作已完成套利。
    pub pending_reconcile: Vec<MultiLegPendingReconcile>,
}

/// 一腿已成交、对手腿被挡下时，对该腿登记的显式待处理事实。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MultiLegPendingReconcile {
    pub leg: &'static str,
    pub counterpart: &'static str,
    pub ts: u64,
    pub filled_qty_raw: String,
    pub turnover_raw: String,
    pub fees_raw: String,
    pub counterpart_filled_qty_raw: String,
}

/// 单腿成交诚实性事实：信号计划了多少、真正成交了多少、被挡下多少以及拒单原因。
#[derive(Clone, Debug)]
pub(crate) struct MultiLegLegIntegrity {
    pub label: &'static str,
    pub instrument: String,
    pub planned_qty_raw: String,
    pub filled_qty_raw: String,
    pub unfilled_planned_qty_raw: String,
    /// 有计划但整根 Bar 零成交的时点：策略以为建了仓，账上其实没有。
    pub vetoed_signal_ts: Vec<u64>,
    pub rejected_orders: usize,
    /// `(原因, 次数)`，按次数降序；原因直接从该腿事件日志读，不重新推断。
    pub rejection_reasons: Vec<(String, usize)>,
}

/// 汇总一条腿"计划 vs 实际成交"的差距，并把引擎写进事件日志的拒单原因归并出来。
pub(crate) fn multi_leg_leg_integrity(
    leg: &MultiLegLegFacts<'_>,
    buckets: &[MultiLegSignalBucket],
) -> MultiLegLegIntegrity {
    let planned_qty_raw = buckets
        .iter()
        .map(|bucket| bucket.planned_qty_raw)
        .sum::<i128>();
    let filled_qty_raw = buckets
        .iter()
        .map(|bucket| bucket.filled_qty_raw)
        .sum::<i128>();
    let rejections = rejection_facts(&leg.report.event_log);
    MultiLegLegIntegrity {
        label: leg.label,
        instrument: leg.instrument.to_string(),
        rejected_orders: rejection_count(&rejections),
        rejection_reasons: rejections,
        vetoed_signal_ts: buckets
            .iter()
            .filter(|bucket| bucket.filled_qty_raw == 0)
            .map(|bucket| bucket.ts)
            .collect(),
        unfilled_planned_qty_raw: (planned_qty_raw - filled_qty_raw).to_string(),
        planned_qty_raw: planned_qty_raw.to_string(),
        filled_qty_raw: filled_qty_raw.to_string(),
    }
}

/// 把两条腿的信号桶按时间戳配成 `SpreadOrderGroup` 并汇总组级归因。只有两腿
/// 都能给出**非零实际成交量**的时点才成组；否则该腿的成本计入 `residual_*`，不静默丢弃。
///
/// V11 Q0e：这里的数量口径从"信号计划"改成"实际成交"。改之前只要某条腿在计划里
/// 出现过量，即使那条腿一笔都没成交（现金不足、保证金不足、reduce-only 被挡），组里
/// 仍然会挂上那条腿的计划数量——于是净敞口、保证金与归因费用可以描述一个现实中拿不
/// 到的组合。改后成组与否只看 `filled_qty_raw`；一腿成交、另一腿落空时，成交腿不再被
/// 折进任何组，而是登记成 `pending_reconcile`，让"裸腿"必须在产物里显式存在。
pub(crate) fn multi_leg_group_attributions(
    strategy_id: &str,
    primary: &MultiLegLegFacts<'_>,
    reference: &MultiLegLegFacts<'_>,
    primary_buckets: &[MultiLegSignalBucket],
    reference_buckets: &[MultiLegSignalBucket],
) -> Result<MultiLegAttribution, String> {
    let mut timestamps = primary_buckets
        .iter()
        .chain(reference_buckets)
        .map(|bucket| bucket.ts)
        .collect::<Vec<_>>();
    timestamps.sort_unstable();
    timestamps.dedup();
    let mut groups = Vec::new();
    let mut residual_fees_raw = 0_i128;
    let mut residual_filled_qty_raw = 0_i128;
    let mut pending_reconcile = Vec::new();
    for (index, ts) in timestamps.into_iter().enumerate() {
        let legs = [(primary, primary_buckets), (reference, reference_buckets)];
        let mut orders = Vec::with_capacity(2);
        let mut attributions = Vec::with_capacity(2);
        let mut paired = true;
        for (leg_id, (leg, buckets)) in ["primary", "reference"].iter().zip(legs) {
            let bucket = buckets.iter().find(|bucket| bucket.ts == ts);
            // 只看实际成交：计划量再大，没成交就不能进入组。
            let qty_raw = bucket.map(|bucket| bucket.filled_qty_raw).unwrap_or(0);
            if qty_raw == 0 {
                paired = false;
                continue;
            }
            let side = bucket.map(|bucket| bucket.side).unwrap_or(Side::Buy);
            orders.push(Order {
                client_id: u64::try_from(index + 1).expect("usize 与 u64 同宽"),
                instrument: leg.instrument.clone(),
                side,
                qty: Quantity::from_raw(qty_raw),
                limit: None,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: format!("multi-leg-{}", leg.label),
                trace: None,
                policy: None,
            });
            attributions.push(SpreadLegAttribution {
                leg_id: (*leg_id).to_string(),
                filled_qty_raw: qty_raw,
                turnover_raw: bucket.map(|bucket| bucket.turnover_raw).unwrap_or(0),
                fees_raw: bucket.map(|bucket| bucket.fees_raw).unwrap_or(0),
                margin_raw: multi_leg_leg_margin(leg, buckets, ts)?,
                funding_raw: bucket.map(|bucket| bucket.funding_raw).unwrap_or(0),
            });
        }
        if !paired || orders.len() != 2 {
            for (leg, buckets) in [(primary, primary_buckets), (reference, reference_buckets)] {
                let Some(bucket) = buckets.iter().find(|bucket| bucket.ts == ts) else {
                    continue;
                };
                residual_fees_raw += bucket.fees_raw;
                residual_filled_qty_raw += bucket.filled_qty_raw;
                // 该腿有成交而组没成 → 裸腿；对手腿的成交量一并写出来，便于
                // 人读产物时立刻看出"谁被挡了"。
                if bucket.filled_qty_raw > 0 {
                    let counterpart = if leg.label == "primary" {
                        ("reference", reference_buckets)
                    } else {
                        ("primary", primary_buckets)
                    };
                    let counterpart_filled = counterpart
                        .1
                        .iter()
                        .find(|other| other.ts == ts)
                        .map(|other| other.filled_qty_raw)
                        .unwrap_or(0);
                    pending_reconcile.push(MultiLegPendingReconcile {
                        leg: leg.label,
                        counterpart: counterpart.0,
                        ts,
                        filled_qty_raw: bucket.filled_qty_raw.to_string(),
                        turnover_raw: bucket.turnover_raw.to_string(),
                        fees_raw: bucket.fees_raw.to_string(),
                        counterpart_filled_qty_raw: counterpart_filled.to_string(),
                    });
                }
            }
            continue;
        }
        let group = SpreadOrderGroup::new(
            spread_group_id(strategy_id, 0, ts),
            strategy_id,
            orders
                .iter()
                .cloned()
                .zip(["primary", "reference"])
                .map(|(order, leg_id)| SpreadOrderLeg {
                    leg_id: leg_id.to_string(),
                    venue_id: order.instrument.venue.to_string(),
                    order,
                })
                .collect(),
        )
        .map_err(|error| format!("创建多腿归因订单组失败: {error:?}"))?;
        groups.push(
            group
                .attribute(attributions)
                .map_err(|error| format!("多腿归因失败: {error}"))?,
        );
    }
    Ok(MultiLegAttribution {
        groups,
        residual_fees_raw,
        residual_filled_qty_raw,
        pending_reconcile,
    })
}
