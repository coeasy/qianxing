//! Shared-account Bar execution across instruments.
//!
//! Same-timestamp callbacks form deterministic batches; strategies decide from
//! completed bars, then shared OMS, Ledger, budget, and EventLog state advances.

use super::{
    append_strategy_decision_facts,
    fill_application::{self, FillApplicationInput},
    order_admission::{self, OrderAdmissionInput},
    BarStrategy, PortfolioBarSeries, PortfolioBarTimeline, PortfolioBudgetState,
};
use crate::cost::MarginRule;
use qx_core::{
    Event, EventKind, EventLog, FundingRateEvent, InstrumentId, Ledger, Money, Price,
    TradingInstrumentSpec, ValuationContext,
};
use qx_guanxing::Bar;
use qx_zhenlu::Oms;
use std::collections::BTreeMap;

const MAX_INSTRUMENT_EQUITY_SAMPLES: usize = 1_000;

mod construction;
mod funding;
mod margin;
mod model;
mod report;
mod runtime;
use funding::{apply_settlements, prepare_timeline, PreparedTimeline, SettlementContext};
use margin::{
    cached_or_other_market_margin, CachedPortfolioMarginSnapshot, OtherMarketMarginQuery,
};
pub use model::{
    PortfolioBarEngine, PortfolioBarMarket, PortfolioBarSessionConfig, PortfolioBarSessionReport,
};
use runtime::{
    bar_at_or_before, build_runtime, initialize_ledger, invariant, pending_funding_reservation_raw,
    sum_pending_funding_reservations,
};
struct OwnedTimelineEvent {
    market_index: usize,
    bar_index: usize,
    ts: u64,
}

impl<'a> PortfolioBarEngine<'a> {
    pub fn run(
        self,
        strategy: &mut dyn BarStrategy,
    ) -> Result<PortfolioBarSessionReport, qx_core::QxError> {
        self.run_with_cancel(strategy, || false)?
            .ok_or_else(|| qx_core::QxError::Invariant("共享 Bar session 意外取消".into()))
    }

