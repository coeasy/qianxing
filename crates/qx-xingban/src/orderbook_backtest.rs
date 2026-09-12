//! 基于 L1/L2 盘口快照的确定性回测入口。
//!
//! 因果顺序固定为：先撮合上一时刻订单，再让策略看到当前盘口并提交新订单；
//! 因此策略不会用当前快照直接成交，避免回测中的同一快照作弊。

use qx_core::{
    Event, EventKind, EventLog, Fill, Ledger, Order, OrderStatus, Price, Priority, ReplayVerifier,
    TradingInstrumentSpec,
};
use qx_zhenlu::{Oms, PositionSnapshot, RiskGate};

use crate::{OrderBookExecutionModel, OrderBookMatchingEngine, OrderBookSnapshot};

fn append_event(log: &mut EventLog, ts: u64, priority: u8, kind: EventKind) {
    let seq = log.alloc_seq();
    log.append(Event::new(seq, ts, priority, kind));
}

pub trait OrderBookStrategy {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
    ) -> Result<Vec<Order>, String>;
}

/// 把统一 Rust 策略协议适配到订单簿回测。策略仍然只返回决定，订单转换和
/// 风控/OMS 处理继续由回测引擎负责。
pub struct NativeOrderBookStrategy<S: qx_strategy::Strategy> {
    pub strategy: S,
    pub context: qx_strategy::StrategyContext,
}

impl<S: qx_strategy::Strategy> NativeOrderBookStrategy<S> {
    pub fn new(strategy: S, context: qx_strategy::StrategyContext) -> Self {
        Self { strategy, context }
    }
}

impl<S: qx_strategy::Strategy> OrderBookStrategy for NativeOrderBookStrategy<S> {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
    ) -> Result<Vec<Order>, String> {
        self.context.as_of = snapshot.ts;
        self.context
            .positions
            .insert(snapshot.instrument.to_string(), position);
        let event = qx_strategy::MarketEvent::OrderBook {
            instrument: snapshot.instrument.clone(),
            ts: snapshot.ts,
            sequence: snapshot.sequence,
            bids: snapshot
                .bids
                .iter()
                .map(|level| qx_strategy::OrderBookLevel {
                    price_raw: level.price.raw(),
                    qty_raw: level.qty.raw(),
                })
                .collect(),
            asks: snapshot
                .asks
                .iter()
                .map(|level| qx_strategy::OrderBookLevel {
                    price_raw: level.price.raw(),
                    qty_raw: level.qty.raw(),
                })
                .collect(),
        };
        let decision = self.strategy.on_event(&self.context, &event)?;
        decision.to_orders(&self.context)
    }
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
}

pub struct OrderBookBacktestReport {
    pub fills: Vec<Fill>,
    pub ledger: Ledger,
    pub event_log: EventLog,
    pub pending_orders: usize,
}

impl OrderBookBacktestReport {
    pub fn result_hash(&self) -> u64 {
        self.event_log.digest()
    }

    pub fn replay_hash(&self) -> u64 {
        ReplayVerifier::rebuild_from(self.event_log.events())
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
        } = config;
        if account_id.trim().is_empty() || currency.trim().is_empty() {
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
            if spec.settlement_currency != currency {
                return Err(qx_core::QxError::BusinessViolation(
                    "订单簿回测产品规格结算币种与配置不一致".into(),
                ));
            }
        }
        for snapshot in snapshots {
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
        let mut matcher = match execution_model {
            Some(model) => OrderBookMatchingEngine::with_model(model),
            None => OrderBookMatchingEngine::new(fee_bps),
        }
        .map_err(qx_core::QxError::BusinessViolation)?;
        let mut ledger = Ledger::new();
        let mut log = EventLog::new();
        let mut oms = Oms::new();
        let mut fills = Vec::new();
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
            Priority::APPLY,
            EventKind::LedgerApplied { entry: deposit },
        );

        for snapshot in snapshots {
            for mut fill in matcher
                .on_snapshot(snapshot)
                .map_err(qx_core::QxError::Permanent)?
            {
                let order = oms.get(fill.order_id).cloned().ok_or_else(|| {
                    qx_core::QxError::Invariant("订单簿成交找不到 OMS 订单".into())
                })?;
                let mut traced_fill = fill.clone();
                order.trace_fill(&mut traced_fill, None, None);
                oms.apply_fill(&traced_fill)?;
                let entry_ids = if let Some(spec) = instrument_spec.as_ref() {
                    ledger.apply_fill_with_spec(&order, &traced_fill, &currency, spec)?
                } else {
                    ledger.apply_fill_with_multiplier(&order, &traced_fill, &currency, 1)?
                };
                fill = traced_fill;
                append_event(
                    &mut log,
                    fill.ts,
                    Priority::APPLY,
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
                        Priority::APPLY,
                        EventKind::LedgerApplied { entry },
                    );
                }
                fills.push(fill);
            }

