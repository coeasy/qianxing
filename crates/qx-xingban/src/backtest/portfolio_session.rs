//! Shared-account Bar execution across instruments.
//!
//! All market callbacks at one timestamp form a deterministic batch: strategies
//! decide from completed bars, orders pass the common admission path, then each
//! market matcher runs in stable priority/instrument order and all fills update
//! one OMS, Ledger, budget state, and EventLog.

use super::{
    fill_application::{self, FillApplicationInput},
    order_admission::{self, OrderAdmissionInput},
    BarStrategy, PortfolioBarSeries, PortfolioBarTimeline, PortfolioBudgetState,
};
use crate::{
    cost::{LatencyModel, MarginRule},
    fill::{DataTier, FillModel},
};
use qx_core::{
    Event, EventKind, EventLog, FeeModel, Fill, FundingRateEvent, InstrumentId, Ledger, Money,
    Price, ReplayVerifier, TradingInstrumentSpec, ValuationContext,
};
use qx_guanxing::{Bar, QualityGate, Verdict};
use qx_zhenlu::{Oms, RiskGate};
use std::collections::BTreeMap;

mod funding;
mod margin;
mod runtime;
use funding::{apply_settlements, prepare_timeline, PreparedTimeline};
use margin::other_market_margin;
use runtime::{build_runtime, initialize_ledger};

pub struct PortfolioBarMarket<'a> {
    pub instrument: InstrumentId,
    pub priority: u32,
    pub bars: &'a [Bar],
    pub spec: TradingInstrumentSpec,
    pub fill: Box<dyn FillModel>,
    pub fee: Box<dyn FeeModel>,
    pub latency: Box<dyn LatencyModel>,
    pub margin: Box<dyn MarginRule>,
    pub seed: u64,
}

pub struct PortfolioBarSessionConfig {
    pub account_id: String,
    pub currency: String,
    pub initial_cash: Money,
    pub risk: RiskGate,
    /// Constant funding rate in basis points per UTC 8-hour settlement, keyed by derivative.
    pub funding_bps: BTreeMap<InstrumentId, i64>,
    /// Optional variable historical funding rates at explicit UTC 8-hour settlements.
    pub funding_events: Vec<FundingRateEvent>,
}

pub struct PortfolioBarSessionReport {
    pub fills: Vec<Fill>,
    pub fills_by_instrument: BTreeMap<InstrumentId, Vec<Fill>>,
    pub rejections_by_instrument: BTreeMap<InstrumentId, EventLog>,
    pub equity: Vec<(u64, i128)>,
    pub ledger: Ledger,
    pub event_log: EventLog,
    pub market_specs: BTreeMap<InstrumentId, TradingInstrumentSpec>,
    pub funding_by_instrument_raw: BTreeMap<InstrumentId, i128>,
    pub funding_events_by_instrument: BTreeMap<InstrumentId, Vec<(u64, i128)>>,
    pub clock_start: u64,
    pub clock_end: u64,
}

impl PortfolioBarSessionReport {
    pub fn result_hash(&self) -> u64 {
        self.event_log.digest()
    }

    pub fn verify(&self) -> Result<(), qx_core::QxError> {
        self.event_log.validate()?;
        ReplayVerifier::verify(self.event_log.events(), &self.ledger)?;
        Ok(())
    }
}

pub struct PortfolioBarEngine<'a> {
    config: PortfolioBarSessionConfig,
    markets: Vec<PortfolioBarMarket<'a>>,
}

struct OwnedTimelineEvent {
    instrument: InstrumentId,
    bar: Bar,
}