    pub fn run_with_cancel<F>(
        self,
        strategy: &mut dyn BarStrategy,
        mut is_cancelled: F,
    ) -> Result<Option<PortfolioBarSessionReport>, qx_core::QxError>
    where
        F: FnMut() -> bool,
    {
        let Self { config, markets } = self;
        let specs = markets
            .iter()
            .map(|market| (market.instrument.clone(), market.spec.clone()))
            .collect::<BTreeMap<_, _>>();
        let initial_cash_raw = config.initial_cash.raw();
        let PreparedTimeline {
            start,
            end,
            events,
            settlements,
        } = prepare_timeline(
            &markets,
            &config.funding_bps,
            &config.funding_events,
            &specs,
        )?;
        let mut runtime = build_runtime(markets);
        let (mut ledger, mut log) = initialize_ledger(
            &config.account_id,
            &config.currency,
            config.initial_cash,
            start,
            settlements.contains_key(&start),
        )?;
        let mut oms = Oms::new();
        let mut budget = PortfolioBudgetState::default();
        let mut next_id = 1_u64;
        let mut processed = vec![0_usize; runtime.by_index.len()];
        let mut pending_funding_reservations = Some(0_u128);
        let mut marks = BTreeMap::<InstrumentId, Price>::new();
        let mut mark_timestamps = BTreeMap::<InstrumentId, u64>::new();
        let mut fills = Vec::new();
        let mut fills_by_instrument = runtime
            .by_index
            .iter()
            .map(|market| (market.instrument.clone(), Vec::new()))
            .collect::<BTreeMap<_, _>>();
        let mut rejections_by_instrument = runtime
            .by_index
            .iter()
            .map(|market| (market.instrument.clone(), EventLog::new()))
            .collect::<BTreeMap<_, _>>();
        let mut equity = Vec::new();
        let mut instrument_equity_contributions = specs
            .keys()
            .cloned()
            .map(|instrument| (instrument, Vec::new()))
            .collect::<BTreeMap<_, _>>();
        let mut instrument_cash_by_currency =
            BTreeMap::<InstrumentId, BTreeMap<String, i128>>::new();
        let mut instrument_cash_entry_cursor = 0_usize;
        let mut funding_by_instrument_raw = BTreeMap::<InstrumentId, i128>::new();
        let mut funding_events_by_instrument = BTreeMap::<InstrumentId, Vec<(u64, i128)>>::new();

        let mut event_cursor = 0;
        let mut settlement_times = settlements.keys().copied().peekable();
        let mut pending_by_market = Vec::<(usize, u64, Vec<qx_core::Order>)>::new();
        let mut pending_batch_count;
        let mut aligned_batch_by_market = vec![usize::MAX; runtime.by_index.len()];
        let mut touched_batch_markets = Vec::new();
        loop {
            let event_ts = events.get(event_cursor).map(|event| event.ts);
            let settlement_ts = settlement_times.peek().copied();
            let ts = match (event_ts, settlement_ts) {
                (None, None) => break,
                (Some(event_ts), Some(settlement_ts)) => event_ts.min(settlement_ts),
                (Some(event_ts), None) => event_ts,
                (None, Some(settlement_ts)) => settlement_ts,
            };
            if settlement_ts == Some(ts) {
                settlement_times.next();
            }
            if is_cancelled() {
                return Ok(None);
            }
            let batch_start = event_cursor;
            while event_cursor < events.len() && events[event_cursor].ts == ts {
                event_cursor += 1;
            }
            let timestamp_events = &events[batch_start..event_cursor];
            if let Some(scheduled) = settlements.get(&ts) {
                apply_settlements(SettlementContext {
                    ts,
                    settlements: scheduled,
                    account_id: &config.account_id,
                    currency: &config.currency,
                    specs: &specs,
                    marks: &marks,
                    mark_timestamps: &mark_timestamps,
                    ledger: &mut ledger,
                    log: &mut log,
                    totals: &mut funding_by_instrument_raw,
                    cashflows: &mut funding_events_by_instrument,
                })?;
            }
            if timestamp_events.is_empty() {
                let marked_equity = ledger
                    .valuate(
                        &ValuationContext::spot(&config.account_id, &marks, &config.currency)
                            .with_market_specs(&specs),
                    )?
                    .equity
                    .raw();
                append_instrument_equity_contributions(
                    &mut instrument_equity_contributions,
                    &mut instrument_cash_by_currency,
                    &mut instrument_cash_entry_cursor,
                    InstrumentContributionInput {
                        equity_index: u64::try_from(equity.len())
                            .map_err(|_| invariant("权益曲线点数超出 u64"))?,
                        ts,
                        ledger: &ledger,
                        account_id: &config.account_id,
                        reporting_currency: &config.currency,
                        specs: &specs,
                        marks: &marks,
                        initial_cash_raw,
                        marked_equity_raw: marked_equity,
                    },
                )?;
                equity.push((ts, marked_equity));
                continue;
            }
            // Aligned callbacks all share one visible timestamp, so the common case can address
            // a batch directly by market index. Crossed/unaligned visibility retains the ordered
            // fallback because one market may then need multiple batches.
            let mut common_visible_ts = None;
            let mut aligned_visible_ts = true;
            for event in timestamp_events {
                let previous_count = processed[event.market_index];
                if previous_count == 0 {
                    continue;
                }
                let visible_ts = runtime.by_index[event.market_index].bars[previous_count - 1].ts;
                if common_visible_ts.is_some_and(|common| common != visible_ts) {
                    aligned_visible_ts = false;
                    break;
                }
                common_visible_ts = Some(visible_ts);
            }
            pending_batch_count = 0;
            touched_batch_markets.clear();
            let mut unaligned_pending = BTreeMap::<(usize, u64), Vec<qx_core::Order>>::new();
            for event in timestamp_events {
                let market = runtime
                    .by_index
                    .get(event.market_index)
                    .ok_or_else(|| invariant("timeline market missing"))?;
                let instrument = &market.instrument;
                let bar = market
                    .bars
                    .get(event.bar_index)
                    .ok_or_else(|| invariant("timeline Bar index missing"))?;
                let previous_count = processed[event.market_index];
                if previous_count > 0 {
                    let history = &market.bars[..previous_count];
                    let visible_ts = history
                        .last()
                        .ok_or_else(|| invariant("processed Bar history missing"))?
                        .ts;
                    let cash = ledger.cash_balances_for_ref(&config.account_id);
                    let reserved =
                        pending_funding_reservation_raw(&runtime, pending_funding_reservations);
                    let free_cash = ledger
                        .cash_for(&config.account_id, &config.currency)
                        .saturating_sub(reserved)
                        .max(0);
                    strategy.update_account_state(cash, Some(free_cash));
                    let position = ledger
                        .position_for(&config.account_id, instrument)
                        .quantity
                        .raw();
                    let orders = strategy.on_bar_orders_with_margin_checked(
                        history,
                        instrument,
                        bar.ts,
                        position,
                        Some(free_cash),
                    )?;
                    append_strategy_decision_facts(
                        &mut log,
                        bar.ts,
                        &config.account_id,
                        strategy.drain_decision_facts(),
                    )?;
                    for order in orders {
                        let Some(&order_market_index) =
                            runtime.index_by_instrument.get(&order.instrument)
                        else {
                            return Err(qx_core::QxError::BusinessViolation(format!(
                                "策略订单标的 {} 不属于共享 Bar session",
                                order.instrument
                            )));
                        };
                        if aligned_visible_ts {
                            let batch_index = aligned_batch_by_market[order_market_index];
                            let batch_index = if batch_index == usize::MAX {
                                if pending_batch_count == pending_by_market.len() {
                                    pending_by_market.push((
                                        order_market_index,
                                        visible_ts,
                                        Vec::new(),
                                    ));
                                }
                                let batch = &mut pending_by_market[pending_batch_count];
                                batch.0 = order_market_index;
                                batch.1 = visible_ts;
                                batch.2.clear();
                                aligned_batch_by_market[order_market_index] = pending_batch_count;
                                touched_batch_markets.push(order_market_index);
                                pending_batch_count += 1;
                                pending_batch_count - 1
                            } else {
                                batch_index
                            };
                            pending_by_market[batch_index].2.push(order);
                        } else {
                            unaligned_pending
                                .entry((order_market_index, visible_ts))
                                .or_default()
                                .push(order);
                        }
                    }
                }
            }

            if !aligned_visible_ts {
                for ((market_index, visible_ts), orders) in unaligned_pending {
                    if pending_batch_count == pending_by_market.len() {
                        pending_by_market.push((market_index, visible_ts, orders));
                    } else {
                        let batch = &mut pending_by_market[pending_batch_count];
                        batch.0 = market_index;
                        batch.1 = visible_ts;
                        batch.2 = orders;
                    }
                    pending_batch_count += 1;
                }
            }
            for market_index in touched_batch_markets.drain(..) {
                aligned_batch_by_market[market_index] = usize::MAX;
            }

            pending_by_market[..pending_batch_count].sort_by(|left, right| {
                let left_market = &runtime.by_index[left.0];
                let right_market = &runtime.by_index[right.0];
                left_market
                    .priority
                    .cmp(&right_market.priority)
                    .then_with(|| left_market.instrument.cmp(&right_market.instrument))
                    .then_with(|| left.1.cmp(&right.1))
            });
            let mut admitted = Vec::new();
            let mut batch_reserved_raw = 0_i128;
            let mut visible_marks_cache: Option<(u64, BTreeMap<InstrumentId, Price>)> = None;
            let mut visible_margin_cache: Option<CachedPortfolioMarginSnapshot> = None;
            for (market_index, visible_ts, orders) in
                pending_by_market.iter_mut().take(pending_batch_count)
            {
                let market = &runtime.by_index[*market_index];
                let instrument = &market.instrument;
                let visible_close = bar_at_or_before(market.bars, *visible_ts)
                    .map(|bar| bar.close)
                    .ok_or_else(|| {
                        qx_core::QxError::BusinessViolation(format!(
                            "策略在 {} 尚无可见行情时下单",
                            instrument
                        ))
                    })?;
                if visible_marks_cache
                    .as_ref()
                    .map(|(cached_ts, _)| *cached_ts)
                    != Some(*visible_ts)
                {
                    let visible_marks = runtime
                        .by_index
                        .iter()
                        .filter_map(|candidate| {
                            bar_at_or_before(candidate.bars, *visible_ts).map(|bar| {
                                (candidate.instrument.clone(), Price::from_raw(bar.close))
                            })
                        })
                        .collect::<BTreeMap<_, _>>();
                    visible_marks_cache = Some((*visible_ts, visible_marks));
                }
                let visible_marks = &visible_marks_cache
                    .as_ref()
                    .ok_or_else(|| invariant("visible marks cache missing"))?
                    .1;
                let own_pending = market.matcher.pending_funding_reservation_sum_raw_exact();
                let other_pending = match (pending_funding_reservations, own_pending) {
                    (Some(total), Some(own)) => {
                        total.saturating_sub(own).min(i128::MAX as u128) as i128
                    }
                    _ => runtime
                        .by_index
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| *index != *market_index)
                        .fold(0_i128, |total, (_, candidate)| {
                            total
                                .saturating_add(candidate.matcher.pending_funding_reservation_raw())
                        }),
                }
                .saturating_add(batch_reserved_raw);
                let other_margin = cached_or_other_market_margin(
                    &mut visible_margin_cache,
                    OtherMarketMarginQuery {
                        visible_ts: *visible_ts,
                        ledger: &ledger,
                        account_id: &config.account_id,
                        target: instrument,
                        specs: &specs,
                        marks: visible_marks,
                        markets: &runtime,
                    },
                )?;
                let derivative_spec = market
                    .spec
                    .product
                    .supports_leverage()
                    .then_some(&market.spec);
                let position = ledger
                    .position_for(&config.account_id, instrument)
                    .quantity
                    .raw();
                let previous_log_len = log.events().len();
                let accepted = order_admission::admit_orders(OrderAdmissionInput {
                    orders,
                    account_id: &config.account_id,
                    instrument,
                    currency: &config.currency,
                    ts,
                    position,
                    visible_close,
                    instrument_spec: Some(&market.spec),
                    derivative_spec,
                    multiplier: 1,
                    strategy,
                    ledger: &ledger,
                    oms: &mut oms,
                    portfolio_budget: &mut budget,
                    margin: market.margin.as_ref(),
                    matcher: &market.matcher,
                    other_pending_funding_raw: other_pending,
                    other_market_margin_raw: other_margin,
                    portfolio_marks: Some(visible_marks),
                    portfolio_specs: Some(&specs),
                    risk: &config.risk,
                    fx_rates: &BTreeMap::new(),
                    ashare_rules: None,
                    ashare_bought_today: 0,
                    next_id: &mut next_id,
                    log: &mut log,
                })?;
                let projection = rejections_by_instrument
                    .get_mut(instrument)
                    .ok_or_else(|| invariant("rejection projection missing"))?;
                for event in log.events()[previous_log_len..].iter() {
                    if matches!(event.kind, EventKind::Rejected { .. }) {
                        let seq = projection.alloc_seq();
                        projection.append(Event::new(
                            seq,
                            event.ts,
                            event.prio,
                            event.kind.clone(),
                        ));
                    }
                }
                for order in accepted {
                    batch_reserved_raw =
                        batch_reserved_raw.saturating_add(order.funding_reservation_raw);
                    admitted.push((*market_index, order));
                }
            }
            for (market_index, accepted) in admitted {
                let accepted_seq = log.alloc_seq();
                log.append(Event::new(
                    accepted_seq,
                    ts,
                    qx_core::Priority::MATCH,
                    EventKind::Accepted {
                        client_order_id: accepted.order.client_id,
                        venue_order_id: None,
                    },
                ));
                pending_funding_reservations = pending_funding_reservations.and_then(|total| {
                    total.checked_add(accepted.funding_reservation_raw.max(0) as u128)
                });
                runtime.by_index[market_index]
                    .matcher
                    .submit_with_funding_reservation_at(
                        accepted.order,
                        ts,
                        accepted.funding_reservation_raw,
                    );
            }
            if pending_funding_reservations.is_none() {
                pending_funding_reservations = sum_pending_funding_reservations(&runtime);
            }

            for event in timestamp_events {
                let market = runtime
                    .by_index
                    .get_mut(event.market_index)
                    .ok_or_else(|| invariant("timeline market missing"))?;
                let before_pending = market.matcher.pending_funding_reservation_sum_raw_exact();
                let bar = market
                    .bars
                    .get(event.bar_index)
                    .ok_or_else(|| invariant("timeline Bar index missing"))?;
                let market_fills = market.matcher.on_bar(bar, ts);
                let after_pending = market.matcher.pending_funding_reservation_sum_raw_exact();
                pending_funding_reservations =
                    match (pending_funding_reservations, before_pending, after_pending) {
                        (Some(total), Some(before), Some(after)) => total
                            .checked_sub(before)
                            .and_then(|value| value.checked_add(after)),
                        _ => None,
                    };
                for fill in market_fills {
                    let (_, stored) = fill_application::apply(FillApplicationInput {
                        fill,
                        instrument_spec: Some(&market.spec),
                        cash_account: !market.spec.product.supports_leverage(),
                        multiplier: 1,
                        strategy,
                        account_id: &config.account_id,
                        currency: &config.currency,
                        ledger: &mut ledger,
                        oms: &mut oms,
                        portfolio_budget: &mut budget,
                        margin: market.margin.as_ref(),
                        log: &mut log,
                        fills: &mut fills,
                    })?;
                    let fills_for_market = fills_by_instrument
                        .get_mut(&market.instrument)
                        .ok_or_else(|| invariant("fill projection missing"))?;
                    fills_for_market.push(stored);
                }
                processed[event.market_index] += 1;
                let mark = Price::from_raw(bar.close);
                if let Some(current) = marks.get_mut(&market.instrument) {
                    *current = mark;
                } else {
                    marks.insert(market.instrument.clone(), mark);
                }
                if let Some(current) = mark_timestamps.get_mut(&market.instrument) {
                    *current = bar.ts;
                } else {
                    mark_timestamps.insert(market.instrument.clone(), bar.ts);
                }
                let marked_equity = ledger
                    .valuate(
                        &ValuationContext::spot(&config.account_id, &marks, &config.currency)
                            .with_market_specs(&specs),
                    )?
                    .equity
                    .raw();
                append_instrument_equity_contributions(
                    &mut instrument_equity_contributions,
                    &mut instrument_cash_by_currency,
                    &mut instrument_cash_entry_cursor,
                    InstrumentContributionInput {
                        equity_index: u64::try_from(equity.len())
                            .map_err(|_| invariant("权益曲线点数超出 u64"))?,
                        ts,
                        ledger: &ledger,
                        account_id: &config.account_id,
                        reporting_currency: &config.currency,
                        specs: &specs,
                        marks: &marks,
                        initial_cash_raw,
                        marked_equity_raw: marked_equity,
                    },
                )?;
                equity.push((ts, marked_equity));
                if pending_funding_reservations.is_none() {
                    pending_funding_reservations = sum_pending_funding_reservations(&runtime);
                }
            }
        }
        log.validate()?;
        let report = PortfolioBarSessionReport {
            fills,
            fills_by_instrument,
            rejections_by_instrument,
            equity,
            instrument_equity_contributions,
            initial_cash_raw,
            ledger,
            event_log: log,
            market_specs: specs,
            funding_by_instrument_raw,
            funding_events_by_instrument,
            clock_start: start,
            clock_end: end,
        };
        report.verify()?;
        Ok(Some(report))
    }
}

