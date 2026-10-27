//! 基于 L1/L2 盘口快照的确定性回测入口。
//!
//! 因果顺序固定为：先撮合上一时刻订单，再让策略看到当前盘口并提交新订单；
//! 因此策略不会用当前快照直接成交，避免回测中的同一快照作弊。

use qx_core::{
    Event, EventKind, EventLog, ExactDecimal, ExactValuationContext, Fnv1a, Ledger, OrderStatus,
    Price, Priority, ReplayVerifier, TradingInstrumentSpec, ValuationContext,
};
use qx_risk::{ExactOrderRiskFacts, OrderRiskContext, OrderRiskPosition};
use qx_zhenlu::{Oms, RiskGate};

use crate::orderbook_strategies::OrderBookStrategy;
use crate::{DataTier, OrderBookExecutionModel, OrderBookMatchingEngine, OrderBookSnapshot};
use std::collections::BTreeMap;

mod funding;
mod margin;
mod pending_exposure;
mod portfolio;
mod report;
mod settlement;
use funding::{FillFundingContext, FundingContext, FundingDecision, SpotCashReservations};
pub use portfolio::{
    OrderBookPortfolioBacktestEngine, OrderBookPortfolioBacktestReport, OrderBookPortfolioMarket,
};
use report::{accumulate_exact_fees, exact_equity_metrics, execution_model_descriptors};
pub use report::{OrderBookBacktestReport, QueuePositionSensitivityReport};

fn append_event(log: &mut EventLog, ts: u64, priority: u8, kind: EventKind) -> u64 {
    let seq = log.alloc_seq();
    log.append(Event::new(seq, ts, priority, kind));
    seq
}

fn checked_raw_total(
    total: i128,
    delta: i128,
    label: &'static str,
) -> Result<i128, qx_core::QxError> {
    total
        .checked_add(delta)
        .ok_or_else(|| qx_core::QxError::Invariant(label.into()))
}

pub struct OrderBookBacktestConfig {
    pub instrument: qx_core::InstrumentId,
    pub account_id: String,
    pub currency: String,
    pub initial_cash: qx_core::Money,
    pub fee_bps: i128,
    /// 可选产品规格；衍生品必须显式携带 OrderPolicy 并按规格记账。
    pub instrument_spec: Option<TradingInstrumentSpec>,
    pub risk: RiskGate,
    /// 输入深度档位声明；L2 以上才有逐档撮合意义，L1 由 Tick 引擎固定。
    pub data_tier: DataTier,
}

fn book_notional(
    spec: Option<&TradingInstrumentSpec>,
    qty: i128,
    price: i128,
) -> Result<i128, qx_core::QxError> {
    match spec {
        Some(spec) => spec.notional(qty, price),
        None => qx_core::fee::notional(qty, price),
    }
}

pub(super) fn fill_turnover_projection_raw(
    spec: Option<&TradingInstrumentSpec>,
    fill: &qx_core::Fill,
) -> Result<i128, qx_core::QxError> {
    if let Some(amounts) = fill.exact_amounts {
        let spec = spec.ok_or_else(|| {
            qx_core::QxError::BusinessViolation(
                "精确订单簿成交名义额投影缺少 TradingInstrumentSpec".into(),
            )
        })?;
        return spec
            .notional_exact(amounts.quantity, amounts.price)?
            .checked_mul_at_scale(ExactDecimal::ONE, 9, qx_core::DecimalRounding::TowardZero)
            .and_then(|projected| projected.to_raw(9).ok())
            .ok_or_else(|| {
                qx_core::QxError::Invariant("精确订单簿成交名义额 raw 投影失败".into())
            });
    }
    book_notional(spec, fill.qty.raw(), fill.price.raw())
}

pub(super) fn fill_turnover_exact(
    spec: Option<&TradingInstrumentSpec>,
    fill: &qx_core::Fill,
) -> Result<ExactDecimal, qx_core::QxError> {
    if let Some(amounts) = fill.exact_amounts {
        let spec = spec.ok_or_else(|| {
            qx_core::QxError::BusinessViolation(
                "精确订单簿成交名义额缺少 TradingInstrumentSpec".into(),
            )
        })?;
        return spec.notional_exact(amounts.quantity, amounts.price);
    }
    ExactDecimal::from_parts(fill_turnover_projection_raw(spec, fill)?, 9)
        .map_err(|error| qx_core::QxError::Invariant(error.into()))
}

pub(super) fn exact_raw_projection(value: ExactDecimal) -> Result<i128, qx_core::QxError> {
    value
        .checked_mul_at_scale(ExactDecimal::ONE, 9, qx_core::DecimalRounding::TowardZero)
        .and_then(|projected| projected.to_raw(9).ok())
        .ok_or_else(|| qx_core::QxError::Invariant("精确指标 raw 投影失败".into()))
}

fn reference_price(snapshot: &OrderBookSnapshot) -> Option<Price> {
    match (
        snapshot.bids.first().map(|level| level.price),
        snapshot.asks.first().map(|level| level.price),
    ) {
        (Some(bid), Some(ask)) => {
            let spread = ask.raw().checked_sub(bid.raw())?;
            Some(Price::from_raw(bid.raw().checked_add(spread / 2)?))
        }
        (Some(bid), None) => Some(bid),
        (None, Some(ask)) => Some(ask),
        (None, None) => None,
    }
}