            let position = ledger.position_for(&account_id, &instrument).quantity.raw();
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
            for mut order in strategy
                .on_order_book(snapshot, position)
                .map_err(qx_core::QxError::BusinessViolation)?
            {
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
                    if !policy.reduce_only {
                        let price = order.limit.or(reference_price).ok_or_else(|| {
                            qx_core::QxError::BusinessViolation("订单簿订单缺少保证金参考价".into())
                        })?;
                        let equity = if spec.product.supports_leverage() {
                            let marks =
                                std::collections::BTreeMap::from([(instrument.clone(), price)]);
                            ledger.equity_for_with_spec(&account_id, &marks, &currency, spec)?
                        } else {
                            ledger.cash_for(&account_id, &currency)
                        };
                        let required =
                            spec.initial_margin(order.qty.raw(), price.raw(), policy.leverage)?;
                        if required > equity {
                            append_event(
                                &mut log,
                                snapshot.ts,
                                Priority::COMMAND,
                                EventKind::Rejected {
                                    client_order_id: order.client_id,
                                    reason: "订单初始保证金超过订单簿回测权益".into(),
                                },
                            );
                            continue;
                        }
                    }
                }
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
                    PositionSnapshot::new_with_multiplier(
                        one_way_qty,
                        gross_notional,
                        spec.contract_size,
                    )
                    .with_hedge_legs(long_qty, short_qty)
                } else {
                    PositionSnapshot::new(one_way_qty, 0).with_hedge_legs(long_qty, short_qty)
                };
                if let Err(error) =
                    risk.check_with_price(&order, &position_snapshot, reference_price)
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
                order.status = OrderStatus::Submitted;
                matcher
                    .submit(order.clone())
                    .map_err(qx_core::QxError::BusinessViolation)?;
                oms.submit(order.clone())?;
                oms.accept(order.client_id)?;
                append_event(
                    &mut log,
                    snapshot.ts,
                    Priority::COMMAND,
                    EventKind::OrderSubmitted {
                        order: order.clone(),
                    },
                );
                append_event(
                    &mut log,
                    snapshot.ts,
                    Priority::MATCH,
                    EventKind::Accepted {
                        client_order_id: order.client_id,
                        venue_order_id: None,
                    },
                );
            }
        }
        Ok(OrderBookBacktestReport {
            fills,
            ledger,
            event_log: log,
            pending_orders: matcher.pending_count(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{
        InstrumentId, MarginMode, Money, Order, OrderPolicy, OrderStatus, PositionMode,
        PositionSide, Quantity, Side, TradingInstrumentSpec, TradingProduct, SCALE,
    };
    use qx_strategy::{
        Strategy, StrategyContext, StrategyDecision, StrategyOrderIntent, STRATEGY_API_VERSION,
    };

    struct BuyOnce {
        emitted: bool,
    }

    impl OrderBookStrategy for BuyOnce {
        fn on_order_book(
            &mut self,
            snapshot: &OrderBookSnapshot,
            _position: i128,
        ) -> Result<Vec<Order>, String> {
            if self.emitted {
                return Ok(Vec::new());
            }
            self.emitted = true;
            Ok(vec![Order {
                client_id: 1,
                instrument: snapshot.instrument.clone(),
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: None,
            }])
        }
    }

    struct NativeBuy;

    impl Strategy for NativeBuy {
        fn on_event(
            &mut self,
            context: &StrategyContext,
            event: &qx_strategy::MarketEvent,
        ) -> Result<StrategyDecision, String> {
            let instrument = event
                .instrument()
                .cloned()
                .ok_or_else(|| "订单簿事件缺少标的".to_string())?;
            Ok(StrategyDecision {
                schema_version: STRATEGY_API_VERSION,
                request_id: format!("native:{}", event.ts()),
                strategy_id: context.strategy_id.clone(),
                signal_id: event.ts(),
                confidence: 1,
                priority: 0,
                expires_at: event.ts(),
                intents: vec![StrategyOrderIntent {
                    intent_id: event.ts(),
                    instrument,
                    side: Side::Buy,
                    qty: Quantity::from_i64(1),
                    limit: None,
                    policy: None,
                    reduce_only: false,
                    post_only: false,
                }],
            })
        }
    }

    struct DerivativeBuy {
        emitted: bool,
    }

    impl OrderBookStrategy for DerivativeBuy {
        fn on_order_book(
            &mut self,
            snapshot: &OrderBookSnapshot,
            _position: i128,
        ) -> Result<Vec<Order>, String> {
            if self.emitted {
                return Ok(Vec::new());
            }
            self.emitted = true;
            Ok(vec![Order {
                client_id: 99,
                instrument: snapshot.instrument.clone(),
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: Some(OrderPolicy {
                    margin_mode: MarginMode::Cross,
                    position_mode: PositionMode::OneWay,
                    position_side: PositionSide::Net,
                    leverage: 10,
                    ..OrderPolicy::default()
                }),
            }])
        }
    }

    fn snapshot(ts: u64, ask: i64) -> OrderBookSnapshot {
        OrderBookSnapshot {
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            ts,
            sequence: ts,
            bids: vec![crate::BookLevel {
                price: Price::from_i64(99),
                qty: Quantity::from_i64(2),
            }],
            asks: vec![crate::BookLevel {
                price: Price::from_i64(ask),
                qty: Quantity::from_i64(2),
            }],
        }
    }

    #[test]
    fn order_book_backtest_is_causal_and_replays_fill() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let config = OrderBookBacktestConfig {
            instrument,
            account_id: "main".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_i64(10_000),
            fee_bps: 1,
            instrument_spec: None,
            risk: RiskGate::new(),
        };
        let mut strategy = BuyOnce { emitted: false };
        let report = OrderBookBacktestEngine::new(config)
            .run(&[snapshot(1, 100), snapshot(2, 101)], &mut strategy)
            .unwrap();
        assert_eq!(report.fills.len(), 1);
        assert_eq!(report.fills[0].ts, 2);
        assert_eq!(
            report.replay_hash(),
            ReplayVerifier::rebuild_from(report.event_log.events())
        );
    }

    #[test]
    fn native_strategy_adapter_uses_the_same_order_book_causal_path() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let config = OrderBookBacktestConfig {
            instrument: instrument.clone(),
            account_id: "main".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_i64(10_000),
            fee_bps: 0,
            instrument_spec: None,
            risk: RiskGate::new(),
        };
        let context = StrategyContext {
            strategy_id: "native-book".into(),
            strategy_version: "v1".into(),
            account_id: "main".into(),
            venue_id: "paper".into(),
            data_fingerprint: "book-1".into(),
            as_of: 1,
            positions: std::collections::BTreeMap::new(),
            cash: std::collections::BTreeMap::new(),
            available_margin_raw: Some(10_000),
            risk_state: "ready".into(),
        };
        let mut strategy = NativeOrderBookStrategy::new(NativeBuy, context);
        let report = OrderBookBacktestEngine::new(config)
            .run(&[snapshot(1, 100), snapshot(2, 101)], &mut strategy)
            .unwrap();
        assert_eq!(report.fills.len(), 1);
        assert_eq!(report.fills[0].order_id, 1);
    }

    #[test]
    fn derivative_order_book_uses_margin_ledger_semantics() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let spec = TradingInstrumentSpec {
            instrument: instrument.clone(),
            product: TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 100,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
        };
        let config = OrderBookBacktestConfig {
            instrument,
            account_id: "main".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_i64(10_000),
            fee_bps: 0,
            instrument_spec: Some(spec),
            risk: RiskGate::new(),
        };
        let mut strategy = DerivativeBuy { emitted: false };
        let report = OrderBookBacktestEngine::new(config)
            .run(&[snapshot(1, 100), snapshot(2, 101)], &mut strategy)
            .unwrap();
        assert_eq!(report.fills.len(), 1);
        assert_eq!(
            report.ledger.cash_for("main", "USDT"),
            Money::from_i64(10_000).raw()
        );
        assert_eq!(
            report
                .ledger
                .position_for("main", &InstrumentId::parse("BTCUSDT.BINANCE").unwrap())
                .quantity
                .raw(),
            Quantity::from_i64(1).raw()
        );
    }
}