fn append_instrument_equity_contributions(
    curves: &mut BTreeMap<InstrumentId, Vec<(u64, u64, i128)>>,
    cash_by_instrument: &mut BTreeMap<InstrumentId, BTreeMap<String, i128>>,
    ledger_entry_cursor: &mut usize,
    input: InstrumentContributionInput<'_>,
) -> Result<(), qx_core::QxError> {
    for entry in input
        .ledger
        .entries()
        .get(*ledger_entry_cursor..)
        .ok_or_else(|| invariant("多市场归因账簿游标超出范围"))?
    {
        if entry.account_id != input.account_id {
            continue;
        }
        let Some(instrument) = entry.instrument.as_ref() else {
            continue;
        };
        let amount = cash_by_instrument
            .entry(instrument.clone())
            .or_default()
            .entry(entry.currency.clone())
            .or_default();
        *amount = amount
            .checked_add(entry.amount.raw())
            .ok_or_else(|| invariant("多市场标的现金归因累计溢出"))?;
    }
    *ledger_entry_cursor = input.ledger.entries().len();

    let mut reconstructed_equity = input.initial_cash_raw;
    let empty_cash_projection = BTreeMap::new();
    for (instrument, spec) in input.specs {
        let valuation = qx_core::ValuationContext::spot(
            input.account_id,
            input.marks,
            input.reporting_currency,
        )
        .with_spec(Some(spec));
        let contribution = input.ledger.instrument_equity_contribution_from_cash(
            &valuation,
            cash_by_instrument
                .get(instrument)
                .unwrap_or(&empty_cash_projection),
        )?;
        reconstructed_equity = reconstructed_equity
            .checked_add(contribution)
            .ok_or_else(|| invariant("多市场标的归因权益溢出"))?;
        curves
            .get_mut(instrument)
            .ok_or_else(|| invariant("多市场标的归因曲线缺失"))?
            .push((input.equity_index, input.ts, contribution));
    }
    if reconstructed_equity != input.marked_equity_raw {
        return Err(invariant("多市场逐标的权益贡献之和与共享账户权益不一致"));
    }
    if curves
        .values()
        .next()
        .is_some_and(|curve| curve.len() > MAX_INSTRUMENT_EQUITY_SAMPLES)
    {
        for curve in curves.values_mut() {
            let last = curve.len().saturating_sub(1);
            let mut compacted = Vec::with_capacity(curve.len().div_ceil(2) + 1);
            for (index, point) in curve.drain(..).enumerate() {
                if index % 2 == 0 || index == last {
                    compacted.push(point);
                }
            }
            *curve = compacted;
        }
    }
    Ok(())
}

struct InstrumentContributionInput<'a> {
    equity_index: u64,
    ts: u64,
    ledger: &'a Ledger,
    account_id: &'a str,
    reporting_currency: &'a str,
    specs: &'a BTreeMap<InstrumentId, TradingInstrumentSpec>,
    marks: &'a BTreeMap<InstrumentId, Price>,
    initial_cash_raw: i128,
    marked_equity_raw: i128,
}
