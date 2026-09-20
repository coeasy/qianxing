//! 基于 L1/L2 盘口快照的确定性回测入口。
//!
//! 因果顺序固定为：先撮合上一时刻订单，再让策略看到当前盘口并提交新订单；
//! 因此策略不会用当前快照直接成交，避免回测中的同一快照作弊。

use qx_core::{
    Event, EventKind, EventLog, Fill, Fnv1a, Ledger, Order, OrderStatus, Price, Priority,
    ReplayVerifier, RunManifest, TradingInstrumentSpec,
};
use qx_risk::OrderRiskPosition;
use qx_zhenlu::{Oms, RiskGate};

use crate::{
    DataTier, OrderBookExecutionModel, OrderBookMatchingEngine, OrderBookSnapshot,
    RunManifestIdentity,
};
use std::collections::BTreeMap;

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

/// 只接受 Bar 事件的策略在 L1/L2 深度流上的视图。
///
/// 每份盘口快照按中间价折叠成一根 Bar（open=high=low=close），可见流动性
/// 汇总为成交量；这样只认 Bar 的内置策略无需第二套实现就能复用逐档撮合内核。
pub struct DepthBarStrategy<S: qx_strategy::Strategy> {
    pub strategy: S,
    pub context: qx_strategy::StrategyContext,
    /// Tick 事件不携带标的，因此策略视图必须显式绑定标的。
    pub instrument: qx_core::InstrumentId,
}

impl<S: qx_strategy::Strategy> DepthBarStrategy<S> {
    pub fn new(
        strategy: S,
        context: qx_strategy::StrategyContext,
        instrument: qx_core::InstrumentId,
    ) -> Self {
        Self {
            strategy,
            context,
            instrument,
        }
    }

    pub fn initialize(&mut self) -> Result<(), String> {
        self.strategy.on_init(&self.context)
    }

    pub(crate) fn on_mid(
        &mut self,
        instrument: &qx_core::InstrumentId,
        ts: u64,
        position: i128,
        mid: qx_core::Price,
        volume_raw: i128,
    ) -> Result<Vec<Order>, String> {
        self.context.as_of = ts;
        self.context
            .positions
            .insert(instrument.to_string(), position);
        let mid_raw = mid.raw();
        let event = qx_strategy::MarketEvent::Bar {
            instrument: instrument.clone(),
            ts,
            open_raw: mid_raw,
            high_raw: mid_raw,
            low_raw: mid_raw,
            close_raw: mid_raw,
            volume_raw,
        };
        let decision = self.strategy.on_event(&self.context, &event)?;
        decision.to_orders(&self.context)
    }
}

