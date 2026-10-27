//! Strategy adapters and cancel/replace actions for the shared L2 backtest.

use crate::OrderBookSnapshot;
use qx_core::{DecimalRounding, ExactDecimal, Order, Price};
use qx_strategy::{StrategyErrorStage, StrategyFillUpdateV2, StrategyStopReason, StrategyUpdate};

pub trait OrderBookStrategy {
    /// Called once before the first L1/L2 strategy event.
    fn on_start(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Called after completion, cancellation or a strategy/execution error.
    fn on_stop(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Reason-aware stop hook; legacy implementations continue through `on_stop`.
    fn on_stop_with_reason(&mut self, _reason: StrategyStopReason) -> Result<(), String> {
        self.on_stop()
    }

    fn update_account_state(
        &mut self,
        _cash: &std::collections::BTreeMap<String, i128>,
        _available_margin_raw: Option<i128>,
    ) {
    }

    /// Whether this strategy explicitly supports scale-aware order intents and account state.
    /// Legacy strategies default to false so an exact fill cannot later appear as a zero raw
    /// position or bypass a strategy-owned capital budget.
    fn supports_exact_orders(&self) -> bool {
        false
    }

    fn update_account_state_exact(
        &mut self,
        cash: &std::collections::BTreeMap<String, i128>,
        cash_exact: &std::collections::BTreeMap<String, ExactDecimal>,
        available_margin_raw: Option<i128>,
        available_margin_exact: Option<ExactDecimal>,
    ) {
        let _ = (cash_exact, available_margin_exact);
        self.update_account_state(cash, available_margin_raw);
    }
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
    ) -> Result<Vec<Order>, String>;

    /// Stateful L2 strategies may cancel resting orders before the next snapshot is matched.
    /// Existing order-only strategies inherit this compatibility adapter.
    fn on_order_book_actions(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
        _open_order_ids: &[u64],
    ) -> Result<OrderBookActions, String> {
        Ok(OrderBookActions {
            orders: self.on_order_book(snapshot, position)?,
            cancel_client_ids: Vec::new(),
        })
    }

    /// Decision-time buying power for spot sizing. The engine supplies a value
    /// only when it can derive a supported spot cash snapshot; derivative
    /// margin remains unavailable until a certified free-margin model exists.
    fn on_order_book_actions_with_margin(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
        open_order_ids: &[u64],
        _available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        self.on_order_book_actions(snapshot, position, open_order_ids)
    }

    /// Exact-aware adapter. Implementations that do not override this hook may only receive
    /// values exactly representable by the legacy 1e-9 position projection.
    fn on_order_book_actions_with_exact_position(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position_raw: i128,
        position_exact: ExactDecimal,
        open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        let projected = position_exact
            .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
            .and_then(|value| value.to_raw(9).ok())
            .ok_or_else(|| "订单簿策略持仓不能投影到兼容 1e-9 字段".to_string())?;
        if projected != position_raw {
            return Err("订单簿策略未实现 exact position hook，不能接收子纳米持仓".into());
        }
        self.on_order_book_actions_with_margin(
            snapshot,
            position_raw,
            open_order_ids,
            available_margin_raw,
        )
    }

    /// Strategy portfolios may enforce per-member capital before an order reaches OMS.
    fn check_order_budget(
        &self,
        _order: &Order,
        _reference_price: i128,
        _instrument_spec: Option<&qx_core::TradingInstrumentSpec>,
    ) -> Result<(), qx_core::QxError> {
        Ok(())
    }

    fn check_exact_order_budget(
        &self,
        _order: &Order,
        _reference_price: ExactDecimal,
        _instrument_spec: Option<&qx_core::TradingInstrumentSpec>,
    ) -> Result<(), qx_core::QxError> {
        Ok(())
    }

    /// Validate the actual fill price before the authoritative ledger is updated.
    fn validate_fill_budget(
        &self,
        _order: &Order,
        _fill: &qx_core::Fill,
        _instrument_spec: Option<&qx_core::TradingInstrumentSpec>,
    ) -> Result<(), qx_core::QxError> {
        Ok(())
    }

    fn on_order_accepted(
        &mut self,
        _order: &Order,
        _reference_price: Option<i128>,
    ) -> Result<(), qx_core::QxError> {
        Ok(())
    }

    fn on_exact_order_accepted(
        &mut self,
        order: &Order,
        reference_price: ExactDecimal,
    ) -> Result<(), qx_core::QxError> {
        let raw = reference_price
            .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
            .and_then(|value| value.to_raw(9).ok())
            .ok_or_else(|| qx_core::QxError::Invariant("精确订单参考价投影失败".into()))?;
        self.on_order_accepted(order, Some(raw))
    }

    fn on_order_cancelled(&mut self, _order: &Order) -> Result<(), qx_core::QxError> {
        Ok(())
    }

    /// Receives fills only after OMS and Ledger accepted the fill.
    fn on_fill(&mut self, _order: &Order, _fill: &qx_core::Fill) -> Result<(), qx_core::QxError> {
        Ok(())
    }

    /// Receives the EventLog sequence assigned to the committed fill. Existing
    /// strategies retain the v1 callback through this default adapter.
    fn on_fill_with_event_seq(
        &mut self,
        order: &Order,
        fill: &qx_core::Fill,
        event_seq: u64,
    ) -> Result<(), qx_core::QxError> {
        let _ = event_seq;
        self.on_fill(order, fill)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OrderBookActions {
    pub orders: Vec<Order>,
    pub cancel_client_ids: Vec<u64>,
}

/// Adapts the shared Rust strategy protocol to full order-book events.
pub struct NativeOrderBookStrategy<S: qx_strategy::Strategy> {
    pub strategy: S,
    pub context: qx_strategy::StrategyContext,
    data_tier: crate::DataTier,
    initialized: bool,
    history_requirements: Option<qx_strategy::StrategyHistoryRequirements>,
    zero_warmup_notified: bool,
    pending_order_updates: Vec<Order>,
}

impl<S: qx_strategy::Strategy> NativeOrderBookStrategy<S> {
    pub fn new(strategy: S, context: qx_strategy::StrategyContext) -> Self {
        Self {
            strategy,
            context,
            data_tier: crate::DataTier::L2L3,
            initialized: false,
            history_requirements: None,
            zero_warmup_notified: false,
            pending_order_updates: Vec::new(),
        }
    }

    /// Emit the L1 quote contract while retaining the shared order-book matcher.
    /// L1 callers must supply exactly one bid and one ask on every snapshot.
    pub fn with_data_tier(mut self, data_tier: crate::DataTier) -> Self {
        self.data_tier = data_tier;
        self
    }

    pub fn initialize(&mut self) -> Result<(), String> {
        if self.initialized {
            return Ok(());
        }
        let init = self.strategy.on_init(&self.context);
        qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::Initialization,
            Some(self.context.as_of),
            None,
            init,
        )?;
        let requirements = self.strategy.history_requirements();
        self.history_requirements = Some(qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::HistoryRequirements,
            Some(self.context.as_of),
            None,
            requirements,
        )?);
        self.initialized = true;
        Ok(())
    }
}

impl<S: qx_strategy::Strategy> OrderBookStrategy for NativeOrderBookStrategy<S> {
    fn on_start(&mut self) -> Result<(), String> {
        self.initialize()?;
        let requirements = self.history_requirements.ok_or_else(|| {
            "NativeOrderBookStrategy history requirements are missing".to_string()
        })?;
        if requirements.warmup_bars > 0 {
            return Err(
                "NativeOrderBookStrategy 不支持 Bar warmup_bars；请使用 Bar 宿主，或等待 Tick/OrderBook 专属就绪声明接入".into(),
            );
        }
        let result = self.strategy.on_start(&self.context);
        qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::Start,
            Some(self.context.as_of),
            None,
            result,
        )?;
        if !self.zero_warmup_notified {
            let result = self.strategy.on_warmup_complete(&self.context, None, 0);
            qx_strategy::notify_strategy_callback_result(
                &mut self.strategy,
                &self.context,
                StrategyErrorStage::WarmupComplete,
                Some(self.context.as_of),
                None,
                result,
            )?;
            self.zero_warmup_notified = true;
        }
        Ok(())
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.on_stop_with_reason(StrategyStopReason::HostStop)
    }