impl<'a> PortfolioBarEngine<'a> {
    pub fn new(
        config: PortfolioBarSessionConfig,
        markets: Vec<PortfolioBarMarket<'a>>,
    ) -> Result<Self, qx_core::QxError> {
        if markets.len() < 2 {
            return Err(qx_core::QxError::BusinessViolation(
                "共享 Bar session 至少需要两个市场".into(),
            ));
        }
        if config.account_id.trim().is_empty() || config.initial_cash.raw() <= 0 {
            return Err(qx_core::QxError::BusinessViolation(
                "共享 Bar session 账户身份和初始资金必须有效".into(),
            ));
        }
        let currency = config.currency.trim().to_ascii_uppercase();
        if currency.is_empty() {
            return Err(qx_core::QxError::BusinessViolation(
                "共享 Bar session 结算币种不能为空".into(),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for market in &markets {
            if !seen.insert(market.instrument.clone()) {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "共享 Bar session 存在重复标的 {}",
                    market.instrument
                )));
            }
            if market.spec.instrument != market.instrument {
                return Err(qx_core::QxError::BusinessViolation(
                    "共享 Bar session 的 market spec 与标的不一致".into(),
                ));
            }
            market.spec.validate()?;
            market.fee.validate()?;
            if market.spec.product == qx_core::TradingProduct::Option {
                return Err(qx_core::QxError::BusinessViolation(
                    "共享 Bar session 不支持期权：期权合约身份与估值语义尚未建模".into(),
                ));
            }
            super::currency::normalize_and_validate(
                currency.clone(),
                &market.instrument,
                Some(&market.spec),
            )?;
            if !DataTier::Bar.supports(market.fill.tier()) {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "市场 {} 的撮合模型需要 {:?} 数据，当前 session 仅接收 Bar",
                    market.instrument,
                    market.fill.tier()
                )));
            }
            let quality = QualityGate::check(market.bars);
            if matches!(quality.verdict(), Verdict::Fail | Verdict::Quarantine) {
                return Err(qx_core::QxError::Permanent(format!(
                    "共享 Bar session 输入未通过质量门 {}: {:?}",
                    market.instrument, quality.issues
                )));
            }
        }
        let mut config = config;
        config.currency = currency;
        Ok(Self { config, markets })
    }

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
        let PreparedTimeline {
            start,
            end,
            events,
            settlements,
            timestamps,
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
        )?;
        let mut oms = Oms::new();
        let mut budget = PortfolioBudgetState::default();
        let mut next_id = 1_u64;
        let mut processed = BTreeMap::<InstrumentId, usize>::new();
        let mut marks = BTreeMap::<InstrumentId, Price>::new();
        let mut mark_timestamps = BTreeMap::<InstrumentId, u64>::new();
        let mut fills = Vec::new();
        let mut fills_by_instrument = runtime
            .keys()
            .cloned()
            .map(|instrument| (instrument, Vec::new()))
            .collect::<BTreeMap<_, _>>();
        let mut rejections_by_instrument = runtime
            .keys()
            .cloned()
            .map(|instrument| (instrument, EventLog::new()))
            .collect::<BTreeMap<_, _>>();
        let mut equity = Vec::new();
        let mut funding_by_instrument_raw = BTreeMap::<InstrumentId, i128>::new();
        let mut funding_events_by_instrument = BTreeMap::<InstrumentId, Vec<(u64, i128)>>::new();

        let mut event_cursor = 0;
        for ts in timestamps {
            if is_cancelled() {
                return Ok(None);
            }
            let batch_start = event_cursor;
            while event_cursor < events.len() && events[event_cursor].bar.ts == ts {
                event_cursor += 1;
            }
            let timestamp_events = &events[batch_start..event_cursor];
            if let Some(scheduled) = settlements.get(&ts) {
                apply_settlements(
                    ts,
                    scheduled,
                    &config.account_id,
                    &config.currency,
                    &specs,
                    &marks,
                    &mark_timestamps,
                    &mut ledger,
                    &mut log,
                    &mut funding_by_instrument_raw,
                    &mut funding_events_by_instrument,
                )?;
            }
            if timestamp_events.is_empty() {
                let marked_equity = ledger
                    .valuate(
                        &ValuationContext::spot(&config.account_id, &marks, &config.currency)
                            .with_market_specs(&specs),
                    )?
                    .equity
                    .raw();
                equity.push((ts, marked_equity));
                continue;
            }
            let mut pending_by_market = BTreeMap::<(InstrumentId, u64), Vec<qx_core::Order>>::new();
            for event in timestamp_events {
                let instrument = event.instrument.clone();
                let market = runtime.get(&instrument).ok_or_else(|| {
                    qx_core::QxError::Invariant("时间线标的不在 session 配置中".into())
                })?;
                let previous_count = processed.get(&instrument).copied().unwrap_or_default();
                if previous_count > 0 {
                    let history = &market.bars[..previous_count];
                    let visible_ts = history.last().expect("previous_count is positive").ts;
                    let cash = ledger.cash_balances_for(&config.account_id);
                    let reserved = runtime.values().fold(0_i128, |total, market| {
                        total.saturating_add(market.matcher.pending_funding_reservation_raw())
                    });
                    let free_cash = ledger
                        .cash_for(&config.account_id, &config.currency)
                        .saturating_sub(reserved)
                        .max(0);
                    strategy.update_account_state(&cash, Some(free_cash));
                    let position = ledger
                        .position_for(&config.account_id, &instrument)
                        .quantity
                        .raw();
                    let orders = strategy.on_bar_orders_with_margin_checked(
                        history,
                        &instrument,
                        event.bar.ts,
                        position,
                        Some(free_cash),
                    )?;
                    for order in orders {
                        if !runtime.contains_key(&order.instrument) {
                            return Err(qx_core::QxError::BusinessViolation(format!(
                                "策略订单标的 {} 不属于共享 Bar session",
                                order.instrument
                            )));
                        }
                        pending_by_market
                            .entry((order.instrument.clone(), visible_ts))
                            .or_default()
                            .push(order);
                    }
                }
            }

            let mut admission_batches = pending_by_market.into_iter().collect::<Vec<_>>();
            admission_batches.sort_by(|left, right| {
                let left_priority = runtime[&left.0 .0].priority;
                let right_priority = runtime[&right.0 .0].priority;
                left_priority
                    .cmp(&right_priority)
                    .then_with(|| left.0 .0.cmp(&right.0 .0))
                    .then_with(|| left.0 .1.cmp(&right.0 .1))
            });
            let mut admitted = Vec::new();
            let mut batch_reserved_raw = 0_i128;
            for ((instrument, visible_ts), orders) in admission_batches {
                let market = runtime.get(&instrument).expect("market validated");
                let visible_close = market
                    .bars
                    .iter()
                    .rev()
                    .find(|bar| bar.ts <= visible_ts)
                    .map(|bar| bar.close)
                    .ok_or_else(|| {
                        qx_core::QxError::BusinessViolation(format!(
                            "策略在 {} 尚无可见行情时下单",
                            instrument
                        ))
                    })?;
                let visible_marks = runtime
                    .iter()
                    .filter_map(|(candidate_id, candidate)| {
                        candidate
                            .bars
                            .iter()
                            .rev()
                            .find(|bar| bar.ts <= visible_ts)
                            .map(|bar| (candidate_id.clone(), Price::from_raw(bar.close)))
                    })
                    .collect::<BTreeMap<_, _>>();
                let other_pending = runtime
                    .iter()
                    .filter(|(key, _)| **key != instrument)
                    .fold(0_i128, |total, (_, market)| {
                        total.saturating_add(market.matcher.pending_funding_reservation_raw())
                    })
                    .saturating_add(batch_reserved_raw);
                let other_margin = other_market_margin(
                    &ledger,
                    &config.account_id,
                    &instrument,
                    &specs,
                    &visible_marks,
                    &runtime,
                )?;
                let derivative_spec = market
                    .spec
                    .product
                    .supports_leverage()
                    .then_some(&market.spec);
                let position = ledger
                    .position_for(&config.account_id, &instrument)
                    .quantity
                    .raw();
                let previous_log_len = log.events().len();
                let accepted = order_admission::admit_orders(OrderAdmissionInput {
                    orders,
                    account_id: &config.account_id,
                    instrument: &instrument,
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
                    portfolio_marks: Some(&visible_marks),
                    portfolio_specs: Some(&specs),
                    risk: &config.risk,
                    fx_rates: &BTreeMap::new(),
                    ashare_rules: None,
                    ashare_bought_today: 0,
                    next_id: &mut next_id,
                    log: &mut log,
                })?;
                let projection = rejections_by_instrument
                    .get_mut(&instrument)
                    .expect("rejection projection exists");
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
                    admitted.push((instrument.clone(), order));
                }
            }
            for (instrument, accepted) in admitted {
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
                runtime
                    .get_mut(&instrument)
                    .expect("matcher exists")
                    .matcher
                    .submit_with_funding_reservation_at(
                        accepted.order,
                        ts,
                        accepted.funding_reservation_raw,
                    );
            }

            for event in timestamp_events {
                let instrument = event.instrument.clone();
                let market = runtime.get_mut(&instrument).expect("market validated");
                let market_fills = market.matcher.on_bar(&event.bar, ts);
                for fill in market_fills {
                    fill_application::apply(FillApplicationInput {
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
                    let stored = fills.last().expect("fill reducer appended fill").clone();
                    fills_by_instrument
                        .get_mut(&instrument)
                        .expect("market fill projection exists")
                        .push(stored);
                }
                let previous_count = processed.get(&instrument).copied().unwrap_or_default();
                processed.insert(instrument.clone(), previous_count + 1);
                marks.insert(instrument, Price::from_raw(event.bar.close));
                mark_timestamps.insert(event.instrument.clone(), event.bar.ts);
                let marked_equity = ledger
                    .valuate(
                        &ValuationContext::spot(&config.account_id, &marks, &config.currency)
                            .with_market_specs(&specs),
                    )?
                    .equity
                    .raw();
                equity.push((ts, marked_equity));
            }
        }
        log.validate()?;
        let report = PortfolioBarSessionReport {
            fills,
            fills_by_instrument,
            rejections_by_instrument,
            equity,
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