impl<S: qx_strategy::Strategy> OrderBookStrategy for DepthBarStrategy<S> {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
    ) -> Result<Vec<Order>, String> {
        let (Some(bid), Some(ask)) = (snapshot.bids.first(), snapshot.asks.first()) else {
            return Ok(Vec::new());
        };
        let mid = Price::from_raw(bid.price.raw().saturating_add(ask.price.raw()) / 2);
        let volume_raw = snapshot
            .bids
            .iter()
            .chain(snapshot.asks.iter())
            .fold(0_i128, |total, level| total.saturating_add(level.qty.raw()));
        self.on_mid(&snapshot.instrument, snapshot.ts, position, mid, volume_raw)
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

impl OrderBookBacktestReport {
    pub fn result_hash(&self) -> u64 {
        self.event_log.digest()
    }

    pub fn replay_hash(&self) -> u64 {
        ReplayVerifier::rebuild_from(self.event_log.events())
    }

    pub fn final_equity(&self) -> i128 {
        *self.equity.last().unwrap_or(&self.initial_equity_raw)
    }

    /// 用报告事实生成与 Bar 回测同构的运行指纹，保证两条链路共享 RunManifest 契约。
    pub fn run_manifest(
        &self,
        identity: RunManifestIdentity<'_>,
        data_fingerprint: &str,
    ) -> Result<RunManifest, String> {
        if data_fingerprint.trim().is_empty() {
            return Err("RunManifest data_fingerprint 不能为空".into());
        }
        let mut model_hash = Fnv1a::new();
        for descriptor in &self.model_descriptors {
            model_hash.write_text(descriptor);
        }
        let manifest = RunManifest {
            run_id: identity.run_id.into(),
            code_commit: identity.code_commit.into(),
            config_hash: identity.config_hash.into(),
            data_fingerprint: data_fingerprint.into(),
            input_components: BTreeMap::new(),
            clock_start: self.clock_start,
            clock_end: self.clock_end,
            global_seed: 0,
            determinism_mode: true,
            result_hash: format!("{:016x}", self.result_hash()),
            strategy_version: identity.strategy_version.into(),
            instrument_spec_version: identity.instrument_spec_version.into(),
            model_fingerprint: format!("{:016x}", model_hash.finish()),
            input_event_hash: format!("{:016x}", self.input_data_hash),
            output_event_hash: format!("{:016x}", self.result_hash()),
            runtime_version: identity.runtime_version.into(),
        };
        manifest.validate()?;
        Ok(manifest)
    }
}

fn book_notional(
    spec: Option<&TradingInstrumentSpec>,
    qty: i128,
    price: i128,
) -> Result<i128, qx_core::QxError> {
    match spec {
        Some(spec) => spec.notional(qty, price),
        None => Ok(crate::cost::notional(qty, price)),
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
            data_tier,
        } = config;
        let risk_rule_set_version = risk.rule_set().version().to_string();
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
        if matches!(data_tier, DataTier::Bar) {
            return Err(qx_core::QxError::BusinessViolation(
                "订单簿回测需要盘口快照档位（L1 或 L2/L3），不能声明为 Bar".into(),
            ));
        }
        let mut input_hash = Fnv1a::new();
        input_hash.write_text(&instrument.to_string());
        input_hash.write_u64(snapshots.len() as u64);
        for snapshot in snapshots {
            input_hash.write_u64(snapshot.ts);
            input_hash.write_u64(snapshot.sequence);
            for level in snapshot.bids.iter().chain(snapshot.asks.iter()) {
                input_hash.write_i128(level.price.raw());
                input_hash.write_i128(level.qty.raw());
            }
        }
        let input_data_hash = input_hash.finish();
        let model_descriptors = vec![
            format!("data_tier={data_tier:?}"),
            format!("orderbook-matching@v1 fee_bps={fee_bps}"),
            format!("risk_rule_set={risk_rule_set_version}"),
            match instrument_spec.as_ref() {
                Some(spec) => format!("ledger=spec:{:?}", spec.product),
                None => "ledger=cash-spot".to_string(),
            },
        ];
        let mut matcher = match execution_model {
            Some(model) => OrderBookMatchingEngine::with_model(model),
            None => OrderBookMatchingEngine::new(fee_bps),
        }
        .map_err(qx_core::QxError::BusinessViolation)?;
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
                fees_raw = fees_raw.saturating_add(fill.fee.raw());
                turnover_raw = turnover_raw.saturating_add(book_notional(
                    instrument_spec.as_ref(),
                    fill.qty.raw(),
                    fill.price.raw(),
                )?);
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
                    OrderRiskPosition::new_with_multiplier(
                        one_way_qty,
                        gross_notional,
                        spec.contract_size,
                    )
                    .with_hedge_legs(long_qty, short_qty)
                } else {
                    OrderRiskPosition::new(one_way_qty, 0).with_hedge_legs(long_qty, short_qty)
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
            let marked_equity = match reference_price {
                Some(price) => {
                    let marks = BTreeMap::from([(instrument.clone(), price)]);
                    if let Some(spec) = instrument_spec
                        .as_ref()
                        .filter(|spec| spec.product.supports_leverage())
                    {
                        ledger.equity_for_with_spec(&account_id, &marks, &currency, spec)?
                    } else {
                        ledger
                            .equity_for_with_multiplier(&account_id, &marks, &currency, 1)
                            .ok_or_else(|| {
                                qx_core::QxError::Invariant("订单簿回测无法计算账户权益".into())
                            })?
                    }
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
        Ok(OrderBookBacktestReport {
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
            risk: RiskGate::conservative_default(),
            data_tier: DataTier::L2L3,
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
            risk: RiskGate::conservative_default(),
            data_tier: DataTier::L2L3,
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
            risk: RiskGate::conservative_default(),
            data_tier: DataTier::L2L3,
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

    fn l2_config(fee_bps: i128) -> OrderBookBacktestConfig {
        OrderBookBacktestConfig {
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            account_id: "main".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_i64(10_000),
            fee_bps,
            instrument_spec: None,
            risk: RiskGate::conservative_default(),
            data_tier: DataTier::L2L3,
        }
    }

    fn book_context() -> qx_strategy::StrategyContext {
        qx_strategy::StrategyContext {
            strategy_id: "depth-book".into(),
            strategy_version: "v1".into(),
            account_id: "main".into(),
            venue_id: "paper".into(),
            data_fingerprint: "book-1".into(),
            as_of: 1,
            positions: std::collections::BTreeMap::new(),
            cash: std::collections::BTreeMap::new(),
            available_margin_raw: Some(10_000),
            risk_state: "ready".into(),
        }
    }

    #[test]
    fn order_book_report_is_deterministic_and_carries_unified_metrics() {
        let snapshots = [snapshot(1, 100), snapshot(2, 101), snapshot(3, 102)];
        let first = OrderBookBacktestEngine::new(l2_config(1))
            .run(&snapshots, &mut BuyOnce { emitted: false })
            .unwrap();
        let second = OrderBookBacktestEngine::new(l2_config(1))
            .run(&snapshots, &mut BuyOnce { emitted: false })
            .unwrap();
        assert_eq!(first.result_hash(), second.result_hash());
        assert_eq!(first.equity, second.equity);
        assert_eq!(first.snapshot_ts, vec![1, 2, 3]);
        assert_eq!(first.positions.len(), first.equity.len());
        assert_eq!(
            first.fees_raw,
            first.fills.iter().map(|fill| fill.fee.raw()).sum::<i128>()
        );
        assert!(first.turnover_raw > 0);
        assert_eq!(first.clock_start, 1);
        assert_eq!(first.clock_end, 3);
        assert_eq!(first.final_equity(), *first.equity.last().unwrap());
        assert!(first
            .model_descriptors
            .iter()
            .any(|descriptor| descriptor == "data_tier=L2L3"));
        let manifest = first
            .run_manifest(
                RunManifestIdentity {
                    run_id: "book-test:run",
                    code_commit: "workspace",
                    config_hash: "config-hash",
                    strategy_version: "v1",
                    instrument_spec_version: "default-instrument-spec-v1",
                    runtime_version: "runtime-schema-1",
                },
                "depth:test",
            )
            .unwrap();
        assert_eq!(manifest.clock_start, 1);
        assert_eq!(manifest.clock_end, 3);
        assert_eq!(
            manifest.input_event_hash,
            format!("{:016x}", first.input_data_hash)
        );
        assert_eq!(
            manifest.result_hash,
            format!("{:016x}", first.result_hash())
        );
        let changed = OrderBookBacktestEngine::new(l2_config(50))
            .run(&snapshots, &mut BuyOnce { emitted: false })
            .unwrap();
        assert_ne!(changed.result_hash(), first.result_hash());
        assert!(changed.fees_raw > first.fees_raw);
    }

    #[test]
    fn depth_bar_view_lets_bar_only_strategies_trade_on_order_book_snapshots() {
        let mut strategy =
            DepthBarStrategy::new(NativeBuy, book_context(), snapshot(1, 100).instrument);
        let report = OrderBookBacktestEngine::new(l2_config(0))
            .run(&[snapshot(1, 100), snapshot(2, 101)], &mut strategy)
            .unwrap();
        assert_eq!(report.fills.len(), 1);
        assert_eq!(report.pending_orders, 1);
        assert_eq!(report.positions, vec![0, Quantity::from_i64(1).raw()]);
    }

    #[test]
    fn order_book_backtest_rejects_bar_data_tier() {
        let config = OrderBookBacktestConfig {
            data_tier: DataTier::Bar,
            ..l2_config(0)
        };
        let error = OrderBookBacktestEngine::new(config)
            .run(&[snapshot(1, 100)], &mut BuyOnce { emitted: false })
            .err()
            .expect("Bar 档位不能用于盘口回测");
        assert!(format!("{error:?}").contains("不能声明为 Bar"));
    }
}
