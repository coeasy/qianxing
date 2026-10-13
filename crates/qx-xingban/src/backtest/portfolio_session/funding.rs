use super::*;

use qx_core::FUNDING_INTERVAL_MS;

const MAX_FUNDING_SETTLEMENTS: u64 = 100_000;

pub(super) struct PreparedTimeline {
    pub start: u64,
    pub end: u64,
    pub events: Vec<OwnedTimelineEvent>,
    pub settlements: BTreeMap<u64, BTreeMap<InstrumentId, i64>>,
    pub timestamps: Vec<u64>,
}

pub(super) fn prepare_timeline(
    markets: &[PortfolioBarMarket<'_>],
    rates: &BTreeMap<InstrumentId, i64>,
    funding_events: &[FundingRateEvent],
    specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
) -> Result<PreparedTimeline, qx_core::QxError> {
    let series = markets
        .iter()
        .map(|market| PortfolioBarSeries {
            instrument: &market.instrument,
            priority: market.priority,
            bars: market.bars,
        })
        .collect::<Vec<_>>();
    let timeline =
        PortfolioBarTimeline::new(&series).map_err(qx_core::QxError::BusinessViolation)?;
    if timeline.events().is_empty() {
        return Err(qx_core::QxError::BusinessViolation(
            "共享 Bar session 至少需要一条行情".into(),
        ));
    }
    let start = timeline.clock_start().unwrap_or(0);
    let end = timeline.clock_end().unwrap_or(0);
    let events = timeline
        .events()
        .iter()
        .map(|event| OwnedTimelineEvent {
            instrument: event.instrument.clone(),
            bar: *event.bar,
        })
        .collect::<Vec<_>>();
    let settlements = scheduled_settlements(start, end, rates, funding_events, specs)?;
    let timestamps = events
        .iter()
        .map(|event| event.bar.ts)
        .chain(settlements.keys().copied())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    Ok(PreparedTimeline {
        start,
        end,
        events,
        settlements,
        timestamps,
    })
}

pub(super) fn scheduled_settlements(
    start: u64,
    end: u64,
    rates: &BTreeMap<InstrumentId, i64>,
    funding_events: &[FundingRateEvent],
    specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
) -> Result<BTreeMap<u64, BTreeMap<InstrumentId, i64>>, qx_core::QxError> {
    if funding_events.len() > MAX_FUNDING_SETTLEMENTS as usize {
        return Err(qx_core::QxError::BusinessViolation(format!(
            "历史资金费事件数 {} 超过上限 {MAX_FUNDING_SETTLEMENTS}",
            funding_events.len()
        )));
    }
    for (instrument, rate) in rates {
        let Some(spec) = specs.get(instrument) else {
            return Err(qx_core::QxError::BusinessViolation(format!(
                "资金费标的 {instrument} 不属于共享 session"
            )));
        };
        if !(-10_000..=10_000).contains(rate) || (*rate != 0 && !spec.product.is_derivative()) {
            return Err(qx_core::QxError::BusinessViolation(format!(
                "资金费配置无效: instrument={instrument}, rate_bps={rate}"
            )));
        }
    }
    let mut event_instruments = std::collections::BTreeSet::new();
    let mut event_keys = std::collections::BTreeSet::new();
    for event in funding_events {
        let Some(spec) = specs.get(&event.instrument) else {
            return Err(qx_core::QxError::BusinessViolation(format!(
                "历史资金费标的 {} 不属于共享 session",
                event.instrument
            )));
        };
        if !(-10_000..=10_000).contains(&event.rate_bps)
            || (event.rate_bps != 0 && !spec.product.is_derivative())
            || event.settlement_ts % FUNDING_INTERVAL_MS != 0
        {
            return Err(qx_core::QxError::BusinessViolation(format!(
                "历史资金费事件无效: instrument={}, settlement_ts={}, rate_bps={}",
                event.instrument, event.settlement_ts, event.rate_bps
            )));
        }
        if !event_keys.insert((event.instrument.clone(), event.settlement_ts)) {
            return Err(qx_core::QxError::BusinessViolation(format!(
                "历史资金费事件重复: instrument={}, settlement_ts={}",
                event.instrument, event.settlement_ts
            )));
        }
        event_instruments.insert(event.instrument.clone());
    }
    if rates
        .iter()
        .any(|(instrument, rate)| *rate != 0 && event_instruments.contains(instrument))
    {
        return Err(qx_core::QxError::BusinessViolation(
            "同一标的不能混用常量资金费率与历史资金费事件".into(),
        ));
    }
    let mut events = BTreeMap::<u64, BTreeMap<InstrumentId, i64>>::new();
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
                events.insert(
                    ts,
                    rates
                        .iter()
                        .filter(|(_, rate)| **rate != 0)
                        .map(|(instrument, rate)| (instrument.clone(), *rate))
                        .collect(),
                );
                let Some(next) = ts.checked_add(FUNDING_INTERVAL_MS) else {
                    break;
                };
                ts = next;
            }
        }
    }
    for event in funding_events
        .iter()
        .filter(|event| event.settlement_ts > start && event.settlement_ts <= end)
    {
        let settlements = events.entry(event.settlement_ts).or_default();
        if settlements.contains_key(&event.instrument) {
            return Err(qx_core::QxError::BusinessViolation(format!(
                "资金费事件与常量资金费率在同一结算点冲突: instrument={}, settlement_ts={}",
                event.instrument, event.settlement_ts
            )));
        }
        settlements.insert(event.instrument.clone(), event.rate_bps);
    }
    if events.len() as u64 > MAX_FUNDING_SETTLEMENTS {
        return Err(qx_core::QxError::BusinessViolation(format!(
            "资金费结算次数 {} 超过上限 {MAX_FUNDING_SETTLEMENTS}",
            events.len()
        )));
    }
    Ok(events)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_settlements(
    ts: u64,
    settlements: &BTreeMap<InstrumentId, i64>,
    account_id: &str,
    currency: &str,
    specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
    marks: &BTreeMap<InstrumentId, Price>,
    mark_timestamps: &BTreeMap<InstrumentId, u64>,
    ledger: &mut Ledger,
    log: &mut EventLog,
    totals: &mut BTreeMap<InstrumentId, i128>,
    cashflows: &mut BTreeMap<InstrumentId, Vec<(u64, i128)>>,
) -> Result<(), qx_core::QxError> {
    for (instrument, rate_bps) in settlements {
        let position = ledger.position_for(account_id, instrument).quantity.raw();
        if position == 0 {
            continue;
        }
        let Some(mark) = marks.get(instrument) else {
            return Err(qx_core::QxError::Invariant(format!(
                "资金费结算缺少已观察到的标记价格: {instrument}"
            )));
        };
        let mark_ts = mark_timestamps
            .get(instrument)
            .copied()
            .ok_or_else(|| qx_core::QxError::Invariant("资金费结算缺少标记时间".into()))?;
        if ts.saturating_sub(mark_ts) > FUNDING_INTERVAL_MS {
            return Err(qx_core::QxError::BusinessViolation(format!(
                "资金费结算标记价已过期: instrument={instrument}, mark_ts={mark_ts}, settlement_ts={ts}"
            )));
        }
        let spec = specs
            .get(instrument)
            .ok_or_else(|| qx_core::QxError::Invariant("资金费结算标的不在规格表中".into()))?;
        let amount = spec
            .funding_payment(position, mark.raw(), *rate_bps)?
            .checked_neg()
            .ok_or_else(|| qx_core::QxError::Invariant("资金费金额取反溢出".into()))?;
        if amount == 0 {
            continue;
        }
        let entry_id = ledger.apply_instrument_funding(
            account_id,
            currency,
            qx_core::InstrumentFunding {
                instrument,
                quantity: qx_core::Quantity::from_raw(position),
                mark_price: *mark,
                amount: Money::from_raw(amount),
                ts,
            },
        )?;
        let entry = ledger
            .entries()
            .iter()
            .find(|entry| entry.id == entry_id)
            .cloned()
            .ok_or_else(|| qx_core::QxError::Invariant("资金费账簿项未写入".into()))?;
        let seq = log.alloc_seq();
        log.append(Event::new(
            seq,
            ts,
            // Settlement is an input known before callbacks/commands at this timestamp.
            qx_core::Priority::TIMER,
            EventKind::LedgerApplied { entry },
        ));
        let total = totals.entry(instrument.clone()).or_default();
        *total = total
            .checked_add(amount)
            .ok_or_else(|| qx_core::QxError::Invariant("标的资金费累计溢出".into()))?;
        cashflows
            .entry(instrument.clone())
            .or_default()
            .push((ts, amount));
    }
    Ok(())
}