    fn on_stop_with_reason(&mut self, reason: StrategyStopReason) -> Result<(), String> {
        let stop_context =
            qx_strategy::StrategyStopContextV1::new(reason, Some(self.context.clone()));
        let result = self.strategy.on_stop_with_context(&stop_context);
        qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::Stop,
            Some(self.context.as_of),
            None,
            result,
        )
    }

    fn update_account_state(
        &mut self,
        cash: &std::collections::BTreeMap<String, i128>,
        available_margin_raw: Option<i128>,
    ) {
        self.context.cash.clone_from(cash);
        self.context.available_margin_raw = available_margin_raw;
    }

    fn supports_exact_orders(&self) -> bool {
        true
    }

    fn update_account_state_exact(
        &mut self,
        cash: &std::collections::BTreeMap<String, i128>,
        cash_exact: &std::collections::BTreeMap<String, ExactDecimal>,
        available_margin_raw: Option<i128>,
        available_margin_exact: Option<ExactDecimal>,
    ) {
        self.context.cash.clone_from(cash);
        self.context.cash_exact.clone_from(cash_exact);
        self.context.available_margin_raw = available_margin_raw;
        self.context.available_margin_exact = available_margin_exact;
    }

    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
    ) -> Result<Vec<Order>, String> {
        self.context.as_of = snapshot.ts;
        self.context
            .positions
            .insert(snapshot.instrument.to_string(), position);
        let event = match self.data_tier {
            crate::DataTier::L1 => {
                if !crate::orderbook::has_l1_shape(snapshot) {
                    return Err(format!(
                        "L1 Tick 事件只接受一档 bid/ask，实际 bid={} ask={}",
                        snapshot.bids.len(),
                        snapshot.asks.len()
                    ));
                }
                let bid = &snapshot.bids[0];
                let ask = &snapshot.asks[0];
                qx_strategy::MarketEvent::Tick {
                    instrument: snapshot.instrument.clone(),
                    ts: snapshot.ts,
                    bid_raw: bid.price.raw(),
                    ask_raw: ask.price.raw(),
                    last_raw: Some(bid.price.raw().saturating_add(ask.price.raw()) / 2),
                    volume_raw: Some(bid.qty.raw().saturating_add(ask.qty.raw())),
                }
            }
            crate::DataTier::Bar => return Err("订单簿策略不能消费 Bar 数据档位".into()),
            crate::DataTier::L2L3 => qx_strategy::MarketEvent::OrderBook {
                instrument: snapshot.instrument.clone(),
                ts: snapshot.ts,
                sequence: snapshot.sequence,
                bids: snapshot
                    .bids
                    .iter()
                    .map(|level| qx_strategy::OrderBookLevel {
                        price_raw: level.price.raw(),
                        qty_raw: level.qty.raw(),
                        price_exact: level.price_exact,
                        qty_exact: level.qty_exact,
                    })
                    .collect(),
                asks: snapshot
                    .asks
                    .iter()
                    .map(|level| qx_strategy::OrderBookLevel {
                        price_raw: level.price.raw(),
                        qty_raw: level.qty.raw(),
                        price_exact: level.price_exact,
                        qty_exact: level.qty_exact,
                    })
                    .collect(),
            },
        };
        let result = self.strategy.on_event(&self.context, &event);
        let decision = qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::MarketEvent,
            Some(snapshot.ts),
            Some(&snapshot.instrument),
            result,
        )?;
        let orders = decision.to_orders(&self.context);
        qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::MarketEvent,
            Some(snapshot.ts),
            Some(&snapshot.instrument),
            orders,
        )
    }

    fn on_order_book_actions_with_margin(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
        open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        self.context.available_margin_raw = available_margin_raw;
        let _ = open_order_ids;
        let mut orders = std::mem::take(&mut self.pending_order_updates);
        orders.extend(self.on_order_book(snapshot, position)?);
        Ok(OrderBookActions {
            orders,
            cancel_client_ids: Vec::new(),
        })
    }

    fn on_order_book_actions_with_exact_position(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position_raw: i128,
        position_exact: ExactDecimal,
        open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        let key = snapshot.instrument.to_string();
        self.context.positions.insert(key.clone(), position_raw);
        self.context.positions_exact.insert(key, position_exact);
        self.on_order_book_actions_with_margin(
            snapshot,
            position_raw,
            open_order_ids,
            available_margin_raw,
        )
    }

    fn on_fill(&mut self, order: &Order, fill: &qx_core::Fill) -> Result<(), qx_core::QxError> {
        deliver_order_update(
            &mut self.strategy,
            &mut self.context,
            &mut self.pending_order_updates,
            order,
            fill,
            None,
        )
    }

    fn on_fill_with_event_seq(
        &mut self,
        order: &Order,
        fill: &qx_core::Fill,
        event_seq: u64,
    ) -> Result<(), qx_core::QxError> {
        deliver_order_update(
            &mut self.strategy,
            &mut self.context,
            &mut self.pending_order_updates,
            order,
            fill,
            Some(event_seq),
        )
    }
}

