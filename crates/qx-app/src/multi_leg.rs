//! Shared Rust multi-leg attribution facts used by the application and its facades.
//!
//! 多腿归因内核：信号桶、FIFO 成交归属、腿级保证金与残余费用分摊。
//!
//! 该模块只产出归因事实，不改变撮合结果，供多标的回测与实时投影复用。

mod buckets;
mod combined_equity;
mod combined_return;
mod metrics;
mod multi_builtin_summary;
mod quality;
pub use buckets::{multi_leg_leg_buckets, multi_leg_realized_standing_at};
pub use combined_equity::{multi_leg_combined_equity_metrics, MultiLegCombinedEquity};
pub use combined_return::multi_leg_combined_return_bps;
pub use metrics::{multi_leg_cost_bps, multi_leg_group_totals, multi_leg_leg_integrity};
pub use multi_builtin_summary::{
    multi_builtin_composition_summary_json, MultiBuiltinCompositionSummary,
};
pub use quality::{aggregate_multi_leg_quality_report, multi_leg_quality_fingerprint};

use qx_core::{InstrumentId, Order, OrderStatus, Quantity, Side, TradingInstrumentSpec};
use qx_guanxing::Bar;
use qx_zhenlu::{SpreadGroupAttribution, SpreadLegAttribution, SpreadOrderGroup, SpreadOrderLeg};
use std::collections::BTreeMap;

mod funding;
pub use funding::multi_leg_apply_funding;

use crate::trade_facts::{rejection_count, rejection_facts, rejection_facts_line, spread_group_id};
/// 多腿回测中由单个信号时点触发的腿变化；成交按 FIFO 落入这些时点，
/// 从而把成交额/费用归属到真正触发它的信号组。
#[derive(Clone, Debug)]
pub struct MultiLegSignalBucket {
    ts: u64,
    side: Side,
    planned_qty_raw: i128,
    price_raw: i128,
    filled_qty_raw: i128,
    turnover_raw: i128,
    fees_raw: i128,
    funding_cost_raw: i128,
    /// 实际持仓，用于保证金与资金费归因；未成交信号不增加持仓。
    realized_standing_after_raw: i128,
}

impl MultiLegSignalBucket {
    pub fn funding_raw(&self) -> i128 {
        self.funding_cost_raw
    }
}

/// 单腿归因输入：目标仓位时间序列、Bar、产品规格和该腿的成交报告。
pub struct MultiLegLegFacts<'a> {
    pub label: &'static str,
    pub account_id: &'a str,
    pub instrument: &'a InstrumentId,
    pub targets: &'a BTreeMap<u64, i128>,
    pub bars: &'a [Bar],
    pub spec: Option<&'a TradingInstrumentSpec>,
    pub fills: &'a [qx_core::Fill],
    pub event_log: &'a qx_core::EventLog,
    pub clock_end: u64,
}

/// 资金费结算周期（8 小时）；持仓不足一个周期时按持有毫秒数比例计提。
pub const MULTI_LEG_FUNDING_INTERVAL_MS: u64 = 8 * 60 * 60 * 1_000;

/// 把待对账裸腿列表编码进产物 JSON。
pub fn multi_leg_pending_reconcile_json(items: &[MultiLegPendingReconcile]) -> serde_json::Value {
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
pub fn multi_leg_integrity_json(integrity: &MultiLegLegIntegrity) -> serde_json::Value {
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
pub fn multi_leg_integrity_summary(integrity: &MultiLegLegIntegrity) -> String {
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

pub fn multi_leg_bar_close_raw(bars: &[Bar], ts: u64) -> Option<i128> {
    bars.iter()
        .rev()
        .find(|bar| bar.ts <= ts)
        .map(|bar| bar.close)
}

pub fn multi_leg_leg_margin(
    leg: &MultiLegLegFacts<'_>,
    buckets: &[MultiLegSignalBucket],
    ts: u64,
) -> Result<i128, String> {
    // 保证金只属于能用杠杆的产品：现货腿是全额现金买入，再按 notional 记一笔保证金就是把
    // 同一笔现金算两次。缺 spec 同样按现货口径 —— 衍生品缺 spec 那种组合已经被规格闸门拒掉。
    let Some(spec) = leg.spec.filter(|spec| spec.product.is_derivative()) else {
        return Ok(0);
    };
    let standing_raw = multi_leg_realized_standing_at(buckets, ts)?
        .checked_abs()
        .ok_or_else(|| format!("{} 腿实际持仓绝对值溢出", leg.label))?;
    if standing_raw == 0 {
        return Ok(0);
    }
    let price_raw = multi_leg_bar_close_raw(leg.bars, ts)
        .ok_or_else(|| "多腿归因缺少保证金参考价".to_string())?;
    spec.initial_margin(standing_raw, price_raw, 1)
        .map_err(|error| format!("{} 腿初始保证金非法: {error:?}", leg.label))
}

/// 组级归因的完整产出：成交组、未配对残余，以及"一腿成交、另一腿落空"的待对账事实。
pub struct MultiLegAttribution {
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
pub struct MultiLegPendingReconcile {
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
pub struct MultiLegLegIntegrity {
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

/// 把两条腿的信号桶按时间戳配成 `SpreadOrderGroup` 并汇总组级归因。只有两腿
/// 都能给出**非零实际成交量**的时点才成组；否则该腿的成本计入 `residual_*`，不静默丢弃。
///
/// V11 Q0e：这里的数量口径从"信号计划"改成"实际成交"。改之前只要某条腿在计划里
/// 出现过量，即使那条腿一笔都没成交（现金不足、保证金不足、reduce-only 被挡），组里
/// 仍然会挂上那条腿的计划数量——于是净敞口、保证金与归因费用可以描述一个现实中拿不
/// 到的组合。改后成组与否只看 `filled_qty_raw`；一腿成交、另一腿落空时，成交腿不再被
/// 折进任何组，而是登记成 `pending_reconcile`，让"裸腿"必须在产物里显式存在。
pub fn multi_leg_group_attributions(
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
                client_id: u64::try_from(index + 1)
                    .map_err(|_| "多腿订单编号超出 u64 范围".to_string())?,
                instrument: leg.instrument.clone(),
                side,
                qty: Quantity::from_raw(qty_raw),
                limit: None,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: leg.account_id.to_string(),
                trace: None,
                policy: None,
            });
            attributions.push(SpreadLegAttribution {
                leg_id: (*leg_id).to_string(),
                filled_qty_raw: qty_raw,
                turnover_raw: bucket.map(|bucket| bucket.turnover_raw).unwrap_or(0),
                fees_raw: bucket.map(|bucket| bucket.fees_raw).unwrap_or(0),
                margin_raw: multi_leg_leg_margin(leg, buckets, ts)?,
                funding_raw: bucket.map(|bucket| bucket.funding_cost_raw).unwrap_or(0),
            });
        }
        if !paired || orders.len() != 2 {
            for (leg, buckets) in [(primary, primary_buckets), (reference, reference_buckets)] {
                let Some(bucket) = buckets.iter().find(|bucket| bucket.ts == ts) else {
                    continue;
                };
                residual_fees_raw = residual_fees_raw
                    .checked_add(bucket.fees_raw)
                    .ok_or_else(|| "多腿残余费用合计溢出".to_string())?;
                residual_filled_qty_raw = residual_filled_qty_raw
                    .checked_add(bucket.filled_qty_raw)
                    .ok_or_else(|| "多腿残余成交量合计溢出".to_string())?;
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
