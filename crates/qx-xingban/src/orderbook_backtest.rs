//! 基于 L1/L2 盘口快照的确定性回测入口。
//!
//! 因果顺序固定为：先撮合上一时刻订单，再让策略看到当前盘口并提交新订单；
//! 因此策略不会用当前快照直接成交，避免回测中的同一快照作弊。

use qx_core::{
    Event, EventKind, EventLog, Fill, Fnv1a, Ledger, OrderStatus, Price, Priority, ReplayVerifier,
    TradingInstrumentSpec, ValuationContext,
};
use qx_risk::OrderRiskPosition;
use qx_zhenlu::{Oms, RiskGate};

use crate::orderbook_strategies::OrderBookStrategy;
use crate::{DataTier, OrderBookExecutionModel, OrderBookMatchingEngine, OrderBookSnapshot};
use std::collections::BTreeMap;

mod funding;
mod margin;
mod portfolio;
mod report;
use funding::{FillFundingContext, FundingContext, FundingDecision, SpotCashReservations};
pub use portfolio::{
    OrderBookPortfolioBacktestEngine, OrderBookPortfolioBacktestReport, OrderBookPortfolioMarket,
};

fn append_event(log: &mut EventLog, ts: u64, priority: u8, kind: EventKind) {
    let seq = log.alloc_seq();
    log.append(Event::new(seq, ts, priority, kind));
}

pub struct OrderBookBacktestConfig {
    pub instrument: qx_core::InstrumentId,
    pub account_id: String,
    pub currency: String,
    pub initial_cash: qx_core::Money,
    pub fee_bps: i128,
    /// 可选产品规格。配置衍生品后，订单必须显式携带 OrderPolicy，成交和
    /// 未平仓头寸使用 `Ledger::apply_fill_with_spec`，不能退化为现货现金语义。
    pub instrument_spec: Option<TradingInstrumentSpec>,
    pub risk: RiskGate,
    /// 输入深度档位声明；L2 以上才有逐档撮合意义，L1 由 Tick 引擎固定。
    pub data_tier: DataTier,
}

pub struct OrderBookBacktestReport {
    pub fills: Vec<Fill>,
    pub ledger: Ledger,
    pub event_log: EventLog,
    pub pending_orders: usize,
    /// 与 Bar 回测同形的统一产物：每个快照结束后的账户权益、净持仓与时间。
    pub equity: Vec<i128>,
    pub positions: Vec<i128>,
    pub snapshot_ts: Vec<u64>,
    pub fees_raw: i128,
    pub turnover_raw: i128,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub initial_equity_raw: i128,
    pub input_data_hash: u64,
    pub clock_start: u64,
    pub clock_end: u64,
    pub data_tier: DataTier,
    pub risk_rule_set_version: String,
    pub model_descriptors: Vec<String>,
    pub assumptions: Vec<String>,
}

fn book_notional(
    spec: Option<&TradingInstrumentSpec>,
    qty: i128,
    price: i128,
) -> Result<i128, qx_core::QxError> {
    match spec {
        Some(spec) => spec.notional(qty, price),
        None => Ok(qx_core::fee::notional(qty, price)),
    }
}

pub struct OrderBookBacktestEngine {
    config: OrderBookBacktestConfig,
    execution_model: Option<OrderBookExecutionModel>,
}

impl OrderBookBacktestEngine {
    pub fn new(config: OrderBookBacktestConfig) -> Self {
        Self {
            config,
            execution_model: None,
        }
    }