/// Folds L1/L2 snapshots into deterministic Bar events for bar-only strategies.
pub struct DepthBarStrategy<S: qx_strategy::Strategy> {
    pub strategy: S,
    pub context: qx_strategy::StrategyContext,
    pub instrument: qx_core::InstrumentId,
    initialized: bool,
    history_requirements: Option<qx_strategy::StrategyHistoryRequirements>,
    warmup_seen: u64,
    warmup_completed: bool,
    zero_warmup_notified: bool,
    pending_order_updates: Vec<Order>,
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
            initialized: false,
            history_requirements: None,
            warmup_seen: 0,
            warmup_completed: false,
            zero_warmup_notified: false,
            pending_order_updates: Vec::new(),
        }
    }

    pub fn initialize(&mut self) -> Result<(), String> {
        if self.initialized {
            return Ok(());
        }
        let init = self.strategy.on_init(&self.context);
        qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::Initialization,
            Some(self.context.as_of),
            Some(&self.instrument),
            init,
        )?;
        let requirements = self.strategy.history_requirements();
        self.history_requirements = Some(qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::HistoryRequirements,
            Some(self.context.as_of),
            Some(&self.instrument),
            requirements,
        )?);
        self.initialized = true;
        Ok(())
    }

    pub(crate) fn on_mid(
        &mut self,
        instrument: &qx_core::InstrumentId,
        ts: u64,
        position: i128,
        mid: Price,
        volume_raw: i128,
        available_margin_raw: Option<i128>,
    ) -> Result<Vec<Order>, String> {
        self.context.as_of = ts;
        self.context.available_margin_raw = available_margin_raw;
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
            exact_ohlcv: None,
        };
        self.warmup_seen = self
            .warmup_seen
            .checked_add(1)
            .ok_or_else(|| "DepthBarStrategy warmup Bar 计数溢出".to_string())?;
        let requirements = match self.history_requirements {
            Some(requirements) => requirements,
            None => {
                let result = self.strategy.history_requirements();
                qx_strategy::notify_strategy_callback_result(
                    &mut self.strategy,
                    &self.context,
                    StrategyErrorStage::HistoryRequirements,
                    Some(ts),
                    Some(instrument),
                    result,
                )?
            }
        };
        let is_warm = self.warmup_seen >= requirements.warmup_bars;
        if requirements.warmup_bars > 0 && is_warm && !self.warmup_completed {
            let result =
                self.strategy
                    .on_warmup_complete(&self.context, Some(instrument), self.warmup_seen);
            qx_strategy::notify_strategy_callback_result(
                &mut self.strategy,
                &self.context,
                StrategyErrorStage::WarmupComplete,
                Some(ts),
                Some(instrument),
                result,
            )?;
            self.warmup_completed = true;
        }
        let result = self.strategy.on_event(&self.context, &event);
        let mut decision = qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::MarketEvent,
            Some(ts),
            Some(instrument),
            result,
        )?;
        if !is_warm {
            decision.intents.clear();
        }
        let orders = decision.to_orders(&self.context);
        qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::MarketEvent,
            Some(ts),
            Some(instrument),
            orders,
        )
    }
}

