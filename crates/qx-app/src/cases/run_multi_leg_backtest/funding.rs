use super::{invalid, invariant, AppError};
use crate::{MultiLegBacktestSpec, MultiLegSignalBucket};
use qx_core::{InstrumentId, TradingInstrumentSpec};
use qx_xingban::PortfolioBarSessionReport;
use std::collections::BTreeMap;

pub(super) fn input_kind(spec: &MultiLegBacktestSpec) -> &'static str {
    if !spec.funding_events.is_empty() {
        "caller_supplied_historical_events"
    } else if spec.funding_bps != 0 {
        "constant_bps"
    } else {
        "none"
    }
}

pub(super) fn input_events(spec: &MultiLegBacktestSpec) -> Vec<serde_json::Value> {
    spec.funding_events
        .iter()
        .map(|event| {
            serde_json::json!({
                "instrument": event.instrument.to_string(),
                "settlement_ts": event.settlement_ts,
                "rate_bps": event.rate_bps,
            })
        })
        .collect()
}

pub(super) fn rates_for_legs(
    rate_bps: i64,
    primary: &InstrumentId,
    primary_spec: Option<&TradingInstrumentSpec>,
    reference: &InstrumentId,
    reference_spec: Option<&TradingInstrumentSpec>,
) -> BTreeMap<InstrumentId, i64> {
    if rate_bps == 0 {
        return BTreeMap::new();
    }
    [
        (primary.clone(), primary_spec),
        (reference.clone(), reference_spec),
    ]
    .into_iter()
    .filter_map(|(instrument, market_spec)| {
        market_spec
            .filter(|market| market.product.is_derivative())
            .map(|_| (instrument, rate_bps))
    })
    .collect()
}

pub(super) fn attribute_execution_funding(
    report: &PortfolioBarSessionReport,
    primary: &InstrumentId,
    primary_buckets: &mut [MultiLegSignalBucket],
    reference: &InstrumentId,
    reference_buckets: &mut [MultiLegSignalBucket],
    completed_groups_funding_raw: i128,
) -> Result<(i128, i128, i128, i128), AppError> {
    crate::multi_leg_apply_funding(
        primary_buckets,
        report
            .funding_events_by_instrument
            .get(primary)
            .map(Vec::as_slice)
            .unwrap_or_default(),
    )
    .map_err(invalid)?;
    crate::multi_leg_apply_funding(
        reference_buckets,
        report
            .funding_events_by_instrument
            .get(reference)
            .map(Vec::as_slice)
            .unwrap_or_default(),
    )
    .map_err(invalid)?;

    let total_cashflow = report
        .funding_by_instrument_raw
        .values()
        .try_fold(0_i128, |sum, amount| sum.checked_add(*amount))
        .and_then(i128::checked_neg)
        .ok_or_else(|| invalid("共享账簿资金费总额溢出"))?;
    let primary_cost = sum_bucket_funding(primary_buckets, "主腿")?;
    let reference_cost = sum_bucket_funding(reference_buckets, "对冲腿")?;
    let attributed = primary_cost
        .checked_add(reference_cost)
        .ok_or_else(|| invalid("多腿资金费桶合计溢出"))?;
    if attributed != total_cashflow {
        return Err(invariant(format!(
            "腿级资金费归因与共享账簿现金流不一致: attribution={attributed}, ledger={total_cashflow}"
        )));
    }
    let ungrouped = total_cashflow
        .checked_sub(completed_groups_funding_raw)
        .ok_or_else(|| invalid("未配对信号资金费归因溢出"))?;
    Ok((total_cashflow, primary_cost, reference_cost, ungrouped))
}

fn sum_bucket_funding(buckets: &[MultiLegSignalBucket], label: &str) -> Result<i128, AppError> {
    buckets
        .iter()
        .try_fold(0_i128, |sum, bucket| sum.checked_add(bucket.funding_raw()))
        .ok_or_else(|| invalid(format!("{label}资金费累计溢出")))
}