    pub fn with_execution_model(mut self, model: OrderBookExecutionModel) -> Self {
        self.execution_model = Some(model);
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

    /// Run the same deterministic engine with cooperative cancellation checks
    /// during input validation and at every snapshot boundary. `None` is a
    /// cancellation result; partial books are never returned as a valid report.
    pub fn run_with_cancel<F>(
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
                return Err(qx_core::QxError::BusinessViolation(
                    format!(
                        "订单簿回测账户币种 {currency} 与 {:?} 市场现金/结算币种 {cash_currency} 不一致",
                        spec.product,
                    ),
                ));
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
        if matches!(data_tier, DataTier::Bar) {
            return Err(qx_core::QxError::BusinessViolation(
                "订单簿回测需要盘口快照档位（L1 或 L2/L3），不能声明为 Bar".into(),
            ));
        }
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
                input_hash.write_i128(level.price.raw());
                input_hash.write_i128(level.qty.raw());
            }
        }
        let input_data_hash = input_hash.finish();
        // 执行模型的三个高保真参数必须出现在产物里：只把它们喂进撮合器而不写进
        // model_descriptors，就等于"能配但看不出来配了什么"。
        let effective_model = match execution_model {
            Some(model) => model,
            None => OrderBookExecutionModel::new(fee_bps)
                .map_err(qx_core::QxError::BusinessViolation)?,
        };
        let model_descriptors = vec![
            format!("data_tier={data_tier:?}"),
            format!(
                "orderbook-matching@v1 fee_bps={} latency_snapshots={} queue_position_bps={} market_impact_bps={}",
                effective_model.fee_bps,
                effective_model.latency_snapshots,
                effective_model.queue_position_bps,
                effective_model.market_impact_bps
            ),
            format!("risk_rule_set={risk_rule_set_version}"),
            match instrument_spec.as_ref() {
                Some(spec) => format!("ledger=spec:{:?}", spec.product),
                None => "ledger=cash-spot".to_string(),
            },
        ];
        let mut matcher = OrderBookMatchingEngine::with_model(effective_model)
            .map_err(qx_core::QxError::BusinessViolation)?
            .with_fee_spec(instrument_spec.clone());
        let mut spot_cash_reservations = SpotCashReservations::default();
        let mut ledger = Ledger::new();
        let mut log = EventLog::new();
        let mut oms = Oms::new();
        let mut fills = Vec::new();
        let mut equity_curve = Vec::with_capacity(snapshots.len());
        let mut position_curve = Vec::with_capacity(snapshots.len());
        let mut snapshot_curve = Vec::with_capacity(snapshots.len());
        let mut fees_raw = 0_i128;
        let mut turnover_raw = 0_i128;
        let mut peak_equity = initial_cash.raw();
        let mut max_drawdown_raw = 0_i128;
        let deposit_id = ledger.deposit(
            &account_id,
            &currency,
            initial_cash,
            snapshots.first().map(|snapshot| snapshot.ts).unwrap_or(1),
        )?;
        let deposit = ledger
            .entries()
            .iter()
            .find(|entry| entry.id == deposit_id)
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
                spot_cash_reservations.commit_fill(reservation_update);
                fees_raw = fees_raw.saturating_add(fill.fee.raw());
                turnover_raw = turnover_raw.saturating_add(book_notional(
                    instrument_spec.as_ref(),
                    fill.qty.raw(),
                    fill.price.raw(),
                )?);
                append_event(
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
                    let entry = ledger
                        .entries()
                        .iter()
                        .find(|entry| entry.id == entry_id)
                        .cloned()
                        .ok_or_else(|| {
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
                let cash = ledger.cash_balances_for(&account_id);
                let margin = margin::settled_cash_margin(
                    instrument_spec
                        .as_ref()
                        .is_some_and(|spec| spec.product.supports_leverage()),
                    ledger.cash_for(&account_id, &currency),
                );
                strategy.update_account_state(&cash, margin);
                strategy.on_fill(&order, &fill)?;
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
            let available_margin_raw = margin::settled_cash_margin(
                instrument_spec
                    .as_ref()
                    .is_some_and(|spec| spec.product.supports_leverage()),
                ledger.cash_for(&account_id, &currency),
            );
            strategy
                .update_account_state(&ledger.cash_balances_for(&account_id), available_margin_raw);
            let reference_price = match (
                snapshot.bids.first().map(|level| level.price),
                snapshot.asks.first().map(|level| level.price),
            ) {
                (Some(bid), Some(ask)) => {
                    Some(Price::from_raw(bid.raw().saturating_add(ask.raw()) / 2))
                }
                (Some(bid), None) => Some(bid),
                (None, Some(ask)) => Some(ask),
                (None, None) => None,
            };
            let open_order_ids = oms.active_order_ids();
            let open_order_set = open_order_ids
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            let actions = strategy
                .on_order_book_actions_with_margin(
                    snapshot,
                    position,
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
                    if let Err(error) = policy.validate_for(spec).and_then(|_| {
                        spec.validate_order(order.qty.raw(), order.limit.map(|price| price.raw()))
                    }) {
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
                let long_qty = ledger
                    .position_for_side(&account_id, &instrument, qx_core::PositionSide::Long)
                    .quantity
                    .raw();
                let short_qty = ledger
                    .position_for_side(&account_id, &instrument, qx_core::PositionSide::Short)
                    .quantity
                    .raw();
                let one_way_qty = position
                    .checked_sub(long_qty)
                    .and_then(|value| value.checked_sub(short_qty))
                    .ok_or_else(|| {
                        qx_core::QxError::Invariant("拆分订单簿 one-way/hedge 持仓数量溢出".into())
                    })?;
                let position_snapshot = if let Some(spec) = instrument_spec.as_ref() {
                    let gross_notional = if let Some(price) = reference_price {
                        let one_way = spec.notional(one_way_qty.saturating_abs(), price.raw())?;
                        let long = spec.notional(long_qty.saturating_abs(), price.raw())?;
                        let short = spec.notional(short_qty.saturating_abs(), price.raw())?;
                        one_way
                            .checked_add(long)
                            .and_then(|value| value.checked_add(short))
                            .ok_or_else(|| {
                                qx_core::QxError::Invariant(
                                    "订单簿 hedge gross notional 溢出".into(),
                                )
                            })?
                    } else {
                        0
                    };
                    OrderRiskPosition::new_with_multiplier(
                        one_way_qty,
                        gross_notional,
                        spec.contract_size,
                    )
                    .with_hedge_legs(long_qty, short_qty)
                } else {
                    OrderRiskPosition::new(one_way_qty, 0).with_hedge_legs(long_qty, short_qty)
                };
                if let Err(error) = risk.check_with_spec(
                    &order,
                    &position_snapshot,
                    reference_price,
                    instrument_spec.as_ref(),
                ) {
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
                let accepted_reservation = match funding::preflight_order(
                    &order,
                    policy,
                    instrument_spec.as_ref(),
                    FundingContext {
                        snapshot,
                        reference_price,
                        ledger: &ledger,
                        account_id: &account_id,
                        currency: &currency,
                        fee_bps,
                        reservations: &spot_cash_reservations,
                        total_reserved_cash_raw: None,
                    },
                )? {
                    FundingDecision::Accept {
                        reserve_cash_raw,
                        reserve_inventory_raw,
                    } => (reserve_cash_raw, reserve_inventory_raw),
                    FundingDecision::NotRequired => (0, 0),
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
                let budget_price = order.limit.or(reference_price);
                if let Some(price) = budget_price {
                    if let Err(error) =
                        strategy.check_order_budget(&order, price.raw(), instrument_spec.as_ref())
                    {
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
                order.status = OrderStatus::Submitted;
                matcher
                    .submit(order.clone())
                    .map_err(qx_core::QxError::BusinessViolation)?;
                oms.submit(order.clone())?;
                oms.accept(order.client_id)?;
                strategy.on_order_accepted(&order, budget_price.map(|price| price.raw()))?;
                if accepted_reservation.0 > 0 || accepted_reservation.1 > 0 {
                    spot_cash_reservations.reserve(
                        order.client_id,
                        order.qty.raw(),
                        accepted_reservation.0,
                        accepted_reservation.1,
                    )?;
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
            let marked_equity = match reference_price {
                Some(price) => {
                    let marks = BTreeMap::from([(instrument.clone(), price)]);
                    let spec = instrument_spec
                        .as_ref()
                        .filter(|spec| spec.product.supports_leverage());
                    let context =
                        ValuationContext::spot(&account_id, &marks, &currency).with_spec(spec);
                    ledger.valuate(&context)?.equity.raw()
                }
                None => ledger.cash_for(&account_id, &currency),
            };
            peak_equity = peak_equity.max(marked_equity);
            max_drawdown_raw = max_drawdown_raw.max(peak_equity.saturating_sub(marked_equity));
            equity_curve.push(marked_equity);
            position_curve.push(position);
            snapshot_curve.push(snapshot.ts);
        }
        let final_equity = equity_curve.last().copied().unwrap_or(initial_cash.raw());
        let return_bps = if initial_cash.raw() > 0 {
            final_equity
                .saturating_sub(initial_cash.raw())
                .saturating_mul(10_000)
                .checked_div(initial_cash.raw())
                .unwrap_or(0)
                .clamp(i128::from(i32::MIN), i128::from(i32::MAX)) as i32
        } else {
            0
        };
        let max_drawdown_bps = if peak_equity > 0 {
            max_drawdown_raw
                .saturating_mul(10_000)
                .checked_div(peak_equity)
                .unwrap_or(0)
                .clamp(0, 10_000) as u32
        } else {
            0
        };
        let report = OrderBookBacktestReport {
            fills,
            ledger,
            event_log: log,
            pending_orders: matcher.pending_count(),
            equity: equity_curve,
            positions: position_curve,
            snapshot_ts: snapshot_curve,
            fees_raw,
            turnover_raw,
            return_bps,
            max_drawdown_bps,
            initial_equity_raw: initial_cash.raw(),
            input_data_hash,
            clock_start: snapshots.first().map(|snapshot| snapshot.ts).unwrap_or(0),
            clock_end: snapshots.last().map(|snapshot| snapshot.ts).unwrap_or(0),
            data_tier,
            risk_rule_set_version,
            assumptions: vec![
                format!("data_tier={data_tier:?}"),
                "benchmark=flat-cash".into(),
                "metrics=raw-fixed-point".into(),
                "causality=match-previous-submit-current".into(),
                format!("pending_orders={}", matcher.pending_count()),
            ],
            model_descriptors,
        };
        // Facts must replay into the same ledger before this report is returned (V11 Q62).
        ReplayVerifier::verify(report.event_log.events(), &report.ledger)?;
        Ok(Some(report))
    }
}

#[cfg(test)]
#[path = "orderbook_backtest/tests.rs"]
mod tests;
