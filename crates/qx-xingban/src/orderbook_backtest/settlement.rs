use qx_core::{
    Event, EventKind, EventLog, ExactDecimal, ExactInstrumentFunding, FundingRateEvent,
    InstrumentId, Ledger, Priority, TradingInstrumentSpec, FUNDING_INTERVAL_MS,
};
use std::collections::{BTreeMap, BTreeSet};

const MAX_FUNDING_SETTLEMENTS: u64 = 100_000;

pub(super) fn scheduled_settlements(
    start: u64,
    end: u64,
    rates: &BTreeMap<InstrumentId, i64>,
    events: &[FundingRateEvent],
    specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
) -> Result<BTreeMap<u64, BTreeMap<InstrumentId, (i64, u64)>>, qx_core::QxError> {
    if start > end || events.len() > MAX_FUNDING_SETTLEMENTS as usize {
        return Err(qx_core::QxError::BusinessViolation(
            "深度资金费时间范围或历史事件数无效".into(),
        ));
    }
    for (instrument, rate) in rates {
        let Some(spec) = specs.get(instrument) else {
            return Err(qx_core::QxError::BusinessViolation(format!(
                "资金费标的 {instrument} 不属于深度回测"
            )));
        };
        if !(-10_000..=10_000).contains(rate) || (*rate != 0 && !spec.product.is_derivative()) {
            return Err(qx_core::QxError::BusinessViolation(format!(
                "资金费配置无效: instrument={instrument}, rate_bps={rate}"
            )));
        }
    }
    let mut event_keys = BTreeSet::new();
    let mut event_instruments = BTreeSet::new();
    for event in events {
        let Some(spec) = specs.get(&event.instrument) else {
            return Err(qx_core::QxError::BusinessViolation(format!(
                "历史资金费标的 {} 不属于深度回测",
                event.instrument
            )));
        };
        if !(-10_000..=10_000).contains(&event.rate_bps)
            || (event.rate_bps != 0 && !spec.product.is_derivative())
            || event.settlement_ts == 0
            || !event.has_valid_funding_interval()
            || !event_keys.insert((&event.instrument, event.settlement_ts))
        {
            return Err(qx_core::QxError::BusinessViolation(format!(
                "历史资金费事件无效或重复: instrument={}, settlement_ts={}, rate_bps={}",
                event.instrument, event.settlement_ts, event.rate_bps
            )));
        }
        event_instruments.insert(&event.instrument);
    }
    if rates
        .iter()
        .any(|(instrument, rate)| *rate != 0 && event_instruments.contains(instrument))
    {
        return Err(qx_core::QxError::BusinessViolation(
            "同一标的不能混用常量资金费率与历史资金费事件".into(),
        ));
    }

    let mut settlements = BTreeMap::<u64, BTreeMap<InstrumentId, (i64, u64)>>::new();
    if rates.values().any(|rate| *rate != 0) {
        let first = start
            .checked_div(FUNDING_INTERVAL_MS)
            .and_then(|period| period.checked_add(1))
            .and_then(|period| period.checked_mul(FUNDING_INTERVAL_MS))
            .ok_or_else(|| qx_core::QxError::BusinessViolation("资金费结算时间溢出".into()))?;
        if first <= end {
            let count = (end - first) / FUNDING_INTERVAL_MS + 1;
            if count > MAX_FUNDING_SETTLEMENTS {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "资金费结算次数 {count} 超过上限 {MAX_FUNDING_SETTLEMENTS}"
                )));
            }
            let mut ts = first;
            while ts <= end {
                settlements.insert(
                    ts,
                    rates
                        .iter()
                        .filter(|(_, rate)| **rate != 0)
                        .map(|(instrument, rate)| {
                            (instrument.clone(), (*rate, FUNDING_INTERVAL_MS))
                        })
                        .collect(),
                );
                let Some(next) = ts.checked_add(FUNDING_INTERVAL_MS) else {
                    break;
                };
                ts = next;
            }
        }
    }
    for event in events
        .iter()
        .filter(|event| event.settlement_ts > start && event.settlement_ts <= end)
    {
        let at_ts = settlements.entry(event.settlement_ts).or_default();
        if at_ts
            .insert(
                event.instrument.clone(),
                (event.rate_bps, event.effective_funding_interval_ms()),
            )
            .is_some()
        {
            return Err(qx_core::QxError::BusinessViolation(format!(
                "资金费结算时间冲突: instrument={}, settlement_ts={}",
                event.instrument, event.settlement_ts
            )));
        }
    }
    if settlements.len() as u64 > MAX_FUNDING_SETTLEMENTS {
        return Err(qx_core::QxError::BusinessViolation(format!(
            "资金费结算次数 {} 超过上限 {MAX_FUNDING_SETTLEMENTS}",
            settlements.len()
        )));
    }
    Ok(settlements)
}

