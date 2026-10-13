use super::super::run_multi_leg_backtest_with_funding_provider::FundingRateProviderProvenance;
use super::{invalid, invariant, AppError};
use crate::{MultiLegBacktestSpec, MultiLegSignalBucket};
use qx_core::{Fnv1a, InstrumentId, TradingInstrumentSpec};
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

pub(super) fn hash_provider_source(
    hasher: &mut Fnv1a,
    source: Option<&FundingRateProviderProvenance>,
) {
    if let Some(source) = source {
        hasher.write_text(&source.provider_id);
        hasher.write_text(&source.provider_version);
        hasher.write_text(&source.request_id);
        hasher.write_text(&source.source_hash);
        hasher.write_text(&source.query_hash);
        for provider in &source.retry_chain {
            hasher.write_text(provider);
        }
    }
}

pub(super) fn apply_artifact_metadata(
    payload: &mut serde_json::Value,
    spec: &MultiLegBacktestSpec,
    source: Option<&FundingRateProviderProvenance>,
) -> Result<(), AppError> {
    let object = payload
        .as_object_mut()
        .ok_or_else(|| invariant("多腿回测产物根节点不是 JSON object"))?;
    if let Some(account_id) = spec.account_id.as_deref() {
        object.insert("account_id".into(), serde_json::json!(account_id));
        object.insert("primary_account_id".into(), serde_json::json!(account_id));
        object.insert("reference_account_id".into(), serde_json::json!(account_id));
    }
    object.insert(
        "funding_rate_input".into(),
        serde_json::Value::String(input_kind(spec).to_string()),
    );
    object.insert(
        "funding_rate_events".into(),
        serde_json::to_value(input_events(spec))
            .map_err(|error| invariant(format!("资金费事件序列化失败: {error}")))?,
    );
    let provenance = source
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| invariant(format!("资金费 Provider 来源序列化失败: {error}")))?
        .unwrap_or(serde_json::Value::Null);
    object.insert("funding_provider_provenance".into(), provenance);
    Ok(())
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