fn reference_price_exact(snapshot: &OrderBookSnapshot) -> Option<ExactDecimal> {
    match (
        snapshot.bids.first().map(|level| level.price_decimal()),
        snapshot.asks.first().map(|level| level.price_decimal()),
    ) {
        (Some(bid), Some(ask)) => {
            let spread = ask.checked_sub(bid)?;
            let half_spread = spread.checked_div_at_scale(
                ExactDecimal::from_parts(2, 0).ok()?,
                ExactDecimal::MAX_SCALE,
                qx_core::DecimalRounding::TowardZero,
            )?;
            bid.checked_add(half_spread)
        }
        (Some(bid), None) => Some(bid),
        (None, Some(ask)) => Some(ask),
        (None, None) => None,
    }
}

enum AcceptedFundingReservation {
    Raw {
        cash: i128,
        inventory: i128,
    },
    Exact {
        quantity: ExactDecimal,
        cash: ExactDecimal,
        inventory: ExactDecimal,
    },
}

pub struct OrderBookBacktestEngine {
    config: OrderBookBacktestConfig,
    execution_model: Option<OrderBookExecutionModel>,
    funding_bps: i64,
    funding_events: Vec<qx_core::FundingRateEvent>,
}

impl OrderBookBacktestEngine {
    pub fn new(config: OrderBookBacktestConfig) -> Self {
        Self {
            config,
            execution_model: None,
            funding_bps: 0,
            funding_events: Vec::new(),
        }
    }

    pub fn with_execution_model(mut self, model: OrderBookExecutionModel) -> Self {
        self.execution_model = Some(model);
        self
    }

    /// Apply a constant rate at each normalized UTC eight-hour funding boundary.
    pub fn with_funding_bps(mut self, rate_bps: i64) -> Self {
        self.funding_bps = rate_bps;
        self
    }

    /// Apply exchange-provided historical funding rates at their declared settlement times.
    pub fn with_funding_events(mut self, events: Vec<qx_core::FundingRateEvent>) -> Self {
        self.funding_events = events;
        self
    }

    pub fn run(
        self,
        snapshots: &[OrderBookSnapshot],
        strategy: &mut dyn OrderBookStrategy,
    ) -> Result<OrderBookBacktestReport, qx_core::QxError> {
        self.run_with_cancel(snapshots, strategy, || false)?
            .ok_or_else(|| qx_core::QxError::Invariant("未请求取消但订单簿回测未产生报告".into()))
    }

    /// Run with cooperative cancellation checks before execution and each snapshot.
    /// `None` means cancellation; partial reports are never returned.
    pub fn run_with_cancel<F>(
        self,
        snapshots: &[OrderBookSnapshot],
        strategy: &mut dyn OrderBookStrategy,
        mut is_cancelled: F,
    ) -> Result<Option<OrderBookBacktestReport>, qx_core::QxError>
    where
        F: FnMut() -> bool,
    {
        if let Err(start_error) = strategy.on_start() {
            return match strategy.on_stop_with_reason(qx_strategy::StrategyStopReason::Failed) {
                Ok(()) => Err(qx_core::QxError::BusinessViolation(format!(
                    "策略 on_start 失败: {start_error}"
                ))),
                Err(stop_error) => Err(qx_core::QxError::BusinessViolation(format!(
                    "策略 on_start 失败: {start_error}; on_stop 也失败: {stop_error}"
                ))),
            };
        }
        let result = self.run_with_cancel_inner(snapshots, strategy, &mut is_cancelled);
        let stop_reason = match &result {
            Ok(Some(_)) => qx_strategy::StrategyStopReason::Completed,
            Ok(None) => qx_strategy::StrategyStopReason::Cancelled,
            Err(_) => qx_strategy::StrategyStopReason::Failed,
        };
        let stop_result = strategy.on_stop_with_reason(stop_reason);
        match (result, stop_result) {
            (Ok(report), Ok(())) => Ok(report),
            (Ok(_), Err(stop_error)) => Err(qx_core::QxError::BusinessViolation(format!(
                "策略 on_stop 失败: {stop_error}"
            ))),
            (Err(primary_error), Ok(())) => Err(primary_error),
            (Err(primary_error), Err(stop_error)) => Err(qx_core::QxError::BusinessViolation(
                format!("{primary_error}; 策略 on_stop 也失败: {stop_error}"),
            )),
        }
    }