impl<S: qx_strategy::Strategy> OrderBookStrategy for DepthBarStrategy<S> {
    fn on_start(&mut self) -> Result<(), String> {
        self.initialize()?;
        let requirements = self
            .history_requirements
            .ok_or_else(|| "DepthBarStrategy history requirements are missing".to_string())?;
        let result = self.strategy.on_start(&self.context);
        qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::Start,
            Some(self.context.as_of),
            Some(&self.instrument),
            result,
        )?;
        if requirements.warmup_bars == 0 && !self.zero_warmup_notified {
            let result = self.strategy.on_warmup_complete(&self.context, None, 0);
            qx_strategy::notify_strategy_callback_result(
                &mut self.strategy,
                &self.context,
                StrategyErrorStage::WarmupComplete,
                Some(self.context.as_of),
                Some(&self.instrument),
                result,
            )?;
            self.zero_warmup_notified = true;
        }
        Ok(())
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.on_stop_with_reason(StrategyStopReason::HostStop)
    }

    fn on_stop_with_reason(&mut self, reason: StrategyStopReason) -> Result<(), String> {
        let stop_context =
            qx_strategy::StrategyStopContextV1::new(reason, Some(self.context.clone()));
        let result = self.strategy.on_stop_with_context(&stop_context);
        let stop_error = qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::Stop,
            Some(self.context.as_of),
            Some(&self.instrument),
            result,
        )
        .err();
        let required = self
            .history_requirements
            .map_or(0, |requirements| requirements.warmup_bars);
        let warmup_error = (required > 0 && !self.warmup_completed).then(|| {
            format!(
                "DepthBarStrategy warmup incomplete at stop ({}:{}/{required})",
                self.instrument, self.warmup_seen
            )
        });
        match (stop_error, warmup_error) {
            (Some(stop), Some(warmup)) => Err(format!("strategy on_stop failed: {stop}; {warmup}")),
            (Some(stop), None) => Err(stop),
            (None, Some(warmup)) => Err(warmup),
            (None, None) => Ok(()),
        }
    }

    fn update_account_state(
        &mut self,
        cash: &std::collections::BTreeMap<String, i128>,
        available_margin_raw: Option<i128>,
    ) {
        self.context.cash.clone_from(cash);
        self.context.available_margin_raw = available_margin_raw;
    }

    fn update_account_state_exact(
        &mut self,
        cash: &std::collections::BTreeMap<String, i128>,
        cash_exact: &std::collections::BTreeMap<String, ExactDecimal>,
        available_margin_raw: Option<i128>,
        available_margin_exact: Option<ExactDecimal>,
    ) {
        self.context.cash.clone_from(cash);
        self.context.cash_exact.clone_from(cash_exact);
        self.context.available_margin_raw = available_margin_raw;
        self.context.available_margin_exact = available_margin_exact;
    }

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
        let mut orders = std::mem::take(&mut self.pending_order_updates);
        orders.extend(self.on_mid(
            &snapshot.instrument,
            snapshot.ts,
            position,
            mid,
            volume_raw,
            None,
        )?);
        Ok(orders)
    }

    fn on_order_book_actions_with_margin(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
        _open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        let (Some(bid), Some(ask)) = (snapshot.bids.first(), snapshot.asks.first()) else {
            return Ok(OrderBookActions {
                orders: std::mem::take(&mut self.pending_order_updates),
                cancel_client_ids: Vec::new(),
            });
        };
        let mid = Price::from_raw(bid.price.raw().saturating_add(ask.price.raw()) / 2);
        let volume_raw = snapshot
            .bids
            .iter()
            .chain(snapshot.asks.iter())
            .fold(0_i128, |total, level| total.saturating_add(level.qty.raw()));
        let mut orders = std::mem::take(&mut self.pending_order_updates);
        orders.extend(self.on_mid(
            &snapshot.instrument,
            snapshot.ts,
            position,
            mid,
            volume_raw,
            available_margin_raw,
        )?);
        Ok(OrderBookActions {
            orders,
            cancel_client_ids: Vec::new(),
        })
    }

    fn on_fill(&mut self, order: &Order, fill: &qx_core::Fill) -> Result<(), qx_core::QxError> {
        deliver_order_update(
            &mut self.strategy,
            &mut self.context,
            &mut self.pending_order_updates,
            order,
            fill,
            None,
        )
    }

    fn on_fill_with_event_seq(
        &mut self,
        order: &Order,
        fill: &qx_core::Fill,
        event_seq: u64,
    ) -> Result<(), qx_core::QxError> {
        deliver_order_update(
            &mut self.strategy,
            &mut self.context,
            &mut self.pending_order_updates,
            order,
            fill,
            Some(event_seq),
        )
    }
}