pub(super) fn apply_settlements(
    ts: u64,
    rates: &BTreeMap<InstrumentId, (i64, u64)>,
    account_id: &str,
    currency: &str,
    specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
    marks: &BTreeMap<InstrumentId, ExactDecimal>,
    mark_timestamps: &BTreeMap<InstrumentId, u64>,
    ledger: &mut Ledger,
    log: &mut EventLog,
) -> Result<(), qx_core::QxError> {
    for (instrument, (rate_bps, funding_interval_ms)) in rates {
        let Some(spec) = specs.get(instrument) else {
            return Err(qx_core::QxError::Invariant(format!(
                "资金费结算标的不在规格表中: {instrument}"
            )));
        };
        let mut signed_quantity = ExactDecimal::ZERO;
        for side in [
            qx_core::PositionSide::Net,
            qx_core::PositionSide::Long,
            qx_core::PositionSide::Short,
        ] {
            signed_quantity = signed_quantity
                .checked_add(
                    ledger
                        .position_for_side_exact(account_id, instrument, side)
                        .quantity,
                )
                .ok_or_else(|| qx_core::QxError::Invariant("资金费持仓数量累计溢出".into()))?;
        }
        if signed_quantity.is_zero() {
            continue;
        }
        let mark = marks.get(instrument).copied().ok_or_else(|| {
            qx_core::QxError::Invariant(format!("资金费结算缺少标记价格: {instrument}"))
        })?;
        let mark_ts = mark_timestamps.get(instrument).copied().ok_or_else(|| {
            qx_core::QxError::Invariant(format!("资金费结算缺少标记时间: {instrument}"))
        })?;
        if ts.saturating_sub(mark_ts) > *funding_interval_ms {
            return Err(qx_core::QxError::BusinessViolation(format!(
                "资金费结算标记价已过期: instrument={instrument}, mark_ts={mark_ts}, settlement_ts={ts}"
            )));
        }
        let payment = spec.funding_payment_exact(signed_quantity, mark, *rate_bps)?;
        let cashflow = ExactDecimal::ZERO
            .checked_sub(payment)
            .ok_or_else(|| qx_core::QxError::Invariant("精确资金费现金流取反溢出".into()))?;
        if cashflow.is_zero() {
            continue;
        }
        let contract_size = spec
            .exact_rules
            .as_ref()
            .map(|rules| rules.contract_size)
            .unwrap_or_else(|| {
                ExactDecimal::from_parts(spec.contract_size, 9)
                    .expect("validated contract size fits supported scale")
            });
        let entry_id = ledger.apply_instrument_funding_exact(
            account_id,
            currency,
            ExactInstrumentFunding {
                instrument,
                quantity: signed_quantity,
                mark_price: mark,
                amount: cashflow,
                contract_size,
                inverse: spec.inverse,
                ts,
            },
        )?;
        let entry = ledger
            .entry(entry_id)
            .cloned()
            .ok_or_else(|| qx_core::QxError::Invariant("资金费账簿项未写入".into()))?;
        let seq = log.alloc_seq();
        log.append(Event::new(
            seq,
            ts,
            Priority::TIMER,
            EventKind::LedgerApplied { entry },
        ));
    }
    Ok(())
}

pub(super) fn exact_price(raw: i128) -> Result<ExactDecimal, qx_core::QxError> {
    ExactDecimal::from_parts(raw, 9).map_err(|error| qx_core::QxError::Invariant(error.into()))
}
