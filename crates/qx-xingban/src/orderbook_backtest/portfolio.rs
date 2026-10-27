//! Shared-account spot order-book execution across multiple markets.

mod model;

use model::{
    reference_price, total_reserved_cash, total_reserved_cash_exact, validate_market_data_tier,
    write_market_identity, MarketRuntime,
};
pub use model::{
    OrderBookPortfolioBacktestEngine, OrderBookPortfolioBacktestReport, OrderBookPortfolioMarket,
};

use super::funding::{
    self, FillFundingContext, FundingContext, FundingDecision, SpotCashReservations,
};
use super::report::{accumulate_exact_fees, exact_equity_metrics};
use super::{
    append_event, checked_raw_total, exact_raw_projection, fill_turnover_exact,
    fill_turnover_projection_raw, reference_price_exact,
};
use crate::portfolio_depth_timeline::{PortfolioDepthSeries, PortfolioDepthTimeline};
use crate::OrderBookMatchingEngine;
use qx_core::{
    EventKind, EventLog, ExactDecimal, ExactValuationContext, Fnv1a, Ledger, Money, OrderStatus,
    Price, Priority, TradingProduct, ValuationContext,
};
use qx_risk::{ExactOrderRiskFacts, OrderRiskContext, OrderRiskPosition};
use qx_zhenlu::{Oms, RiskGate};
use std::collections::{BTreeMap, BTreeSet};

impl<'a> OrderBookPortfolioBacktestEngine<'a> {
    pub fn new(
        account_id: impl Into<String>,
        currency: impl Into<String>,
        initial_cash: Money,
        risk: RiskGate,
        markets: Vec<OrderBookPortfolioMarket<'a>>,
    ) -> Result<Self, qx_core::QxError> {
        let initial_cash_exact = ExactDecimal::from_parts(initial_cash.raw(), 9)
            .map_err(|error| qx_core::QxError::BusinessViolation(error.into()))?;
        Self::new_inner(
            account_id,
            currency,
            initial_cash.raw(),
            initial_cash_exact,
            risk,
            markets,
            false,
        )
    }

    /// Build a shared L1/L2 run with an authoritative exact principal and a legacy raw projection.
    /// A positive principal may project to raw zero; exact ledger accounting remains authoritative.
    pub fn new_with_exact_initial_cash(
        account_id: impl Into<String>,
        currency: impl Into<String>,
        initial_cash_raw: i128,
        initial_cash_exact: ExactDecimal,
        risk: RiskGate,
        markets: Vec<OrderBookPortfolioMarket<'a>>,
    ) -> Result<Self, qx_core::QxError> {
        Self::new_inner(
            account_id,
            currency,
            initial_cash_raw,
            initial_cash_exact,
            risk,
            markets,
            true,
        )
    }