fn deliver_order_update<S: qx_strategy::Strategy>(
    strategy: &mut S,
    context: &mut qx_strategy::StrategyContext,
    pending: &mut Vec<Order>,
    order: &Order,
    fill: &qx_core::Fill,
    event_seq: Option<u64>,
) -> Result<(), qx_core::QxError> {
    let owned = order
        .trace
        .as_ref()
        .and_then(|trace| trace.strategy_id.as_deref())
        == Some(context.strategy_id.as_str());
    if !owned {
        return Ok(());
    }
    context.as_of = context.as_of.max(fill.ts);
    let key = order.instrument.to_string();
    let current = context.positions.get(&key).copied().unwrap_or_default();
    let current_exact = context
        .positions_exact
        .get(&key)
        .copied()
        .or_else(|| qx_core::ExactDecimal::from_parts(current, 9).ok())
        .ok_or_else(|| qx_core::QxError::Invariant("策略精确持仓初始化失败".into()))?;
    let quantity_delta = match fill.exact_amounts {
        Some(amounts) => amounts.quantity,
        None => qx_core::ExactDecimal::from_parts(fill.qty.raw(), 9)
            .map_err(|error| qx_core::QxError::Invariant(error.into()))?,
    };
    let signed_quantity = if order.side == qx_core::Side::Buy {
        quantity_delta
    } else {
        quantity_delta
            .checked_neg()
            .ok_or_else(|| qx_core::QxError::Invariant("策略成交数量取反溢出".into()))?
    };
    let updated_exact = current_exact
        .checked_add(signed_quantity)
        .ok_or_else(|| qx_core::QxError::Invariant("策略精确持仓累计溢出".into()))?;
    let raw_projection = updated_exact
        .checked_mul_at_scale(
            qx_core::ExactDecimal::ONE,
            9,
            qx_core::DecimalRounding::TowardZero,
        )
        .and_then(|value| value.to_raw(9).ok())
        .ok_or_else(|| qx_core::QxError::Invariant("策略精确持仓投影失败".into()))?;
    context.positions.insert(key.clone(), raw_projection);
    context.positions_exact.insert(key, updated_exact);
    let decision = if let Some(event_seq) = event_seq {
        let update = StrategyFillUpdateV2::from_fill(order, fill, &context.strategy_id, event_seq)
            .map_err(qx_core::QxError::BusinessViolation)?;
        strategy.on_strategy_fill_update_v2(context, &update)
    } else {
        let update = StrategyUpdate::from_fill(order, fill, &context.strategy_id)
            .map_err(qx_core::QxError::BusinessViolation)?;
        strategy.on_strategy_update(context, &update)
    };
    let decision = qx_strategy::notify_strategy_callback_result(
        strategy,
        context,
        StrategyErrorStage::FillUpdate,
        Some(fill.ts),
        Some(&order.instrument),
        decision,
    )
    .map_err(qx_core::QxError::BusinessViolation)?;
    if let Some(decision) = decision {
        let orders = decision.to_orders(context);
        pending.extend(
            qx_strategy::notify_strategy_callback_result(
                strategy,
                context,
                StrategyErrorStage::FillUpdate,
                Some(fill.ts),
                Some(&order.instrument),
                orders,
            )
            .map_err(qx_core::QxError::BusinessViolation)?,
        );
    }
    Ok(())
}