    fn run_with_cancel_inner<F>(
        self,
        snapshots: &[OrderBookSnapshot],
        strategy: &mut dyn OrderBookStrategy,
        mut is_cancelled: F,
    ) -> Result<Option<OrderBookBacktestReport>, qx_core::QxError>
    where
        F: FnMut() -> bool,
    {
        let OrderBookBacktestEngine {
            config,
            execution_model,
            funding_bps,
            funding_events,
        } = self;
        let OrderBookBacktestConfig {
            instrument,
            account_id,
            currency,
            initial_cash,
            fee_bps,
            instrument_spec,
            risk,
            data_tier,
        } = config;
        let currency = currency.trim().to_ascii_uppercase();
        let risk_rule_set_version = risk.rule_set().version().to_string();
        if account_id.trim().is_empty() || currency.is_empty() {
            return Err(qx_core::QxError::BusinessViolation(
                "订单簿回测账户和结算币不能为空".into(),
            ));
        }
        if let Some(spec) = instrument_spec.as_ref() {
            spec.validate()?;
            if spec.instrument != instrument {
                return Err(qx_core::QxError::BusinessViolation(
                    "订单簿回测产品规格 instrument 与配置不一致".into(),
                ));
            }
            let cash_currency = if spec.product == qx_core::TradingProduct::Spot {
                &spec.quote_currency
            } else {
                &spec.settlement_currency
            };
            if !cash_currency.eq_ignore_ascii_case(&currency) {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "订单簿回测账户币种 {currency} 与 {:?} 市场现金/结算币种 {cash_currency} 不一致",
                    spec.product,
                )));
            }
        }
        let mut cancellations_for_next_snapshot = Vec::new();
        for snapshot in snapshots {
            if is_cancelled() {
                return Ok(None);
            }
            snapshot.validate().map_err(qx_core::QxError::Permanent)?;
            if snapshot.instrument != instrument {
                return Err(qx_core::QxError::BusinessViolation(
                    "订单簿回测快照 instrument 与配置不一致".into(),
                ));
            }
        }
        if snapshots
            .windows(2)
            .any(|window| window[0].ts >= window[1].ts || window[0].sequence >= window[1].sequence)
        {
            return Err(qx_core::QxError::BusinessViolation(
                "订单簿回测快照 ts/sequence 必须严格递增".into(),
            ));
        }
        crate::orderbook::validate_backtest_data_tier(data_tier, snapshots)
            .map_err(qx_core::QxError::BusinessViolation)?;
        let specs = instrument_spec
            .as_ref()
            .map(|spec| BTreeMap::from([(instrument.clone(), spec.clone())]))
            .unwrap_or_default();
        let funding_rates = if funding_bps == 0 {
            BTreeMap::new()
        } else {
            BTreeMap::from([(instrument.clone(), funding_bps)])
        };
        let settlements = settlement::scheduled_settlements(
            snapshots.first().map(|snapshot| snapshot.ts).unwrap_or(0),
            snapshots.last().map(|snapshot| snapshot.ts).unwrap_or(0),
            &funding_rates,
            &funding_events,
            &specs,
        )?;
        let funding_settlement_count = settlements.len();
        if let Some(model) = execution_model.as_ref() {
            // 与上方规格/instrument 校验同一约定：冲突要报错，不能让配置里的
            // fee_bps 被执行模型静默覆盖，否则回测成本口径与声明不一致。
            if model.fee_bps != fee_bps {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "订单簿回测执行模型 fee_bps={} 与配置 fee_bps={fee_bps} 不一致",
                    model.fee_bps
                )));
            }
        }
        let mut input_hash = Fnv1a::new();
        input_hash.write_text(&instrument.to_string());
        input_hash.write_u64(snapshots.len() as u64);
        for snapshot in snapshots {
            input_hash.write_u64(snapshot.ts);
            input_hash.write_u64(snapshot.sequence);
            input_hash.write_u64(snapshot.bids.len() as u64);
            input_hash.write_u64(snapshot.asks.len() as u64);
            for level in snapshot.bids.iter().chain(snapshot.asks.iter()) {
                level.write_identity(&mut input_hash);
            }
        }
        for (ts, instruments) in &settlements {
            input_hash.write_u64(*ts);
            for (instrument, (rate, funding_interval_ms)) in instruments {
                input_hash.write_text(&instrument.to_string());
                input_hash.write_i128(i128::from(*rate));
                input_hash.write_u64(*funding_interval_ms);
            }
        }
        let input_data_hash = input_hash.finish();
        // 关键执行参数必须进入模型描述子，避免只传给撮合器却不留证据。
        let effective_model = match execution_model {
            Some(model) => model,
            None => OrderBookExecutionModel::new(fee_bps)
                .map_err(qx_core::QxError::BusinessViolation)?,
        };
        let exact_queue_estimation_enabled = effective_model.queue_position_bps != 0
            || effective_model.queue_position_model != crate::QueuePositionModel::Disabled;
        let mut model_descriptors = execution_model_descriptors(
            data_tier,
            &effective_model,
            &risk_rule_set_version,
            instrument_spec.as_ref(),
        );
        if funding_bps != 0 || !funding_events.is_empty() {
            model_descriptors.push(format!(
                "funding_rate_bps={funding_bps}; funding_history_event_count={}; effective_settlement_timestamps={funding_settlement_count}",
                funding_events.len()
            ));
        }
        let execution_model_descriptor = effective_model.audit_descriptor();
        let mut matcher = OrderBookMatchingEngine::with_model(effective_model)
            .map_err(qx_core::QxError::BusinessViolation)?
            .with_fee_spec(instrument_spec.clone());
        let mut spot_cash_reservations = SpotCashReservations::default();
        let mut ledger = Ledger::new();
        let mut log = EventLog::new();
        let mut marks_exact = BTreeMap::new();
        let mut mark_timestamps = BTreeMap::new();
        let mut settlements = settlements.into_iter().peekable();
        let mut oms = Oms::new();
        let mut fills = Vec::new();
        let mut equity_curve = Vec::with_capacity(snapshots.len());
        let mut equity_curve_exact = Vec::with_capacity(snapshots.len());
        let mut position_curve = Vec::with_capacity(snapshots.len());
        let mut snapshot_curve = Vec::with_capacity(snapshots.len());
        let mut fees_raw = 0_i128;
        let mut turnover_raw = 0_i128;
        let mut fees_exact_by_currency = BTreeMap::new();
        let mut turnover_exact = ExactDecimal::ZERO;
        let initial_equity_exact = ExactDecimal::from_parts(initial_cash.raw(), 9)
            .map_err(|error| qx_core::QxError::Invariant(error.into()))?;
        let deposit_id = ledger.deposit(
            &account_id,
            &currency,
            initial_cash,
            snapshots.first().map(|snapshot| snapshot.ts).unwrap_or(1),
        )?;
        let deposit = ledger
            .entry(deposit_id)
            .cloned()
            .ok_or_else(|| qx_core::QxError::Invariant("订单簿回测初始入金缺失".into()))?;
        append_event(
            &mut log,
            deposit.ts,
            // 初始入金是本观测起点**之前**就已成立的账户状态：策略在同一时间戳上就能用它下单。
            // 用 APPLY 会让起点时间戳上的策略命令（COMMAND=3）在因果序里倒退，
            // 深度链的事实流就不是可重放的规范日志（V11 Q62 —— 此前没有任何一处校验过这件事）。
            Priority::TIMER,
            EventKind::LedgerApplied { entry: deposit },
        );

        for snapshot in snapshots {
            if is_cancelled() {
                return Ok(None);
            }
            let current_reference_price = reference_price(snapshot);
            let current_reference_price_exact = reference_price_exact(snapshot);
            while settlements
                .peek()
                .is_some_and(|(settlement_ts, _)| *settlement_ts < snapshot.ts)
            {
                let (settlement_ts, rates) = settlements.next().expect("peeked settlement");
                settlement::apply_settlements(
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
            if let Some(price) = current_reference_price_exact.or_else(|| {
                current_reference_price.and_then(|price| settlement::exact_price(price.raw()).ok())
            }) {
                marks_exact.insert(instrument.clone(), price);
                mark_timestamps.insert(instrument.clone(), snapshot.ts);
            }
            while settlements
                .peek()
                .is_some_and(|(settlement_ts, _)| *settlement_ts == snapshot.ts)
            {
                let (settlement_ts, rates) = settlements.next().expect("peeked settlement");
                settlement::apply_settlements(
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
            let mut cancelled_this_snapshot = Vec::new();
            for client_order_id in &cancellations_for_next_snapshot {
                let order = oms.get(*client_order_id).cloned().ok_or_else(|| {
                    qx_core::QxError::ReconcileRequired("撤单回报找不到 OMS 订单".into())
                })?;
                if !matcher.cancel(*client_order_id) {
                    return Err(qx_core::QxError::Invariant(format!(
                        "做市策略待撤订单 {client_order_id} 不在撮合器中"
                    )));
                }
                oms.cancel(*client_order_id)?;
                spot_cash_reservations.release(order.client_id);
                strategy.on_order_cancelled(&order)?;
                cancelled_this_snapshot.push(*client_order_id);
            }
            cancellations_for_next_snapshot.clear();
            for mut fill in matcher
                .on_snapshot(snapshot)
                .map_err(qx_core::QxError::Permanent)?
            {
                let order = oms.get(fill.order_id).cloned().ok_or_else(|| {
                    qx_core::QxError::ReconcileRequired("订单簿成交找不到 OMS 订单".into())
                })?;
                order.trace_fill(&mut fill, None, None);
                accumulate_exact_fees(&mut fees_exact_by_currency, &fill, &currency)
                    .map_err(qx_core::QxError::Permanent)?;
                turnover_exact = turnover_exact
                    .checked_add(fill_turnover_exact(instrument_spec.as_ref(), &fill)?)
                    .ok_or_else(|| qx_core::QxError::Invariant("精确成交额累计溢出".into()))?;
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
                                    qx_core::QxError::Invariant("手续费 raw 投影累计溢出".into())
                                })
                        })?;
                    fees_raw =
                        checked_raw_total(fees_raw, settlement_fees, "手续费 raw 投影累计溢出")?;
                } else {
                    fees_raw =
                        checked_raw_total(fees_raw, fill.fee.raw(), "手续费 raw 投影累计溢出")?;
                }
                let reservation_update = spot_cash_reservations.plan_fill(
                    &order,
                    &fill,
                    FillFundingContext {
                        spec: instrument_spec.as_ref(),
                        cash_before_fill: ledger.cash_for(&account_id, &currency),
                        total_reserved_before_fill: spot_cash_reservations.total()?,
                        inventory_before_fill: ledger
                            .position_for(&account_id, &instrument)
                            .quantity
                            .raw(),
                        total_inventory_reserved_before_fill: spot_cash_reservations
                            .total_inventory()?,
                        cash_before_fill_exact: ledger.cash_for_exact(&account_id, &currency),
                        total_reserved_cash_exact_before_fill: spot_cash_reservations
                            .total_cash_exact()?,
                        inventory_before_fill_exact: ledger
                            .position_for_side_exact(
                                &account_id,
                                &instrument,
                                qx_core::PositionSide::Net,
                            )
                            .quantity,
                    },
                )?;
                strategy.validate_fill_budget(&order, &fill, instrument_spec.as_ref())?;
                let entry_ids = qx_core::apply_fill_to_books(
                    &mut ledger,
                    &mut oms,
                    &currency,
                    &fill,
                    qx_core::FillTerms::resolve(instrument_spec.as_ref(), 1),
                )?;
                if fill.exact_amounts.is_some() {
                    if let Some(spec) = instrument_spec
                        .as_ref()
                        .filter(|spec| spec.product.is_derivative())
                    {
                        let order_after_fill = oms.get(fill.order_id).ok_or_else(|| {
                            qx_core::QxError::ReconcileRequired(
                                "精确衍生成交后找不到 OMS 订单".into(),
                            )
                        })?;
                        funding::validate_exact_derivative_margin_after_fill(
                            order_after_fill,
                            spec,
                            fill.price_decimal(),
                            fee_bps,
                            &ledger,
                            &account_id,
                            &oms.all_orders(),
                        )?;
                    }
                }
                spot_cash_reservations.commit_fill(reservation_update);
                turnover_raw = checked_raw_total(
                    turnover_raw,
                    fill_turnover_projection_raw(instrument_spec.as_ref(), &fill)?,
                    "raw 成交额累计溢出",
                )?;
                let fill_event_seq = append_event(
                    &mut log,
                    fill.ts,
                    // 深度链的因果口径是 `match-previous-submit-current`：这一批成交回报来自
                    // **更早时刻**提交的订单，只是在本快照到期。它们排在本时点策略命令之前，
                    // 所以占用 FEEDBACK 槽；用 APPLY 会让同一时间戳上的 COMMAND 在因果序里倒退，
                    // 事实流就不再是可重放的规范日志（V11 Q62）。
                    Priority::FEEDBACK,
                    EventKind::Filled { fill: fill.clone() },
                );
                for entry_id in entry_ids {
                    let entry = ledger.entry(entry_id).cloned().ok_or_else(|| {
                        qx_core::QxError::Invariant("订单簿回测账簿 entry 缺失".into())
                    })?;
                    append_event(
                        &mut log,
                        fill.ts,
                        // 与上面那条 `Filled` 同一个槽：账簿事实必须紧跟它所依据的成交回报。
                        Priority::FEEDBACK,
                        EventKind::LedgerApplied { entry },
                    );
                }
                let cash = ledger.cash_balances_for_ref(&account_id);
                let cash_exact = ledger.cash_balances_exact_for(&account_id);
                let margin = margin::strategy_available_margin_raw(
                    instrument_spec.as_ref(),
                    reference_price(snapshot),
                    ledger.cash_for(&account_id, &currency),
                    fee_bps,
                    &ledger,
                    &account_id,
                    &oms.all_orders(),
                )?;
                strategy.update_account_state_exact(cash, &cash_exact, margin, None);
                strategy.on_fill_with_event_seq(&order, &fill, fill_event_seq)?;
                fills.push(fill);
            }
            // The venue-side cancel is applied before matching, while its acknowledgement is
            // recorded after any other prior-order feedback so event priority remains canonical.
            for client_order_id in cancelled_this_snapshot {
                append_event(
                    &mut log,
                    snapshot.ts,
                    Priority::MARKET,
                    EventKind::Cancelled { client_order_id },
                );
            }

            let position = ledger.position_for(&account_id, &instrument).quantity.raw();
            let position_exact = ledger
                .position_for_side_exact(&account_id, &instrument, qx_core::PositionSide::Net)
                .quantity;
            let available_margin_raw = margin::strategy_available_margin_raw(
                instrument_spec.as_ref(),
                current_reference_price,
                ledger.cash_for(&account_id, &currency),
                fee_bps,
                &ledger,
                &account_id,
                &oms.all_orders(),
            )?;
            let cash = ledger.cash_balances_for_ref(&account_id);
            let cash_exact = ledger.cash_balances_exact_for(&account_id);
            strategy.update_account_state_exact(cash, &cash_exact, available_margin_raw, None);
            let reference_price = current_reference_price;
            let reference_price_exact = current_reference_price_exact;
            let open_order_ids = oms.active_order_ids();
            let open_order_set = open_order_ids
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            let actions = strategy
                .on_order_book_actions_with_exact_position(
                    snapshot,
                    position,
                    position_exact,
                    &open_order_ids,
                    available_margin_raw,
                )
                .map_err(qx_core::QxError::BusinessViolation)?;
            let mut unique_cancellations = std::collections::BTreeSet::new();
            for client_order_id in actions.cancel_client_ids {
                if !unique_cancellations.insert(client_order_id)
                    || !open_order_set.contains(&client_order_id)
                {
                    return Err(qx_core::QxError::BusinessViolation(format!(
                        "订单簿策略撤单目标重复或非活动订单: {client_order_id}"
                    )));
                }
                cancellations_for_next_snapshot.push(client_order_id);
            }
            let mut accepted_client_ids = Vec::new();
            for mut order in actions.orders {
                if order.client_id == 0 {
                    return Err(qx_core::QxError::BusinessViolation(
                        "订单簿策略返回的 client_order_id 必须为正".into(),
                    ));
                }
                if order.account_id.is_empty() {
                    order.account_id = account_id.clone();
                }
                if order.instrument != instrument || order.account_id != account_id {
                    append_event(
                        &mut log,
                        snapshot.ts,
                        Priority::COMMAND,
                        EventKind::Rejected {
                            client_order_id: order.client_id,
                            reason: "订单账户或标的与回测配置不一致".into(),
                        },
                    );
                    continue;
                }
                if snapshot.has_exact_levels() && order.exact_amounts.is_none() {
                    append_event(
                        &mut log,
                        snapshot.ts,
                        Priority::COMMAND,
                        EventKind::Rejected {
                            client_order_id: order.client_id,
                            reason: "DepthFrame v2 精确盘口要求精确数量订单；拒绝把 legacy 1e-9 订单回退到盘口投影".into(),
                        },
                    );
                    continue;
                }
                if order.exact_amounts.is_some()
                    && (!strategy.supports_exact_orders() || !snapshot.has_exact_levels())
                {
                    append_event(
                        &mut log,
                        snapshot.ts,
                        Priority::COMMAND,
                        EventKind::Rejected {
                            client_order_id: order.client_id,
                            reason: "精确订单要求策略实现 exact hooks 且当前盘口为 DepthFrame v2；否则拒绝将精确持仓或盘口折算为 1e-9".into(),
                        },
                    );
                    continue;
                }
                if order.exact_amounts.is_some() && exact_queue_estimation_enabled {
                    append_event(
                        &mut log,
                        snapshot.ts,
                        Priority::COMMAND,
                        EventKind::Rejected {
                            client_order_id: order.client_id,
                            reason: "精确盘口撮合暂不支持队列位置估算模型".into(),
                        },
                    );
                    continue;
                }
                if let Some(amounts) = order.exact_amounts.as_mut() {
                    if amounts.limit_price.is_none() && amounts.market_reference_price.is_none() {
                        amounts.market_reference_price = reference_price_exact;
                    }
                }
                if let Some(spec) = instrument_spec.as_ref() {
                    if spec.product.supports_leverage() && order.policy.is_none() {
                        append_event(
                            &mut log,
                            snapshot.ts,
                            Priority::COMMAND,
                            EventKind::Rejected {
                                client_order_id: order.client_id,
                                reason: "衍生品订单必须显式携带 OrderPolicy".into(),
                            },
                        );
                        continue;
                    }
                    let policy = order.policy.unwrap_or_default();
                    let validation = policy.validate_for(spec).and_then(|()| {
                        if let Some(amounts) = order.exact_amounts {
                            if (!spec.product.is_derivative()
                                && spec.product != qx_core::TradingProduct::Spot)
                                || spec.exact_rules.is_none()
                            {
                                return Err(qx_core::QxError::BusinessViolation(
                                    "精确订单簿回测仅支持带精确规则的现货、永续或期货".into(),
                                ));
                            }
                            if let Some(budget) = amounts.quote_budget {
                                spec.validate_exact_quote_budget(budget)?;
                                let reference = amounts
                                    .market_reference_price
                                    .or(reference_price_exact)
                                    .ok_or_else(|| {
                                        qx_core::QxError::BusinessViolation(
                                            "精确预算订单缺少参考价格".into(),
                                        )
                                    })?;
                                let quantity =
                                    spec.base_quantity_for_quote_budget(budget, reference)?;
                                spec.validate_exact_order_with_reference(
                                    quantity,
                                    None,
                                    Some(reference),
                                )
                            } else {
                                spec.validate_exact_order_with_reference(
                                    amounts.quantity,
                                    amounts.limit_price,
                                    amounts.market_reference_price.or(reference_price_exact),
                                )
                            }
                        } else {
                            spec.validate_order_with_reference(
                                order.qty.raw().checked_abs().unwrap_or(0),
                                order.limit.map(|price| price.raw()),
                                current_reference_price.map(|price| price.raw()),
                            )
                        }
                    });
                    if let Err(error) = validation {
                        append_event(
                            &mut log,
                            snapshot.ts,
                            Priority::COMMAND,
                            EventKind::Rejected {
                                client_order_id: order.client_id,
                                reason: error.to_string(),
                            },
                        );
                        continue;
                    }
                }
                let policy = order.policy.unwrap_or_default();
                let position_snapshot = pending_exposure::position_snapshot(
                    &ledger,
                    position,
                    pending_exposure::PendingExposureContext {
                        risk: &risk,
                        oms: &oms,
                        candidate_order_id: order.client_id,
                        account_id: &account_id,
                        instrument: &instrument,
                        spec: instrument_spec.as_ref(),
                        reference_price,
                    },
                )?;
                let exact_risk = if order.exact_amounts.is_some() {
                    let exact_reference = order
                        .market_reference_price_decimal()
                        .or(reference_price_exact)
                        .or_else(|| order.limit_price_decimal())
                        .ok_or_else(|| {
                            qx_core::QxError::BusinessViolation(
                                "精确订单簿风控缺少精确参考价".into(),
                            )
                        })?;
                    let spec = instrument_spec.as_ref().ok_or_else(|| {
                        qx_core::QxError::BusinessViolation(
                            "精确订单簿风控缺少 TradingInstrumentSpec".into(),
                        )
                    })?;
                    let exact_position = pending_exposure::exact_position_snapshot(
                        &ledger,
                        pending_exposure::PendingExposureContext {
                            risk: &risk,
                            oms: &oms,
                            candidate_order_id: order.client_id,
                            account_id: &account_id,
                            instrument: &instrument,
                            spec: Some(spec),
                            reference_price,
                        },
                        exact_reference,
                    )?;
                    if spec.product.is_derivative() {
                        let (projected_margin, projected_fees) =
                            funding::projected_margin_and_fees_exact(
                                &order,
                                policy,
                                spec,
                                exact_reference,
                                fee_bps,
                                &ledger,
                                &account_id,
                                &oms.all_orders(),
                            )?;
                        let equity = funding::marked_equity_exact(
                            &ledger,
                            &account_id,
                            spec,
                            exact_reference,
                        )?;
                        let available_margin =
                            equity.checked_sub(projected_fees).ok_or_else(|| {
                                qx_core::QxError::Invariant("精确衍生手续费预留超过标记权益".into())
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
                        let available_cash = ledger
                            .cash_for_exact(&account_id, &currency)
                            .checked_sub(spot_cash_reservations.total_cash_exact()?)
                            .ok_or_else(|| {
                                qx_core::QxError::Invariant("精确订单可用现金计算溢出".into())
                            })?;
                        Some(ExactOrderRiskFacts {
                            available_margin: Some(available_cash),
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
                let accepted_reservation = match funding::preflight_order(
                    &order,
                    policy,
                    instrument_spec.as_ref(),
                    FundingContext {
                        snapshot,
                        reference_price,
                        reference_price_exact,
                        ledger: &ledger,
                        account_id: &account_id,
                        currency: &currency,
                        fee_bps,
                        reservations: &spot_cash_reservations,
                        total_reserved_cash_raw: None,
                        total_reserved_cash_exact: None,
                    },
                )? {
                    FundingDecision::Accept {
                        reserve_cash_raw,
                        reserve_inventory_raw,
                    } => AcceptedFundingReservation::Raw {
                        cash: reserve_cash_raw,
                        inventory: reserve_inventory_raw,
                    },
                    FundingDecision::AcceptExact {
                        reserve_quantity,
                        reserve_cash,
                        reserve_inventory,
                    } => AcceptedFundingReservation::Exact {
                        quantity: reserve_quantity,
                        cash: reserve_cash,
                        inventory: reserve_inventory,
                    },
                    FundingDecision::NotRequired => AcceptedFundingReservation::Raw {
                        cash: 0,
                        inventory: 0,
                    },
                    FundingDecision::Reject(reason) => {
                        append_event(
                            &mut log,
                            snapshot.ts,
                            Priority::COMMAND,
                            EventKind::Rejected {
                                client_order_id: order.client_id,
                                reason,
                            },
                        );
                        continue;
                    }
                };
                let exact_derivative_order = order.exact_amounts.is_some()
                    && instrument_spec
                        .as_ref()
                        .is_some_and(|spec| spec.product.is_derivative());
                let (projected_initial_margin_raw, projected_fee_reserve_raw) =
                    match (instrument_spec.as_ref(), reference_price) {
                        _ if exact_derivative_order => (None, 0),
                        (Some(spec), Some(price)) if spec.product.supports_leverage() => {
                            let (margin, fees) = funding::projected_margin_and_fees_raw(
                                &order,
                                policy,
                                spec,
                                price,
                                fee_bps,
                                &ledger,
                                &account_id,
                                &oms.all_orders(),
                            )?;
                            (Some(margin), fees)
                        }
                        (Some(spec), None) if spec.product.supports_leverage() => {
                            append_event(
                                &mut log,
                                snapshot.ts,
                                Priority::COMMAND,
                                EventKind::Rejected {
                                    client_order_id: order.client_id,
                                    reason: "衍生品组合保证金计算缺少参考价".into(),
                                },
                            );
                            continue;
                        }
                        _ => (None, 0),
                    };
                let available_margin_raw = if exact_derivative_order {
                    None
                } else if instrument_spec
                    .as_ref()
                    .is_some_and(|spec| spec.product.supports_leverage())
                {
                    let Some(price) = reference_price else {
                        append_event(
                            &mut log,
                            snapshot.ts,
                            Priority::COMMAND,
                            EventKind::Rejected {
                                client_order_id: order.client_id,
                                reason: "衍生品组合保证金计算缺少参考价".into(),
                            },
                        );
                        continue;
                    };
                    let marks = BTreeMap::from([(instrument.clone(), price)]);
                    let valuation = ValuationContext::spot(&account_id, &marks, &currency)
                        .with_spec(instrument_spec.as_ref());
                    Some(
                        ledger
                            .valuate(&valuation)?
                            .equity
                            .raw()
                            .checked_sub(projected_fee_reserve_raw)
                            .ok_or_else(|| {
                                qx_core::QxError::Invariant("订单簿投影费用超过标记权益".into())
                            })?,
                    )
                } else {
                    None
                };
                let risk_context = OrderRiskContext {
                    available_margin_raw,
                    projected_initial_margin_raw,
                    reference_price,
                    instrument_spec: instrument_spec.clone(),
                    position: position_snapshot,
                    exact: exact_risk,
                    ..OrderRiskContext::default()
                };
                if let Err(error) = risk.check_with_context(&order, &risk_context) {
                    append_event(
                        &mut log,
                        snapshot.ts,
                        Priority::COMMAND,
                        EventKind::Rejected {
                            client_order_id: order.client_id,
                            reason: error.to_string(),
                        },
                    );
                    continue;
                }
                let exact_budget_price = order.limit_price_decimal().or(reference_price_exact);
                let budget_price = order.limit.or(reference_price);
                let budget_check = if order.exact_amounts.is_some() {
                    exact_budget_price.map_or_else(
                        || {
                            Err(qx_core::QxError::BusinessViolation(
                                "精确策略预算缺少精确参考价".into(),
                            ))
                        },
                        |price| {
                            strategy.check_exact_order_budget(
                                &order,
                                price,
                                instrument_spec.as_ref(),
                            )
                        },
                    )
                } else if let Some(price) = budget_price {
                    strategy.check_order_budget(&order, price.raw(), instrument_spec.as_ref())
                } else {
                    Ok(())
                };
                if let Err(error) = budget_check {
                    append_event(
                        &mut log,
                        snapshot.ts,
                        Priority::COMMAND,
                        EventKind::Rejected {
                            client_order_id: order.client_id,
                            reason: error.to_string(),
                        },
                    );
                    continue;
                }
                order.status = OrderStatus::Submitted;
                matcher
                    .submit(order.clone())
                    .map_err(qx_core::QxError::BusinessViolation)?;
                oms.submit(order.clone())?;
                oms.accept(order.client_id)?;
                if order.exact_amounts.is_some() {
                    strategy.on_exact_order_accepted(
                        &order,
                        exact_budget_price.ok_or_else(|| {
                            qx_core::QxError::Invariant("精确订单参考价缺失".into())
                        })?,
                    )?;
                } else {
                    strategy.on_order_accepted(&order, budget_price.map(|price| price.raw()))?;
                }
                match accepted_reservation {
                    AcceptedFundingReservation::Raw { cash, inventory }
                        if cash > 0 || inventory > 0 =>
                    {
                        spot_cash_reservations.reserve(
                            order.client_id,
                            order.qty.raw(),
                            cash,
                            inventory,
                        )?;
                    }
                    AcceptedFundingReservation::Exact {
                        quantity,
                        cash,
                        inventory,
                    } => {
                        spot_cash_reservations.reserve_exact(
                            order.client_id,
                            quantity,
                            cash,
                            inventory,
                            order.quote_budget_decimal(),
                        )?;
                    }
                    AcceptedFundingReservation::Raw { .. } => {}
                }
                append_event(
                    &mut log,
                    snapshot.ts,
                    Priority::COMMAND,
                    EventKind::OrderSubmitted {
                        order: order.clone(),
                    },
                );
                accepted_client_ids.push(order.client_id);
            }
            // Preserve canonical priority: record every command before acceptance facts.
            for client_order_id in accepted_client_ids {
                append_event(
                    &mut log,
                    snapshot.ts,
                    Priority::MATCH,
                    EventKind::Accepted {
                        client_order_id,
                        venue_order_id: None,
                    },
                );
            }
            let (marked_equity, marked_equity_exact) = match reference_price {
                Some(price) => {
                    let marks = BTreeMap::from([(instrument.clone(), price)]);
                    let spec = instrument_spec
                        .as_ref()
                        .filter(|spec| spec.product.supports_leverage());
                    let context =
                        ValuationContext::spot(&account_id, &marks, &currency).with_spec(spec);
                    let raw = ledger.valuate(&context)?.equity.raw();
                    let exact = if let Some(instrument_spec) = instrument_spec.as_ref() {
                        let exact_mark = current_reference_price_exact.ok_or_else(|| {
                            qx_core::QxError::ReconcileRequired(
                                "精确订单簿权益估值缺少精确标记价".into(),
                            )
                        })?;
                        let exact_marks = BTreeMap::from([(instrument.clone(), exact_mark)]);
                        let exact_context = ExactValuationContext::new(
                            ValuationContext::spot(&account_id, &marks, &currency)
                                .with_spec(Some(instrument_spec)),
                        )
                        .with_exact_marks(&exact_marks);
                        ledger.valuate_exact(&exact_context)?
                    } else {
                        let exact_context = ExactValuationContext::new(ValuationContext::spot(
                            &account_id,
                            &marks,
                            &currency,
                        ));
                        ledger.valuate_exact(&exact_context)?
                    };
                    (raw, exact)
                }
                None => (
                    ledger.cash_for(&account_id, &currency),
                    ledger.cash_for_exact(&account_id, &currency),
                ),
            };
            equity_curve.push(marked_equity);
            equity_curve_exact.push(marked_equity_exact);
            position_curve.push(position);
            snapshot_curve.push(snapshot.ts);
        }
        let exact_metrics = exact_equity_metrics(initial_equity_exact, &equity_curve_exact)
            .map_err(qx_core::QxError::Invariant)?;
        let return_bps = exact_metrics
            .return_bps
            .unwrap_or_default()
            .clamp(i128::from(i32::MIN), i128::from(i32::MAX)) as i32;
        let max_drawdown_bps = exact_metrics.max_drawdown_bps.clamp(0, 10_000) as u32;
        let mut report = OrderBookBacktestReport {
            fills,
            execution_attempts: matcher.execution_attempts().to_vec(),
            ledger,
            event_log: log,
            pending_orders: matcher.pending_count(),
            equity: equity_curve,
            equity_exact: equity_curve_exact,
            positions: position_curve,
            snapshot_ts: snapshot_curve,
            fees_raw,
            turnover_raw,
            fees_exact_by_currency,
            turnover_exact,
            turnover_currency: instrument_spec
                .as_ref()
                .map(|spec| spec.settlement_currency.trim().to_ascii_uppercase())
                .filter(|currency| !currency.is_empty())
                .unwrap_or_else(|| currency.trim().to_ascii_uppercase()),
            return_bps,
            max_drawdown_bps,
            initial_equity_raw: initial_cash.raw(),
            initial_equity_exact,
            input_data_hash,
            clock_start: snapshots.first().map(|snapshot| snapshot.ts).unwrap_or(0),
            clock_end: snapshots.last().map(|snapshot| snapshot.ts).unwrap_or(0),
            data_tier,
            risk_rule_set_version,
            execution_model_descriptor,
            assumptions: vec![
                format!("data_tier={data_tier:?}"),
                "benchmark=flat-cash".into(),
                "metrics=scale-aware-exact-equity-with-legacy-raw-projections".into(),
                "depth_frame_v2_exact_single_market_spot_and_derivatives=matching_exact_cash_or_margin_fees_ledger_without_queue_estimation; v1_uses_1e-9_projection".into(),
                "shared_multi_market_exact_derivatives=single_settlement_currency; mixed_products_and_cross_currency_fail_closed".into(),
                format!("funding_rate_bps={funding_bps}; funding_history_event_count={}; effective_settlement_timestamps={funding_settlement_count}; exact_cashflows_in_ledger", funding_events.len()),
                "causality=match-previous-submit-current".into(),
                format!("pending_orders={}", matcher.pending_count()),
            ],
            model_descriptors,
        };
        report.append_limit_order_fill_rate_descriptor();
        // Facts must replay into the same ledger before this report is returned (V11 Q62).
        ReplayVerifier::verify(report.event_log.events(), &report.ledger)?;
        Ok(Some(report))
    }
}

#[cfg(test)]
#[path = "orderbook_backtest/tests.rs"]
mod tests;