    fn new_inner(
        account_id: impl Into<String>,
        currency: impl Into<String>,
        initial_cash_raw: i128,
        initial_cash_exact: ExactDecimal,
        risk: RiskGate,
        markets: Vec<OrderBookPortfolioMarket<'a>>,
        exact_initial_cash_contract: bool,
    ) -> Result<Self, qx_core::QxError> {
        let account_id = account_id.into();
        let currency = currency.into().trim().to_ascii_uppercase();
        if account_id.trim().is_empty()
            || currency.is_empty()
            || !initial_cash_exact.is_positive()
            || exact_raw_projection(initial_cash_exact)? != initial_cash_raw
        {
            return Err(qx_core::QxError::BusinessViolation(
                "共享订单簿回测账户、结算币和精确初始资金/兼容投影必须有效".into(),
            ));
        }
        if !(2..=32).contains(&markets.len()) {
            return Err(qx_core::QxError::BusinessViolation(
                "共享订单簿回测市场数量必须在 2..=32".into(),
            ));
        }
        let mut seen = BTreeSet::new();
        let mut derivative_portfolio = None;
        for market in &markets {
            let spec = &market.instrument_spec;
            spec.validate()?;
            if !seen.insert(spec.instrument.clone()) {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "共享订单簿回测标的重复: {}",
                    spec.instrument
                )));
            }
            let is_derivative = spec.product.is_derivative();
            if spec.product != TradingProduct::Spot && !is_derivative {
                return Err(qx_core::QxError::BusinessViolation(
                    "共享多市场订单簿只支持现货或永续/期货；保证金市场和期权暂不支持".into(),
                ));
            }
            if derivative_portfolio.is_some_and(|existing| existing != is_derivative) {
                return Err(qx_core::QxError::BusinessViolation(
                    "共享订单簿暂不支持现货与衍生品混合保证金；请拆分为同产品组合或使用单市场回测"
                        .into(),
                ));
            }
            derivative_portfolio = Some(is_derivative);
            if is_derivative {
                if spec.exact_rules.is_none() {
                    return Err(qx_core::QxError::BusinessViolation(format!(
                        "共享衍生市场 {} 必须冻结精确交易规则",
                        spec.instrument
                    )));
                }
                if !spec.settlement_currency.eq_ignore_ascii_case(&currency) {
                    return Err(qx_core::QxError::BusinessViolation(format!(
                        "共享衍生市场 {} 结算币 {} 与组合结算币 {} 不一致；跨币种保证金尚未支持",
                        spec.instrument, spec.settlement_currency, currency
                    )));
                }
                if market
                    .snapshots
                    .iter()
                    .any(|snapshot| !snapshot.has_exact_levels())
                {
                    return Err(qx_core::QxError::BusinessViolation(format!(
                        "共享衍生市场 {} 要求所有快照使用 DepthFrame v2 精确盘口",
                        spec.instrument
                    )));
                }
            } else if !spec.quote_currency.eq_ignore_ascii_case(&currency)
                || !spec.settlement_currency.eq_ignore_ascii_case(&currency)
            {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "共享订单簿现货市场 {} 报价/结算币必须与组合结算币 {} 一致",
                    spec.instrument, currency
                )));
            }
            if market.snapshots.len() < 2 {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "共享订单簿市场 {} 至少需要两份快照",
                    spec.instrument
                )));
            }
            validate_market_data_tier(market)?;
        }
        Ok(Self {
            account_id,
            currency,
            initial_cash_raw,
            initial_cash_exact,
            exact_initial_cash_contract,
            risk,
            markets,
            funding_bps: BTreeMap::new(),
            funding_events: Vec::new(),
        })
    }

    /// Apply constant per-instrument rates at normalized UTC eight-hour boundaries.
    pub fn with_funding_bps(mut self, rates: BTreeMap<qx_core::InstrumentId, i64>) -> Self {
        self.funding_bps = rates;
        self
    }

    /// Apply exchange-provided historical funding rates at their declared settlement times.
    pub fn with_funding_events(mut self, events: Vec<qx_core::FundingRateEvent>) -> Self {
        self.funding_events = events;
        self
    }

    pub fn run(self) -> Result<OrderBookPortfolioBacktestReport, qx_core::QxError> {
        self.run_with_cancel(|| false)?
            .ok_or_else(|| qx_core::QxError::Invariant("共享订单簿回测意外取消".into()))
    }
    pub fn run_with_cancel<F>(
        self,
        mut is_cancelled: F,
    ) -> Result<Option<OrderBookPortfolioBacktestReport>, qx_core::QxError>
    where
        F: FnMut() -> bool,
    {
        let Self {
            account_id,
            currency,
            initial_cash_raw,
            initial_cash_exact,
            exact_initial_cash_contract,
            risk,
            mut markets,
            funding_bps,
            funding_events,
        } = self;
        markets.sort_by(|left, right| {
            left.priority.cmp(&right.priority).then_with(|| {
                left.instrument_spec
                    .instrument
                    .cmp(&right.instrument_spec.instrument)
            })
        });
        let series = markets
            .iter()
            .map(|market| PortfolioDepthSeries {
                instrument: market.instrument_spec.instrument.clone(),
                priority: market.priority,
                snapshots: market.snapshots,
            })
            .collect::<Vec<_>>();
        let timeline =
            PortfolioDepthTimeline::new(&series).map_err(qx_core::QxError::BusinessViolation)?;
        let mut input_hash = Fnv1a::new();
        let mut runtime = Vec::with_capacity(markets.len());
        let mut execution_models = BTreeMap::new();
        let mut specs = BTreeMap::new();
        let mut index_by_instrument = BTreeMap::new();
        for (index, market) in markets.into_iter().enumerate() {
            let instrument = market.instrument_spec.instrument.clone();
            write_market_identity(&mut input_hash, &market)?;
            execution_models.insert(
                instrument.clone(),
                market.execution_model.audit_descriptor(),
            );
            let matcher = OrderBookMatchingEngine::with_model(market.execution_model)
                .map_err(qx_core::QxError::BusinessViolation)?
                .with_fee_spec(Some(market.instrument_spec.clone()));
            specs.insert(instrument.clone(), market.instrument_spec.clone());
            index_by_instrument.insert(instrument.clone(), index);
            runtime.push(MarketRuntime {
                instrument,
                spec: market.instrument_spec,
                fee_bps: market.execution_model.fee_bps,
                strategy: market.strategy,
                matcher,
                reservations: SpotCashReservations::default(),
                exact_queue_estimation_enabled: market.execution_model.queue_position_bps != 0
                    || market.execution_model.queue_position_model
                        != crate::QueuePositionModel::Disabled,
                cancellations_for_next_snapshot: Vec::new(),
            });
        }
        let first_ts = timeline.events()[0].snapshot.ts;
        let last_ts = timeline
            .events()
            .last()
            .map(|event| event.snapshot.ts)
            .ok_or_else(|| qx_core::QxError::BusinessViolation("共享深度时间线为空".into()))?;
        let settlements = super::settlement::scheduled_settlements(
            first_ts,
            last_ts,
            &funding_bps,
            &funding_events,
            &specs,
        )?;
        for (ts, instruments) in &settlements {
            input_hash.write_u64(*ts);
            for (instrument, (rate, funding_interval_ms)) in instruments {
                input_hash.write_text(&instrument.to_string());
                input_hash.write_i128(i128::from(*rate));
                input_hash.write_u64(*funding_interval_ms);
            }
        }
        let fee_bps_by_instrument = runtime
            .iter()
            .map(|market| (market.instrument.clone(), market.fee_bps))
            .collect::<BTreeMap<_, _>>();
        let data_hash = input_hash.finish();
        let start_result = runtime
            .iter_mut()
            .try_for_each(|market| market.strategy.on_start());
        if let Err(start_error) = start_result {
            let mut stop_error = None;
            for market in &mut runtime {
                if let Err(error) = market
                    .strategy
                    .on_stop_with_reason(qx_strategy::StrategyStopReason::Failed)
                {
                    stop_error.get_or_insert(error);
                }
            }
            return Err(qx_core::QxError::BusinessViolation(match stop_error {
                Some(stop_error) => format!(
                    "共享订单簿策略 on_start 失败: {start_error}; on_stop 也失败: {stop_error}"
                ),
                None => format!("共享订单簿策略 on_start 失败: {start_error}"),
            }));
        }
        let result = (|| {
            let mut ledger = Ledger::new();
            let mut log = EventLog::new();
            let mut oms = Oms::new();
            let first_ts = timeline.events()[0].snapshot.ts;
            let deposit_id = if exact_initial_cash_contract {
                ledger.deposit_exact(&account_id, &currency, initial_cash_exact, first_ts)?
            } else {
                ledger.deposit(
                    &account_id,
                    &currency,
                    Money::from_raw(initial_cash_raw),
                    first_ts,
                )?
            };
            let deposit = ledger
                .entry(deposit_id)
                .cloned()
                .ok_or_else(|| qx_core::QxError::Invariant("共享订单簿初始入金缺失".into()))?;
            append_event(
                &mut log,
                deposit.ts,
                Priority::TIMER,
                EventKind::LedgerApplied { entry: deposit },
            );
            let mut owners = BTreeMap::<u64, (usize, u64)>::new();
            let mut local_to_global = BTreeMap::<(usize, u64), u64>::new();
            let mut next_order_id = 1_u64;
            let mut marks = BTreeMap::new();
            let mut marks_exact = BTreeMap::new();
            let mut mark_timestamps = BTreeMap::new();
            let mut settlement_cursor = settlements.into_iter().peekable();
            let mut fills = Vec::new();
            let mut fills_by_instrument = runtime
                .iter()
                .map(|market| (market.instrument.clone(), Vec::new()))
                .collect::<BTreeMap<_, _>>();
            let mut equity = Vec::new();
            let mut equity_exact = Vec::new();
            let mut fees_raw = 0_i128;
            let mut turnover_raw = 0_i128;
            let mut fees_exact_by_currency = BTreeMap::new();
            let mut turnover_exact = ExactDecimal::ZERO;
            let initial_equity_exact = initial_cash_exact;
            let mut cursor = 0;
            while cursor < timeline.events().len() {
                if is_cancelled() {
                    return Ok(None);
                }
                let ts = timeline.events()[cursor].snapshot.ts;
                let start = cursor;
                while cursor < timeline.events().len()
                    && timeline.events()[cursor].snapshot.ts == ts
                {
                    cursor += 1;
                }
                let batch = &timeline.events()[start..cursor];
                while settlement_cursor
                    .peek()
                    .is_some_and(|(settlement_ts, _)| *settlement_ts < ts)
                {
                    let (settlement_ts, rates) =
                        settlement_cursor.next().expect("peeked settlement");
                    super::settlement::apply_settlements(
                        settlement_ts,
                        &rates,
                        &account_id,
                        &currency,
                        &specs,
                        &marks_exact,
                        &mark_timestamps,
                        &mut ledger,
                        &mut log,
                    )?;
                }
                for event in batch {
                    if let Some(mark) = super::reference_price(event.snapshot) {
                        marks.insert(event.instrument.clone(), mark);
                    }
                    let exact_mark = super::reference_price_exact(event.snapshot).or_else(|| {
                        super::reference_price(event.snapshot)
                            .and_then(|mark| super::settlement::exact_price(mark.raw()).ok())
                    });
                    if let Some(mark) = exact_mark {
                        marks_exact.insert(event.instrument.clone(), mark);
                        mark_timestamps.insert(event.instrument.clone(), ts);
                    }
                }
                while settlement_cursor
                    .peek()
                    .is_some_and(|(settlement_ts, _)| *settlement_ts == ts)
                {
                    let (settlement_ts, rates) =
                        settlement_cursor.next().expect("peeked settlement");
                    super::settlement::apply_settlements(
                        settlement_ts,
                        &rates,
                        &account_id,
                        &currency,
                        &specs,
                        &marks_exact,
                        &mark_timestamps,
                        &mut ledger,
                        &mut log,
                    )?;
                }
                let mut cancel_acks = Vec::new();
                for event in batch {
                    let market_index = *index_by_instrument
                        .get(&event.instrument)
                        .ok_or_else(|| qx_core::QxError::Invariant("深度事件市场未登记".into()))?;
                    let cancellations =
                        std::mem::take(&mut runtime[market_index].cancellations_for_next_snapshot);
                    for local_id in cancellations {
                        let global_id = local_to_global
                            .remove(&(market_index, local_id))
                            .ok_or_else(|| {
                                qx_core::QxError::ReconcileRequired(format!(
                                    "共享订单簿撤单找不到全局订单映射: {}/{}",
                                    runtime[market_index].instrument, local_id
                                ))
                            })?;
                        let order = oms.get(global_id).cloned().ok_or_else(|| {
                            qx_core::QxError::ReconcileRequired(
                                "共享订单簿撤单找不到 OMS 订单".into(),
                            )
                        })?;
                        let market = &mut runtime[market_index];
                        if !market.matcher.cancel(global_id) {
                            return Err(qx_core::QxError::Invariant(format!(
                                "共享订单簿待撤订单 {global_id} 不在撮合器中"
                            )));
                        }
                        oms.cancel(global_id)?;
                        market.reservations.release(global_id);
                        let mut local_order = order;
                        local_order.client_id = local_id;
                        market.strategy.on_order_cancelled(&local_order)?;
                        owners.remove(&global_id);
                        cancel_acks.push(global_id);
                    }
                    let market_fills = runtime[market_index]
                        .matcher
                        .on_snapshot(event.snapshot)
                        .map_err(qx_core::QxError::Permanent)?;
                    for mut fill in market_fills {
                        let mut order = oms.get(fill.order_id).cloned().ok_or_else(|| {
                            qx_core::QxError::ReconcileRequired(
                                "共享订单簿成交找不到 OMS 订单".into(),
                            )
                        })?;
                        let owner = owners.get(&fill.order_id).copied().ok_or_else(|| {
                            qx_core::QxError::ReconcileRequired(
                                "共享订单簿成交找不到策略订单归属".into(),
                            )
                        })?;
                        if owner.0 != market_index {
                            return Err(qx_core::QxError::Invariant(
                                "共享订单簿撮合器返回了其他市场的订单".into(),
                            ));
                        }
                        order.trace_fill(&mut fill, None, None);
                        accumulate_exact_fees(&mut fees_exact_by_currency, &fill, &currency)
                            .map_err(qx_core::QxError::Permanent)?;
                        turnover_exact = turnover_exact
                            .checked_add(fill_turnover_exact(
                                Some(&runtime[market_index].spec),
                                &fill,
                            )?)
                            .ok_or_else(|| {
                                qx_core::QxError::Invariant("共享精确成交额累计溢出".into())
                            })?;
                        if fill.exact_amounts.is_some() {
                            let settlement_fees = fill
                                .exact_fees
                                .iter()
                                .filter(|fee| {
                                    fee.currency.as_deref().is_some_and(|fee_currency| {
                                        fee_currency.eq_ignore_ascii_case(&currency)
                                    })
                                })
                                .try_fold(0_i128, |total, fee| {
                                    total
                                        .checked_add(exact_raw_projection(fee.amount)?)
                                        .ok_or_else(|| {
                                            qx_core::QxError::Invariant(
                                                "共享精确手续费 raw 投影累计溢出".into(),
                                            )
                                        })
                                })?;
                            fees_raw = checked_raw_total(
                                fees_raw,
                                settlement_fees,
                                "共享手续费 raw 投影累计溢出",
                            )?;
                        } else {
                            fees_raw = checked_raw_total(
                                fees_raw,
                                fill.fee.raw(),
                                "共享手续费 raw 投影累计溢出",
                            )?;
                        }
                        let reserved_cash = total_reserved_cash(&runtime)?;
                        let reserved_cash_exact = total_reserved_cash_exact(&runtime)?;
                        let market = &mut runtime[market_index];
                        let reservation_update = market.reservations.plan_fill(
                            &order,
                            &fill,
                            FillFundingContext {
                                spec: Some(&market.spec),
                                cash_before_fill: ledger.cash_for(&account_id, &currency),
                                total_reserved_before_fill: reserved_cash,
                                inventory_before_fill: ledger
                                    .position_for(&account_id, &market.instrument)
                                    .quantity
                                    .raw(),
                                total_inventory_reserved_before_fill: market
                                    .reservations
                                    .total_inventory()?,
                                cash_before_fill_exact: ledger
                                    .cash_for_exact(&account_id, &currency),
                                total_reserved_cash_exact_before_fill: reserved_cash_exact,
                                inventory_before_fill_exact: ledger
                                    .position_for_side_exact(
                                        &account_id,
                                        &market.instrument,
                                        qx_core::PositionSide::Net,
                                    )
                                    .quantity,
                            },
                        )?;
                        let mut local_order = order.clone();
                        local_order.client_id = owner.1;
                        let mut local_fill = fill.clone();
                        local_fill.order_id = owner.1;
                        market.strategy.validate_fill_budget(
                            &local_order,
                            &local_fill,
                            Some(&market.spec),
                        )?;
                        let entry_ids = qx_core::apply_fill_to_books(
                            &mut ledger,
                            &mut oms,
                            &currency,
                            &fill,
                            qx_core::FillTerms::resolve(Some(&market.spec), 1),
                        )?;
                        if market.spec.product.is_derivative() {
                            if fill.exact_amounts.is_none() {
                                return Err(qx_core::QxError::BusinessViolation(
                                    "共享衍生成交缺少精确成交事实".into(),
                                ));
                            }
                            let mut fill_marks_exact = marks_exact.clone();
                            fill_marks_exact
                                .insert(market.instrument.clone(), fill.price_decimal());
                            let order_after_fill = oms.get(fill.order_id).ok_or_else(|| {
                                qx_core::QxError::ReconcileRequired(
                                    "共享精确衍生成交后找不到 OMS 订单".into(),
                                )
                            })?;
                            funding::validate_multi_market_exact_margin_after_fill(
                                order_after_fill,
                                &specs,
                                &fee_bps_by_instrument,
                                &fill_marks_exact,
                                &ledger,
                                &account_id,
                                &currency,
                                &oms.all_orders(),
                            )?;
                        }
                        market.reservations.commit_fill(reservation_update);
                        turnover_raw = checked_raw_total(
                            turnover_raw,
                            fill_turnover_projection_raw(Some(&market.spec), &fill)?,
                            "共享 raw 成交额累计溢出",
                        )?;
                        let fill_event_seq = append_event(
                            &mut log,
                            fill.ts,
                            Priority::FEEDBACK,
                            EventKind::Filled { fill: fill.clone() },
                        );
                        for entry_id in entry_ids {
                            let entry = ledger.entry(entry_id).cloned().ok_or_else(|| {
                                qx_core::QxError::Invariant("共享订单簿账簿 entry 缺失".into())
                            })?;
                            append_event(
                                &mut log,
                                fill.ts,
                                Priority::FEEDBACK,
                                EventKind::LedgerApplied { entry },
                            );
                        }
                        market.strategy.on_fill_with_event_seq(
                            &local_order,
                            &local_fill,
                            fill_event_seq,
                        )?;
                        fills_by_instrument
                            .get_mut(&market.instrument)
                            .ok_or(qx_core::QxError::Invariant("成交明细投影未注册".into()))?
                            .push(fill.clone());
                        fills.push(fill);
                        order = oms.get(order.client_id).cloned().ok_or_else(|| {
                            qx_core::QxError::ReconcileRequired(
                                "共享订单簿成交后 OMS 订单消失".into(),
                            )
                        })?;
                        if order.status == OrderStatus::Filled {
                            owners.remove(&order.client_id);
                            local_to_global.remove(&(owner.0, owner.1));
                        }
                    }
                }
                for global_id in cancel_acks {
                    append_event(
                        &mut log,
                        ts,
                        Priority::MARKET,
                        EventKind::Cancelled {
                            client_order_id: global_id,
                        },
                    );
                }
                let mut accepted_ids = Vec::new();
                for event in batch {
                    let market_index =
                        *index_by_instrument.get(&event.instrument).ok_or_else(|| {
                            qx_core::QxError::Invariant("订单簿时间线标的未注册".into())
                        })?;
                    let reference_price = reference_price(event.snapshot);
                    let reference_price_exact = reference_price_exact(event.snapshot);
                    if let Some(price) = reference_price {
                        marks.insert(event.instrument.clone(), price);
                    }
                    if let Some(price) = reference_price_exact {
                        marks_exact.insert(event.instrument.clone(), price);
                    }
                    let mut total_reserved = total_reserved_cash(&runtime)?;
                    let mut total_reserved_exact = total_reserved_cash_exact(&runtime)?;
                    let cash = ledger.cash_balances_for_ref(&account_id);
                    let cash_exact = ledger.cash_balances_exact_for(&account_id);
                    let free_cash = ledger
                        .cash_for(&account_id, &currency)
                        .saturating_sub(total_reserved)
                        .max(0);
                    let free_cash_exact = ledger
                        .cash_for_exact(&account_id, &currency)
                        .checked_sub(total_reserved_exact)
                        .ok_or_else(|| {
                            qx_core::QxError::Invariant("共享精确订单簿资金预留超过账户现金".into())
                        })?;
                    let market_spec = &runtime[market_index].spec;
                    let (available_margin_raw, available_margin_exact) = if market_spec
                        .product
                        .is_derivative()
                    {
                        let (used_margin, fee_reserve) =
                            funding::projected_multi_market_margin_and_fees_exact(
                                None,
                                &specs,
                                &fee_bps_by_instrument,
                                &marks_exact,
                                &ledger,
                                &account_id,
                                &oms.all_orders(),
                            )?;
                        let equity = funding::marked_multi_market_equity_exact(
                            &ledger,
                            &account_id,
                            &currency,
                            &specs,
                            &marks_exact,
                        )?;
                        let free = equity
                            .checked_sub(used_margin)
                            .and_then(|value| value.checked_sub(fee_reserve))
                            .ok_or_else(|| {
                                qx_core::QxError::Invariant("共享精确衍生可用保证金计算溢出".into())
                            })?;
                        let free = if free.is_negative() {
                            ExactDecimal::ZERO
                        } else {
                            free
                        };
                        (
                            Some(funding::exact_to_raw(
                                free,
                                qx_core::DecimalRounding::TowardZero,
                            )?),
                            Some(free),
                        )
                    } else {
                        (Some(free_cash), Some(free_cash_exact))
                    };
                    let market = &mut runtime[market_index];
                    market.strategy.update_account_state_exact(
                        cash,
                        &cash_exact,
                        available_margin_raw,
                        available_margin_exact,
                    );
                    let position = ledger
                        .position_for(&account_id, &market.instrument)
                        .quantity
                        .raw();
                    let position_exact = ledger
                        .position_for_side_exact(
                            &account_id,
                            &market.instrument,
                            qx_core::PositionSide::Net,
                        )
                        .quantity;
                    let open_local = oms
                        .active_order_ids()
                        .into_iter()
                        .filter_map(|global_id| {
                            owners.get(&global_id).and_then(|(owner_market, local_id)| {
                                (*owner_market == market_index).then_some(*local_id)
                            })
                        })
                        .collect::<Vec<_>>();
                    let actions = market
                        .strategy
                        .on_order_book_actions_with_exact_position(
                            event.snapshot,
                            position,
                            position_exact,
                            &open_local,
                            available_margin_raw,
                        )
                        .map_err(qx_core::QxError::BusinessViolation)?;
                    let open_set = open_local.into_iter().collect::<BTreeSet<_>>();
                    let mut seen_cancels = BTreeSet::new();
                    for local_id in actions.cancel_client_ids {
                        if !seen_cancels.insert(local_id) || !open_set.contains(&local_id) {
                            return Err(qx_core::QxError::BusinessViolation(format!(
                                "共享订单簿策略撤单目标重复或非活动订单: {}/{}",
                                market.instrument, local_id
                            )));
                        }
                        market.cancellations_for_next_snapshot.push(local_id);
                    }
                    let mut seen_order_ids = BTreeSet::new();
                    for mut local_order in actions.orders {
                        let local_id = local_order.client_id;
                        let global_id = next_order_id;
                        next_order_id = next_order_id.checked_add(1).ok_or_else(|| {
                            qx_core::QxError::Invariant("共享订单簿全局订单 ID 溢出".into())
                        })?;
                        if local_id == 0
                            || !seen_order_ids.insert(local_id)
                            || local_to_global.contains_key(&(market_index, local_id))
                        {
                            append_event(
                                &mut log,
                                ts,
                                Priority::COMMAND,
                                EventKind::Rejected {
                                    client_order_id: global_id,
                                    reason: "策略本地 client_id 必须为正且在本市场唯一".into(),
                                },
                            );
                            continue;
                        }
                        if local_order.account_id.is_empty() {
                            local_order.account_id = account_id.clone();
                        }
                        if local_order.instrument != market.instrument
                            || local_order.account_id != account_id
                        {
                            append_event(
                                &mut log,
                                ts,
                                Priority::COMMAND,
                                EventKind::Rejected {
                                    client_order_id: global_id,
                                    reason: "订单账户或标的与共享订单簿市场不一致".into(),
                                },
                            );
                            continue;
                        }
                        if event.snapshot.has_exact_levels() && local_order.exact_amounts.is_none()
                        {
                            append_event(
                                &mut log,
                                ts,
                                Priority::COMMAND,
                                EventKind::Rejected {
                                    client_order_id: global_id,
                                    reason: "共享多市场 DepthFrame v2 精确盘口要求精确数量订单；拒绝把 legacy 1e-9 订单回退到盘口投影".into(),
                                },
                            );
                            continue;
                        }
                        if local_order.exact_amounts.is_some()
                            && (!event.snapshot.has_exact_levels()
                                || !market.strategy.supports_exact_orders())
                        {
                            append_event(
                                &mut log,
                                ts,
                                Priority::COMMAND,
                                EventKind::Rejected {
                                    client_order_id: global_id,
                                    reason: "共享订单簿精确订单要求 DepthFrame v2 且策略实现 exact hooks".into(),
                                },
                            );
                            continue;
                        }
                        if local_order.exact_amounts.is_some()
                            && market.exact_queue_estimation_enabled
                        {
                            append_event(
                                &mut log,
                                ts,
                                Priority::COMMAND,
                                EventKind::Rejected {
                                    client_order_id: global_id,
                                    reason: "共享精确盘口撮合暂不支持队列位置估算模型".into(),
                                },
                            );
                            continue;
                        }
                        if let Some(amounts) = local_order.exact_amounts.as_mut() {
                            if amounts.limit_price.is_none()
                                && amounts.market_reference_price.is_none()
                            {
                                amounts.market_reference_price = reference_price_exact;
                            }
                        }
                        if market.spec.product.is_derivative() && local_order.policy.is_none() {
                            append_event(
                                &mut log,
                                ts,
                                Priority::COMMAND,
                                EventKind::Rejected {
                                    client_order_id: global_id,
                                    reason: "共享衍生订单必须显式携带 OrderPolicy".into(),
                                },
                            );
                            continue;
                        }
                        let policy = local_order.policy.unwrap_or_default();
                        let validation = policy.validate_for(&market.spec).and_then(|()| {
                            if let Some(amounts) = local_order.exact_amounts {
                                if let Some(budget) = amounts.quote_budget {
                                    market.spec.validate_exact_quote_budget(budget)?;
                                    let reference = amounts
                                        .market_reference_price
                                        .or(reference_price_exact)
                                        .ok_or_else(|| {
                                            qx_core::QxError::BusinessViolation(
                                                "共享精确预算订单缺少参考价格".into(),
                                            )
                                        })?;
                                    let quantity = market
                                        .spec
                                        .base_quantity_for_quote_budget(budget, reference)?;
                                    market.spec.validate_exact_order_with_reference(
                                        quantity,
                                        None,
                                        Some(reference),
                                    )
                                } else {
                                    market.spec.validate_exact_order_with_reference(
                                        amounts.quantity,
                                        amounts.limit_price,
                                        amounts.market_reference_price.or(reference_price_exact),
                                    )
                                }
                            } else {
                                market.spec.validate_order_with_reference(
                                    local_order.qty.raw().checked_abs().unwrap_or(0),
                                    local_order.limit.map(|price| price.raw()),
                                    reference_price.map(|price| price.raw()),
                                )
                            }
                        });
                        if let Err(error) = validation {
                            append_event(
                                &mut log,
                                ts,
                                Priority::COMMAND,
                                EventKind::Rejected {
                                    client_order_id: global_id,
                                    reason: error.to_string(),
                                },
                            );
                            continue;
                        }
                        let position_snapshot = OrderRiskPosition::new_with_multiplier(
                            position,
                            reference_price
                                .map(|price| {
                                    market.spec.notional(position.saturating_abs(), price.raw())
                                })
                                .transpose()?
                                .unwrap_or(0),
                            market.spec.contract_size,
                        );
                        let exact_risk = if local_order.exact_amounts.is_some() {
                            let exact_reference = local_order
                                .market_reference_price_decimal()
                                .or(reference_price_exact)
                                .or_else(|| local_order.limit_price_decimal())
                                .ok_or_else(|| {
                                    qx_core::QxError::BusinessViolation(
                                        "共享精确订单簿风控缺少精确参考价".into(),
                                    )
                                })?;
                            let exact_position = super::pending_exposure::exact_position_snapshot(
                                &ledger,
                                super::pending_exposure::PendingExposureContext {
                                    risk: &risk,
                                    oms: &oms,
                                    candidate_order_id: global_id,
                                    account_id: &account_id,
                                    instrument: &market.instrument,
                                    spec: Some(&market.spec),
                                    reference_price,
                                },
                                exact_reference,
                            )?;
                            if market.spec.product.is_derivative() {
                                let mut portfolio_candidate = local_order.clone();
                                portfolio_candidate.client_id = global_id;
                                let (projected_margin, projected_fees) =
                                    funding::projected_multi_market_margin_and_fees_exact(
                                        Some(&portfolio_candidate),
                                        &specs,
                                        &fee_bps_by_instrument,
                                        &marks_exact,
                                        &ledger,
                                        &account_id,
                                        &oms.all_orders(),
                                    )?;
                                let equity = funding::marked_multi_market_equity_exact(
                                    &ledger,
                                    &account_id,
                                    &currency,
                                    &specs,
                                    &marks_exact,
                                )?;
                                let available_margin =
                                    equity.checked_sub(projected_fees).ok_or_else(|| {
                                        qx_core::QxError::Invariant(
                                            "共享精确衍生手续费预留计算溢出".into(),
                                        )
                                    })?;
                                Some(ExactOrderRiskFacts {
                                    available_margin: Some(available_margin),
                                    projected_initial_margin: Some(projected_margin),
                                    reference_price: Some(exact_reference),
                                    max_order_notional: None,
                                    max_position_notional: None,
                                    position: exact_position,
                                })
                            } else {
                                let available_cash_exact = ledger
                                    .cash_for_exact(&account_id, &currency)
                                    .checked_sub(total_reserved_exact)
                                    .ok_or_else(|| {
                                        qx_core::QxError::Invariant(
                                            "共享精确订单风控可用现金计算溢出".into(),
                                        )
                                    })?;
                                Some(ExactOrderRiskFacts {
                                    available_margin: Some(available_cash_exact),
                                    projected_initial_margin: None,
                                    reference_price: Some(exact_reference),
                                    max_order_notional: None,
                                    max_position_notional: None,
                                    position: exact_position,
                                })
                            }
                        } else {
                            None
                        };
                        let funding = funding::preflight_order(
                            &local_order,
                            policy,
                            Some(&market.spec),
                            FundingContext {
                                snapshot: event.snapshot,
                                reference_price,
                                reference_price_exact,
                                ledger: &ledger,
                                account_id: &account_id,
                                currency: &currency,
                                fee_bps: market.fee_bps,
                                reservations: &market.reservations,
                                total_reserved_cash_raw: Some(total_reserved),
                                total_reserved_cash_exact: Some(total_reserved_exact),
                            },
                        )?;
                        let reserve = match funding {
                            FundingDecision::Accept {
                                reserve_cash_raw,
                                reserve_inventory_raw,
                            } => (reserve_cash_raw, reserve_inventory_raw, None),
                            FundingDecision::NotRequired => (0, 0, None),
                            FundingDecision::AcceptExact {
                                reserve_quantity,
                                reserve_cash,
                                reserve_inventory,
                            } => (
                                funding::exact_to_raw(
                                    reserve_cash,
                                    qx_core::DecimalRounding::Ceiling,
                                )?,
                                funding::exact_to_raw(
                                    reserve_inventory,
                                    qx_core::DecimalRounding::Ceiling,
                                )?,
                                Some((
                                    reserve_quantity,
                                    reserve_cash,
                                    reserve_inventory,
                                    local_order.quote_budget_decimal(),
                                )),
                            ),
                            FundingDecision::Reject(reason) => {
                                append_event(
                                    &mut log,
                                    ts,
                                    Priority::COMMAND,
                                    EventKind::Rejected {
                                        client_order_id: global_id,
                                        reason,
                                    },
                                );
                                continue;
                            }
                        };
                        let risk_context = OrderRiskContext {
                            reference_price,
                            instrument_spec: Some(market.spec.clone()),
                            position: position_snapshot,
                            exact: exact_risk,
                            ..OrderRiskContext::default()
                        };
                        if let Err(error) = risk.check_with_context(&local_order, &risk_context) {
                            append_event(
                                &mut log,
                                ts,
                                Priority::COMMAND,
                                EventKind::Rejected {
                                    client_order_id: global_id,
                                    reason: error.to_string(),
                                },
                            );
                            continue;
                        }
                        if let Some(amounts) = local_order.exact_amounts {
                            let price = amounts
                                .limit_price
                                .or(amounts.market_reference_price)
                                .or(reference_price_exact)
                                .ok_or_else(|| {
                                    qx_core::QxError::BusinessViolation(
                                        "共享精确策略预算缺少精确参考价".into(),
                                    )
                                })?;
                            market.strategy.check_exact_order_budget(
                                &local_order,
                                price,
                                Some(&market.spec),
                            )?;
                        } else if let Some(price) = local_order.limit.or(reference_price) {
                            market.strategy.check_order_budget(
                                &local_order,
                                price.raw(),
                                Some(&market.spec),
                            )?;
                        }
                        local_order.client_id = global_id;
                        local_order.status = OrderStatus::Submitted;
                        market
                            .matcher
                            .submit(local_order.clone())
                            .map_err(qx_core::QxError::BusinessViolation)?;
                        oms.submit(local_order.clone())?;
                        oms.accept(global_id)?;
                        let (reserve_cash, reserve_inventory, reserve_exact) = reserve;
                        if let Some((quantity, cash, inventory, quote_budget)) = reserve_exact {
                            market.reservations.reserve_exact(
                                global_id,
                                quantity,
                                cash,
                                inventory,
                                quote_budget,
                            )?;
                        } else if reserve_cash > 0 || reserve_inventory > 0 {
                            market.reservations.reserve(
                                global_id,
                                local_order.qty.raw(),
                                reserve_cash,
                                reserve_inventory,
                            )?;
                        }
                        total_reserved =
                            total_reserved.checked_add(reserve_cash).ok_or_else(|| {
                                qx_core::QxError::Invariant("共享盘口现金预留溢出".into())
                            })?;
                        total_reserved_exact = total_reserved_exact
                            .checked_add(match reserve_exact {
                                Some((_, cash, _, _)) => cash,
                                None => ExactDecimal::from_parts(reserve_cash, 9)
                                    .map_err(|error| qx_core::QxError::Invariant(error.into()))?,
                            })
                            .ok_or_else(|| {
                                qx_core::QxError::Invariant("共享精确盘口现金预留溢出".into())
                            })?;
                        owners.insert(global_id, (market_index, local_id));
                        local_to_global.insert((market_index, local_id), global_id);
                        let mut callback_order = local_order.clone();
                        callback_order.client_id = local_id;
                        if callback_order.exact_amounts.is_some() {
                            let exact_reference = callback_order
                                .market_reference_price_decimal()
                                .or(reference_price_exact)
                                .or_else(|| callback_order.limit_price_decimal())
                                .ok_or_else(|| {
                                    qx_core::QxError::Invariant(
                                        "共享精确订单接受反馈缺少精确参考价".into(),
                                    )
                                })?;
                            market
                                .strategy
                                .on_exact_order_accepted(&callback_order, exact_reference)?;
                        } else {
                            market.strategy.on_order_accepted(
                                &callback_order,
                                reference_price.map(Price::raw),
                            )?;
                        }
                        append_event(
                            &mut log,
                            ts,
                            Priority::COMMAND,
                            EventKind::OrderSubmitted {
                                order: local_order.clone(),
                            },
                        );
                        accepted_ids.push(global_id);
                    }
                    if let Some(price) = reference_price {
                        marks.insert(market.instrument.clone(), price);
                    }
                    if let Some(price) = reference_price_exact {
                        marks_exact.insert(market.instrument.clone(), price);
                    }
                }
                for global_id in accepted_ids {
                    append_event(
                        &mut log,
                        ts,
                        Priority::MATCH,
                        EventKind::Accepted {
                            client_order_id: global_id,
                            venue_order_id: None,
                        },
                    );
                }
                let valuation = ValuationContext::spot(&account_id, &marks, &currency)
                    .with_market_specs(&specs);
                let legacy_marked_equity = if exact_initial_cash_contract {
                    None
                } else {
                    Some(ledger.valuate(&valuation)?.equity.raw())
                };
                let marked_equity_exact = ledger.valuate_exact(
                    &ExactValuationContext::new(valuation).with_exact_marks(&marks_exact),
                )?;
                let marked_equity = match legacy_marked_equity {
                    Some(value) => value,
                    None => exact_raw_projection(marked_equity_exact)?,
                };
                equity.push((ts, marked_equity));
                equity_exact.push((ts, marked_equity_exact));
            }
            log.validate()?;
            let exact_curve = equity_exact
                .iter()
                .map(|(_, value)| *value)
                .collect::<Vec<_>>();
            let exact_metrics = exact_equity_metrics(initial_equity_exact, &exact_curve)
                .map_err(qx_core::QxError::Invariant)?;
            let return_bps = exact_metrics
                .return_bps
                .unwrap_or_default()
                .clamp(i128::from(i32::MIN), i128::from(i32::MAX))
                as i32;
            let max_drawdown_bps = exact_metrics.max_drawdown_bps.clamp(0, 10_000) as u32;
            let pending_orders = runtime
                .iter()
                .map(|market| market.matcher.pending_count())
                .sum();
            let clock_end = timeline
                .events()
                .last()
                .map(|event| event.snapshot.ts)
                .ok_or(qx_core::QxError::Invariant("深度时间线为空".into()))?;
            let report = OrderBookPortfolioBacktestReport {
                fills,
                fills_by_instrument,
                execution_models,
                ledger,
                event_log: log,
                equity,
                equity_exact,
                initial_equity_exact,
                pending_orders,
                fees_raw,
                turnover_raw,
                fees_exact_by_currency,
                turnover_exact,
                turnover_currency: currency.clone(),
                return_bps,
                max_drawdown_bps,
                input_data_hash: data_hash,
                clock_start: first_ts,
                clock_end,
            };
            report.verify()?;
            Ok(Some(report))
        })();
        let stop_reason = match &result {
            Ok(Some(_)) => qx_strategy::StrategyStopReason::Completed,
            Ok(None) => qx_strategy::StrategyStopReason::Cancelled,
            Err(_) => qx_strategy::StrategyStopReason::Failed,
        };
        let mut stop_error = None;
        for market in &mut runtime {
            if let Err(error) = market.strategy.on_stop_with_reason(stop_reason) {
                stop_error.get_or_insert(error);
            }
        }
        match (result, stop_error) {
            (Ok(report), None) => Ok(report),
            (Ok(_), Some(stop_error)) => Err(qx_core::QxError::BusinessViolation(format!(
                "共享订单簿策略 on_stop 失败: {stop_error}"
            ))),
            (Err(primary_error), None) => Err(primary_error),
            (Err(primary_error), Some(stop_error)) => Err(qx_core::QxError::BusinessViolation(
                format!("{primary_error}; 共享订单簿策略 on_stop 也失败: {stop_error}"),
            )),
        }
    }
}
