//! Strategy adapters for the deterministic bar backtest engine.

use qx_core::{Fill, InstrumentId, Order};
use qx_guanxing::Bar;
use qx_strategy::{
    MarketEvent, Strategy, StrategyContext, StrategyErrorStage, StrategyErrorV1,
    StrategyFillUpdateV2, StrategyHistoryRequirements, StrategyOrderStatusUpdate,
    StrategyStopContextV1, StrategyStopReason, StrategyUpdate,
};
use std::collections::{BTreeMap, BTreeSet};

pub trait BarStrategy {
    /// Called once after run inputs are prepared and before `on_start`.
    fn on_init(&mut self) -> Result<(), qx_core::QxError> {
        Ok(())
    }

    /// Called after run inputs are prepared and before the first strategy event.
    fn on_start(&mut self) -> Result<(), qx_core::QxError> {
        Ok(())
    }

    /// Called after completion, cancellation, pruning or an engine error.
    fn on_stop(&mut self) -> Result<(), qx_core::QxError> {
        Ok(())
    }

    /// Reason-aware companion that preserves existing strategy implementations.
    /// Hosts should use this hook when the terminal outcome is known.
    fn on_stop_with_reason(&mut self, _reason: StrategyStopReason) -> Result<(), qx_core::QxError> {
        self.on_stop()
    }

    /// Refresh account state from the authoritative ledger before strategy callbacks.
    fn update_account_state(
        &mut self,
        _cash: &BTreeMap<String, i128>,
        _available_margin_raw: Option<i128>,
    ) {
    }

    /// Exact companion snapshot for adapters that need exchange precision beyond legacy raw
    /// fields. Existing adapters keep receiving the original projection by default.
    fn update_account_state_exact(
        &mut self,
        cash: &BTreeMap<String, i128>,
        available_margin_raw: Option<i128>,
        _cash_exact: &BTreeMap<String, qx_core::ExactDecimal>,
        _available_margin_exact: Option<qx_core::ExactDecimal>,
    ) {
        self.update_account_state(cash, available_margin_raw);
    }

    fn refresh(
        &mut self,
        ledger: &qx_core::Ledger,
        account_id: &str,
        currency: &str,
        cash_account: bool,
    ) -> Option<i128> {
        let cash = ledger.cash_balances_for_ref(account_id);
        let margin = cash_account.then(|| ledger.cash_for(account_id, currency).max(0));
        let cash_exact = ledger.cash_balances_exact_for(account_id);
        let margin_exact = cash_account.then(|| {
            let available = ledger.cash_for_exact(account_id, currency);
            if available.is_negative() {
                qx_core::ExactDecimal::ZERO
            } else {
                available
            }
        });
        self.update_account_state_exact(cash, margin, &cash_exact, margin_exact);
        margin
    }

    /// Per-strategy capital limit, when this adapter owns a strategy portfolio.
    fn strategy_budget_raw(&self, _strategy_id: &str) -> Option<i128> {
        None
    }

    /// Scale-aware per-strategy capital limit. Legacy adapters inherit the 1e-9 raw projection.
    fn strategy_budget_exact(&self, strategy_id: &str) -> Option<qx_core::ExactDecimal> {
        self.strategy_budget_raw(strategy_id)
            .and_then(|raw| qx_core::ExactDecimal::from_parts(raw, 9).ok())
    }

    /// Receive a fill only after the shared ledger and replay event log have accepted it.
    /// Portfolio adapters use this to reconcile strategy-owned virtual positions with
    /// the authoritative account fills before the next decision boundary.
    fn on_fill(&mut self, _order: &Order, _fill: &Fill) -> Result<(), qx_core::QxError> {
        Ok(())
    }

    /// Receive a fill with its authoritative EventLog sequence. Legacy adapters
    /// remain source compatible and can continue implementing `on_fill` only.
    fn on_fill_with_event_seq(
        &mut self,
        order: &Order,
        fill: &Fill,
        event_seq: u64,
    ) -> Result<(), qx_core::QxError> {
        let _ = event_seq;
        self.on_fill(order, fill)
    }

    /// Receive a committed OMS/EventLog order-state transition. The default
    /// keeps legacy order-only strategies source compatible.
    fn on_order_status(
        &mut self,
        _update: &StrategyOrderStatusUpdate,
    ) -> Result<(), qx_core::QxError> {
        Ok(())
    }

    /// Whether this adapter owns an order status update. Routers use this to
    /// deliver cross-market strategy callbacks to exactly one strategy basket.
    fn owns_order_status(&self, _update: &StrategyOrderStatusUpdate) -> bool {
        false
    }

    fn on_bar(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
    ) -> Option<Order> {
        let _ = (history, instrument, ts, position);
        None
    }

    /// 多订单策略入口。旧策略只实现 `on_bar` 即可；组合/多腿策略可以
    /// 覆盖该方法一次返回多个 OrderIntent 对应的核心订单。
    fn on_bar_orders(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
    ) -> Vec<Order> {
        self.on_bar(history, instrument, ts, position)
            .into_iter()
            .collect()
    }

    /// 可返回错误的正式入口。兼容策略继续使用 `on_bar_orders`，跨语言或
    /// 统一 Strategy API adapter 可以把契约错误传回回测引擎，而不是静默
    /// 丢弃策略事件。
    fn on_bar_orders_checked(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        Ok(self.on_bar_orders(history, instrument, ts, position))
    }

    /// Account buying power visible to a strategy. The default preserves legacy
    /// adapters; native strategy adapters receive it through StrategyContext.
    fn on_bar_orders_with_margin_checked(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
        available_margin_raw: Option<i128>,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        let _ = available_margin_raw;
        self.on_bar_orders_checked(history, instrument, ts, position)
    }

    /// Precision-aware Bar callback. Legacy adapters use the established raw projection;
    /// native strategies also receive the authoritative exact position snapshot.
    fn on_bar_orders_with_margin_exact_checked(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
        position_exact: Option<qx_core::ExactDecimal>,
        available_margin_raw: Option<i128>,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        let _ = position_exact;
        self.on_bar_orders_with_margin_checked(
            history,
            instrument,
            ts,
            position,
            available_margin_raw,
        )
    }

    /// Drain the native strategy decisions produced at the most recent callback.
    /// Legacy order-only adapters keep the default empty audit surface.
    fn drain_decision_facts(&mut self) -> Vec<qx_core::StrategyDecisionFact> {
        Vec::new()
    }
}

/// 将统一 `qx-strategy::Strategy` 接入 Bar 回测。
///
/// 回测引擎在 Bar(t) 开始决策时传入的 `history` 最后一根是 Bar(t-1)，
/// 因此 adapter 只把该可见 Bar 转换为事件；新订单仍在 Bar(t) 提交，
/// 由撮合器按既有规则成交。
pub struct NativeBarStrategy<S: Strategy> {
    pub strategy: S,
    pub context: StrategyContext,
    virtual_position: bool,
    initialized: bool,
    history_requirements: StrategyHistoryRequirements,
    warmup_seen: BTreeMap<InstrumentId, u64>,
    warmup_completed: BTreeSet<InstrumentId>,
    zero_warmup_notified: bool,
    pending_order_updates: Vec<qx_strategy::StrategyDecision>,
    emitted_decisions: Vec<qx_core::StrategyDecisionFact>,
}

impl<S: Strategy> NativeBarStrategy<S> {
    pub fn new(strategy: S, context: StrategyContext) -> Self {
        Self {
            strategy,
            context,
            virtual_position: false,
            initialized: false,
            history_requirements: StrategyHistoryRequirements::default(),
            warmup_seen: BTreeMap::new(),
            warmup_completed: BTreeSet::new(),
            zero_warmup_notified: false,
            pending_order_updates: Vec::new(),
            emitted_decisions: Vec::new(),
        }
    }

    /// Use fill-confirmed positions for a strategy that can trade across instruments.
    /// Position state is updated only after the shared Ledger accepts a fill.
    pub fn with_virtual_position(mut self) -> Self {
        self.virtual_position = true;
        self
    }

    pub fn initialize(&mut self) -> Result<(), qx_core::QxError> {
        if self.initialized {
            return Ok(());
        }
        let init = self.strategy.on_init(&self.context);
        self.callback_result(StrategyErrorStage::Initialization, None, None, init)?;
        let requirements = self.strategy.history_requirements();
        self.history_requirements = self.callback_result(
            StrategyErrorStage::HistoryRequirements,
            None,
            None,
            requirements,
        )?;
        self.initialized = true;
        Ok(())
    }

    fn notify_callback_error(
        &mut self,
        stage: StrategyErrorStage,
        timestamp: Option<u64>,
        instrument: Option<&InstrumentId>,
        message: &str,
    ) {
        let error = StrategyErrorV1::new(
            stage,
            &self.context,
            timestamp,
            instrument.cloned(),
            message,
        );
        qx_strategy::notify_strategy_error(&mut self.strategy, &error);
    }

    fn callback_result<T>(
        &mut self,
        stage: StrategyErrorStage,
        timestamp: Option<u64>,
        instrument: Option<&InstrumentId>,
        result: Result<T, String>,
    ) -> Result<T, qx_core::QxError> {
        match result {
            Ok(value) => Ok(value),
            Err(message) => {
                self.notify_callback_error(stage, timestamp, instrument, &message);
                Err(qx_core::QxError::BusinessViolation(message))
            }
        }
    }
}

impl<S: Strategy> NativeBarStrategy<S> {
    fn decision_fact(decision: &qx_strategy::StrategyDecision) -> qx_core::StrategyDecisionFact {
        qx_core::StrategyDecisionFact {
            schema_version: decision.schema_version,
            request_id: decision.request_id.clone(),
            strategy_id: decision.strategy_id.clone(),
            signal_id: decision.signal_id,
            confidence: decision.confidence,
            priority: decision.priority,
            expires_at: decision.expires_at,
            intents: decision
                .intents
                .iter()
                .map(|intent| qx_core::StrategyIntentFact {
                    intent_id: intent.intent_id,
                    instrument: intent.instrument.clone(),
                    side: intent.side,
                    qty: intent.qty,
                    limit: intent.limit,
                    qty_exact: intent.qty_exact,
                    limit_exact: intent.limit_price_exact,
                    policy: intent.policy,
                    reduce_only: intent.reduce_only,
                    post_only: intent.post_only,
                })
                .collect(),
        }
    }

    fn apply_strategy_fill(&mut self, order: &Order, fill: &Fill) -> Result<(), qx_core::QxError> {
        let Some(owner) = order
            .trace
            .as_ref()
            .and_then(|trace| trace.strategy_id.as_deref())
        else {
            if self.virtual_position {
                return Err(qx_core::QxError::Invariant(
                    "组合策略成交缺少 strategy_id trace".into(),
                ));
            }
            return Ok(());
        };
        if owner != self.context.strategy_id {
            return Ok(());
        }
        let quantity = fill.quantity_decimal();
        let size_is_valid = if order.is_quote_budget_order() {
            match (
                order.remaining_budget_decimal(),
                fill.exact_amounts.and_then(|amounts| amounts.quote_cost),
            ) {
                (Some(remaining), Some(cost)) => {
                    cost.is_positive()
                        && cost
                            .checked_cmp(remaining)
                            .is_some_and(|ordering| !ordering.is_gt())
                }
                _ => false,
            }
        } else {
            let order_remaining = order
                .remaining_decimal()
                .unwrap_or(qx_core::ExactDecimal::ZERO);
            quantity
                .checked_cmp(order_remaining)
                .is_some_and(|ordering| !ordering.is_gt())
        };
        if order.account_id != self.context.account_id
            || fill.account_id != order.account_id
            || fill.order_id != order.client_id
            || !quantity.is_positive()
            || !size_is_valid
        {
            return Err(qx_core::QxError::Invariant(
                "组合策略成交账户、订单或数量与原订单不匹配".into(),
            ));
        }
        if self.virtual_position
            && order.policy.unwrap_or_default().position_mode != qx_core::PositionMode::OneWay
        {
            return Err(qx_core::QxError::BusinessViolation(
                "组合策略虚拟持仓暂不支持 Hedge position mode".into(),
            ));
        }
        let delta = match order.side {
            qx_core::Side::Buy => quantity,
            qx_core::Side::Sell => quantity
                .checked_neg()
                .ok_or_else(|| qx_core::QxError::Invariant("组合策略精确成交数量溢出".into()))?,
        };
        let instrument_key = order.instrument.to_string();
        let current = self
            .context
            .positions_exact
            .get(&instrument_key)
            .copied()
            .or_else(|| {
                self.context
                    .positions
                    .get(&instrument_key)
                    .and_then(|raw| qx_core::ExactDecimal::from_parts(*raw, 9).ok())
            })
            .unwrap_or(qx_core::ExactDecimal::ZERO);
        let next = current
            .checked_add(delta)
            .ok_or_else(|| qx_core::QxError::Invariant("组合策略精确虚拟持仓溢出".into()))?;
        self.context.positions.insert(
            instrument_key.clone(),
            exact_to_raw_projection(next, qx_core::DecimalRounding::TowardZero)?,
        );
        self.context.positions_exact.insert(instrument_key, next);
        Ok(())
    }

    fn dispatch_fill_update(
        &mut self,
        order: &Order,
        fill: &Fill,
        event_seq: Option<u64>,
    ) -> Result<(), qx_core::QxError> {
        self.apply_strategy_fill(order, fill)?;
        let is_owned = order
            .trace
            .as_ref()
            .and_then(|trace| trace.strategy_id.as_deref())
            == Some(self.context.strategy_id.as_str());
        if !is_owned {
            return Ok(());
        }

        self.context.as_of = self.context.as_of.max(fill.ts);
        let decision = if let Some(event_seq) = event_seq {
            let update =
                StrategyFillUpdateV2::from_fill(order, fill, &self.context.strategy_id, event_seq)
                    .map_err(qx_core::QxError::BusinessViolation)?;
            let result = self
                .strategy
                .on_strategy_fill_update_v2(&self.context, &update);
            self.callback_result(
                StrategyErrorStage::FillUpdate,
                Some(fill.ts),
                Some(&order.instrument),
                result,
            )?
        } else {
            let update = StrategyUpdate::from_fill(order, fill, &self.context.strategy_id)
                .map_err(qx_core::QxError::BusinessViolation)?;
            let result = self.strategy.on_strategy_update(&self.context, &update);
            self.callback_result(
                StrategyErrorStage::FillUpdate,
                Some(fill.ts),
                Some(&order.instrument),
                result,
            )?
        };
        if let Some(decision) = decision {
            self.pending_order_updates.push(decision);
        }
        Ok(())
    }
}

impl<S: Strategy> BarStrategy for NativeBarStrategy<S> {
    fn on_init(&mut self) -> Result<(), qx_core::QxError> {
        self.initialize()
    }

    fn on_start(&mut self) -> Result<(), qx_core::QxError> {
        self.initialize()?;
        let result = self.strategy.on_start(&self.context);
        self.callback_result(StrategyErrorStage::Start, None, None, result)?;
        if self.history_requirements.warmup_bars == 0 && !self.zero_warmup_notified {
            let result = self.strategy.on_warmup_complete(&self.context, None, 0);
            self.callback_result(StrategyErrorStage::WarmupComplete, None, None, result)?;
            self.zero_warmup_notified = true;
        }
        Ok(())
    }

    fn on_stop(&mut self) -> Result<(), qx_core::QxError> {
        self.on_stop_with_reason(StrategyStopReason::HostStop)
    }

    fn on_stop_with_reason(&mut self, reason: StrategyStopReason) -> Result<(), qx_core::QxError> {
        let stop_context = StrategyStopContextV1::new(reason, Some(self.context.clone()));
        let stop_error = match self.strategy.on_stop_with_context(&stop_context) {
            Ok(()) => None,
            Err(message) => {
                self.notify_callback_error(
                    StrategyErrorStage::Stop,
                    Some(self.context.as_of),
                    None,
                    &message,
                );
                Some(message)
            }
        };
        let required = self.history_requirements.warmup_bars;
        let mut incomplete = self
            .warmup_seen
            .iter()
            .filter(|(instrument, observed)| {
                **observed < required && !self.warmup_completed.contains(*instrument)
            })
            .map(|(instrument, observed)| format!("{instrument}: {observed}/{required}"))
            .collect::<Vec<_>>();
        if required > 0 && self.warmup_seen.is_empty() {
            incomplete.push(format!("no Bar events: 0/{required}"));
        }
        let warmup_error = (!incomplete.is_empty()).then(|| {
            format!(
                "NativeBarStrategy warmup incomplete at stop ({})",
                incomplete.join(", ")
            )
        });
        match (stop_error, warmup_error) {
            (Some(stop), Some(warmup)) => Err(qx_core::QxError::BusinessViolation(format!(
                "strategy on_stop failed: {stop}; {warmup}"
            ))),
            (Some(stop), None) => Err(qx_core::QxError::BusinessViolation(stop)),
            (None, Some(warmup)) => Err(qx_core::QxError::BusinessViolation(warmup)),
            (None, None) => Ok(()),
        }
    }

    fn update_account_state(
        &mut self,
        cash: &BTreeMap<String, i128>,
        available_margin_raw: Option<i128>,
    ) {
        self.context.cash.clone_from(cash);
        self.context.available_margin_raw = available_margin_raw;
    }

    fn on_fill(&mut self, order: &Order, fill: &Fill) -> Result<(), qx_core::QxError> {
        self.dispatch_fill_update(order, fill, None)
    }

    fn on_fill_with_event_seq(
        &mut self,
        order: &Order,
        fill: &Fill,
        event_seq: u64,
    ) -> Result<(), qx_core::QxError> {
        self.dispatch_fill_update(order, fill, Some(event_seq))
    }

    fn on_order_status(
        &mut self,
        update: &StrategyOrderStatusUpdate,
    ) -> Result<(), qx_core::QxError> {
        if update.order.strategy_id != self.context.strategy_id {
            return Ok(());
        }
        self.context.as_of = self.context.as_of.max(update.ts);
        let result = self
            .strategy
            .on_strategy_order_status(&self.context, update);
        let instrument = InstrumentId::parse(&update.order.instrument);
        if let Some(decision) = self.callback_result(
            StrategyErrorStage::OrderUpdate,
            Some(update.ts),
            instrument.as_ref(),
            result,
        )? {
            self.pending_order_updates.push(decision);
        }
        Ok(())
    }

    fn drain_decision_facts(&mut self) -> Vec<qx_core::StrategyDecisionFact> {
        std::mem::take(&mut self.emitted_decisions)
    }

    fn on_bar_orders_checked(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        let Some(visible) = history.last() else {
            return Ok(Vec::new());
        };
        if visible.ts >= ts {
            return Err(qx_core::QxError::Invariant(
                "NativeBarStrategy 收到不可见的当前或未来 Bar".into(),
            ));
        }
        // Markets may be sampled at different cadences. A callback for a slower
        // market must not move the shared strategy clock behind data already seen.
        self.context.as_of = self.context.as_of.max(visible.ts);
        self.context.venue_id = instrument.venue.to_string();
        if !self.virtual_position {
            self.context
                .positions
                .insert(instrument.to_string(), position);
        }
        let observed = self.warmup_seen.entry(instrument.clone()).or_default();
        *observed = observed.checked_add(1).ok_or_else(|| {
            qx_core::QxError::BusinessViolation("NativeBarStrategy warmup Bar 计数溢出".into())
        })?;
        let observed = *observed;
        let warmup_bars = self.history_requirements.warmup_bars;
        let is_warm = warmup_bars == 0 || observed >= warmup_bars;
        if is_warm && warmup_bars > 0 && !self.warmup_completed.contains(instrument) {
            let result =
                self.strategy
                    .on_warmup_complete(&self.context, Some(instrument), observed);
            self.callback_result(
                StrategyErrorStage::WarmupComplete,
                Some(visible.ts),
                Some(instrument),
                result,
            )?;
            self.warmup_completed.insert(instrument.clone());
        }
        let event = MarketEvent::Bar {
            instrument: instrument.clone(),
            ts: visible.ts,
            open_raw: visible.open,
            high_raw: visible.high,
            low_raw: visible.low,
            close_raw: visible.close,
            volume_raw: visible.volume,
            exact_ohlcv: visible.exact_ohlcv,
        };
        let result = self.strategy.on_event(&self.context, &event);
        let mut decision = self.callback_result(
            StrategyErrorStage::MarketEvent,
            Some(event.ts()),
            Some(instrument),
            result,
        )?;
        if !is_warm {
            decision.intents.clear();
        }
        let mut orders = Vec::new();
        for update_decision in self.pending_order_updates.drain(..) {
            let mut update_decision = update_decision;
            if !is_warm {
                update_decision.intents.clear();
            }
            update_decision
                .append_orders(&self.context, &mut orders)
                .map_err(qx_core::QxError::BusinessViolation)?;
            self.emitted_decisions
                .push(Self::decision_fact(&update_decision));
        }
        decision
            .append_orders(&self.context, &mut orders)
            .map_err(qx_core::QxError::BusinessViolation)?;
        self.emitted_decisions.push(Self::decision_fact(&decision));
        Ok(orders)
    }

    fn on_bar_orders_with_margin_checked(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
        available_margin_raw: Option<i128>,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        self.context.available_margin_raw = available_margin_raw;
        self.on_bar_orders_checked(history, instrument, ts, position)
    }

    fn update_account_state_exact(
        &mut self,
        cash: &BTreeMap<String, i128>,
        available_margin_raw: Option<i128>,
        cash_exact: &BTreeMap<String, qx_core::ExactDecimal>,
        available_margin_exact: Option<qx_core::ExactDecimal>,
    ) {
        self.context.cash = cash.clone();
        self.context.cash_exact = cash_exact.clone();
        self.context.available_margin_raw = available_margin_raw;
        self.context.available_margin_exact = available_margin_exact;
    }

    fn on_bar_orders_with_margin_exact_checked(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
        position_exact: Option<qx_core::ExactDecimal>,
        available_margin_raw: Option<i128>,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        if !self.virtual_position {
            let key = instrument.to_string();
            let exact = match position_exact {
                Some(exact) => exact,
                None => qx_core::ExactDecimal::from_parts(position, 9)
                    .map_err(|error| qx_core::QxError::Invariant(error.into()))?,
            };
            self.context.positions.insert(
                key.clone(),
                exact_to_raw_projection(exact, qx_core::DecimalRounding::TowardZero)?,
            );
            self.context.positions_exact.insert(key, exact);
        }
        self.context.available_margin_raw = available_margin_raw;
        self.on_bar_orders_checked(history, instrument, ts, position)
    }
}

fn exact_to_raw_projection(
    value: qx_core::ExactDecimal,
    rounding: qx_core::DecimalRounding,
) -> Result<i128, qx_core::QxError> {
    value
        .checked_div_at_scale(qx_core::ExactDecimal::ONE, 9, rounding)
        .and_then(|projected| projected.to_raw(9).ok())
        .ok_or_else(|| qx_core::QxError::Invariant("精确值投影到 raw 失败".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{ExactDecimal, Quantity, Side};
    use qx_strategy::{
        MarketEvent, Strategy, StrategyContext, StrategyDecision, StrategyErrorStage,
        StrategyErrorV1, StrategyHistoryRequirements, StrategyOrderIntent, StrategyStopContextV1,
        StrategyStopReason,
    };
    use std::sync::{Arc, Mutex};

    struct NoopStrategy;

    impl Strategy for NoopStrategy {
        fn on_event(
            &mut self,
            _context: &StrategyContext,
            _event: &MarketEvent,
        ) -> Result<StrategyDecision, String> {
            Ok(StrategyDecision {
                schema_version: qx_strategy::STRATEGY_API_VERSION,
                request_id: "as-of-test".into(),
                strategy_id: "as-of-test".into(),
                signal_id: 1,
                confidence: 10_000,
                priority: 0,
                expires_at: 0,
                intents: Vec::new(),
            })
        }
    }

    struct BrokenEventStrategy(Arc<Mutex<Option<StrategyErrorV1>>>);

    impl Strategy for BrokenEventStrategy {
        fn on_event(
            &mut self,
            _context: &StrategyContext,
            _event: &MarketEvent,
        ) -> Result<StrategyDecision, String> {
            Err("original strategy callback failure".into())
        }

        fn on_error(&mut self, error: &StrategyErrorV1) -> Result<(), String> {
            *self.0.lock().unwrap() = Some(error.clone());
            Err("error observer failure must not replace callback failure".into())
        }
    }

    #[test]
    fn native_bar_strategy_notifies_error_observer_and_preserves_original_failure() {
        let observed = Arc::new(Mutex::new(None));
        let mut adapter =
            NativeBarStrategy::new(BrokenEventStrategy(Arc::clone(&observed)), warmup_context());
        adapter.initialize().unwrap();
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let result =
            adapter.on_bar_orders_checked(&[Bar::new(100, 1, 1, 1, 1, 1)], &instrument, 110, 0);

        let error = result.unwrap_err().to_string();
        assert!(error.contains("original strategy callback failure"));
        assert!(!error.contains("error observer failure"));
        let observed = observed.lock().unwrap().clone().unwrap();
        assert_eq!(observed.stage, StrategyErrorStage::MarketEvent);
        assert_eq!(observed.strategy_id, "warmup-test");
        assert_eq!(observed.timestamp, Some(100));
        assert_eq!(observed.instrument.as_deref(), Some("BTCUSDT.BINANCE"));
        assert_eq!(observed.message, "original strategy callback failure");
    }

    struct ExactIntentStrategy {
        quantity: Option<ExactDecimal>,
        limit_price: Option<ExactDecimal>,
        quote_budget: Option<ExactDecimal>,
    }

    impl Strategy for ExactIntentStrategy {
        fn on_event(
            &mut self,
            context: &StrategyContext,
            event: &MarketEvent,
        ) -> Result<StrategyDecision, String> {
            Ok(StrategyDecision {
                schema_version: qx_strategy::STRATEGY_API_VERSION,
                request_id: "exact-bar-test".into(),
                strategy_id: context.strategy_id.clone(),
                signal_id: event.ts().max(1),
                confidence: 10_000,
                priority: 0,
                expires_at: 0,
                intents: vec![StrategyOrderIntent {
                    intent_id: event.ts().max(1),
                    instrument: event.instrument().unwrap().clone(),
                    side: Side::Buy,
                    qty: Quantity::ZERO,
                    qty_exact: self.quantity,
                    limit: None,
                    limit_price_exact: self.limit_price,
                    quote_budget: self.quote_budget,
                    policy: None,
                    reduce_only: false,
                    post_only: false,
                }],
            })
        }
    }

    #[test]
    fn native_strategy_context_clock_never_regresses_across_unaligned_markets() {
        let mut adapter = NativeBarStrategy::new(
            NoopStrategy,
            StrategyContext {
                strategy_id: "as-of-test".into(),
                strategy_version: "test-v1".into(),
                account_id: "account".into(),
                venue_id: "SIM".into(),
                data_fingerprint: "as-of-test".into(),
                as_of: 1,
                positions: BTreeMap::new(),
                cash: BTreeMap::new(),
                available_margin_raw: None,
                risk_state: "backtest".into(),
                positions_exact: BTreeMap::new(),
                cash_exact: BTreeMap::new(),
                available_margin_exact: None,
            },
        );
        let fast = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let slow = InstrumentId::parse("ETHUSDT.KRAKEN").unwrap();
        adapter
            .on_bar_orders_checked(&[Bar::new(100, 1, 1, 1, 1, 1)], &fast, 110, 0)
            .unwrap();
        assert_eq!(adapter.context.as_of, 100);
        adapter
            .on_bar_orders_checked(&[Bar::new(80, 1, 1, 1, 1, 1)], &slow, 120, 0)
            .unwrap();
        assert_eq!(adapter.context.as_of, 100);
    }

    #[test]
    fn native_bar_strategy_preserves_exact_orders_and_audits_original_precision() {
        let mut adapter = NativeBarStrategy::new(
            ExactIntentStrategy {
                quantity: Some(ExactDecimal::parse("0.001").unwrap()),
                limit_price: Some(ExactDecimal::parse("10.25").unwrap()),
                quote_budget: None,
            },
            warmup_context(),
        );
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let orders = adapter
            .on_bar_orders_checked(&[Bar::new(100, 1, 1, 1, 1, 1)], &instrument, 110, 0)
            .unwrap();

        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].qty.raw(), 0);
        assert_eq!(orders[0].limit, None);
        assert_eq!(
            orders[0].quantity_decimal(),
            ExactDecimal::parse("0.001").unwrap()
        );
        assert_eq!(
            orders[0].limit_price_decimal(),
            Some(ExactDecimal::parse("10.25").unwrap())
        );
        let fact = adapter.emitted_decisions.last().unwrap();
        assert_eq!(fact.intents[0].qty.raw(), 0);
        assert_eq!(
            fact.intents[0].qty_exact,
            Some(ExactDecimal::parse("0.001").unwrap())
        );
        assert_eq!(
            fact.intents[0].limit_exact,
            Some(ExactDecimal::parse("10.25").unwrap())
        );
    }

    #[test]
    fn native_bar_strategy_preserves_exact_quantity_and_quote_budget_intents() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let mut subnano = NativeBarStrategy::new(
            ExactIntentStrategy {
                quantity: Some(ExactDecimal::parse("0.0000000001").unwrap()),
                limit_price: None,
                quote_budget: None,
            },
            warmup_context(),
        );
        let orders = subnano
            .on_bar_orders_checked(&[Bar::new(100, 1, 1, 1, 1, 1)], &instrument, 110, 0)
            .unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(
            orders[0].quantity_decimal(),
            ExactDecimal::parse("0.0000000001").unwrap()
        );

        let mut quote_budget = NativeBarStrategy::new(
            ExactIntentStrategy {
                quantity: None,
                limit_price: None,
                quote_budget: Some(ExactDecimal::parse("25.5").unwrap()),
            },
            warmup_context(),
        );
        let orders = quote_budget
            .on_bar_orders_checked(&[Bar::new(100, 1, 1, 1, 1, 1)], &instrument, 110, 0)
            .unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(
            orders[0].remaining_budget_decimal(),
            Some(ExactDecimal::parse("25.5").unwrap())
        );
    }

    struct WarmupStrategy {
        required: u64,
        ready: bool,
        callbacks: Arc<Mutex<Vec<String>>>,
    }

    impl Strategy for WarmupStrategy {
        fn history_requirements(&self) -> Result<StrategyHistoryRequirements, String> {
            Ok(StrategyHistoryRequirements {
                warmup_bars: self.required,
            })
        }

        fn on_warmup_complete(
            &mut self,
            _context: &StrategyContext,
            instrument: Option<&InstrumentId>,
            observed_bars: u64,
        ) -> Result<(), String> {
            self.ready = true;
            self.callbacks.lock().unwrap().push(format!(
                "ready:{}:{observed_bars}",
                instrument.map(ToString::to_string).unwrap_or_default()
            ));
            Ok(())
        }

        fn on_event(
            &mut self,
            context: &StrategyContext,
            event: &MarketEvent,
        ) -> Result<StrategyDecision, String> {
            self.callbacks
                .lock()
                .unwrap()
                .push(format!("event:{}:{}", self.ready, event.ts()));
            let instrument = event.instrument().unwrap().clone();
            Ok(StrategyDecision {
                schema_version: qx_strategy::STRATEGY_API_VERSION,
                request_id: format!("warmup:{}", event.ts()),
                strategy_id: context.strategy_id.clone(),
                signal_id: event.ts(),
                confidence: 10_000,
                priority: 0,
                expires_at: 0,
                intents: vec![StrategyOrderIntent {
                    intent_id: event.ts(),
                    instrument,
                    side: Side::Buy,
                    qty: Quantity::from_raw(1),
                    qty_exact: None,
                    limit: None,
                    limit_price_exact: None,
                    quote_budget: None,
                    policy: None,
                    reduce_only: false,
                    post_only: false,
                }],
            })
        }
    }

    fn warmup_context() -> StrategyContext {
        StrategyContext {
            strategy_id: "warmup-test".into(),
            strategy_version: "test-v1".into(),
            account_id: "account".into(),
            venue_id: "SIM".into(),
            data_fingerprint: "warmup-test".into(),
            as_of: 1,
            positions_exact: std::collections::BTreeMap::new(),
            cash_exact: std::collections::BTreeMap::new(),
            available_margin_exact: None,
            positions: BTreeMap::new(),
            cash: BTreeMap::new(),
            available_margin_raw: None,
            risk_state: "backtest".into(),
        }
    }

    #[test]
    fn native_bar_strategy_passes_terminal_reason_and_final_context() {
        struct StopObserver(Arc<Mutex<Option<StrategyStopContextV1>>>);

        impl Strategy for StopObserver {
            fn on_event(
                &mut self,
                context: &StrategyContext,
                _event: &MarketEvent,
            ) -> Result<StrategyDecision, String> {
                Ok(StrategyDecision {
                    schema_version: qx_strategy::STRATEGY_API_VERSION,
                    request_id: "stop-context-test".into(),
                    strategy_id: context.strategy_id.clone(),
                    signal_id: 1,
                    confidence: 10_000,
                    priority: 0,
                    expires_at: 0,
                    intents: Vec::new(),
                })
            }

            fn on_stop_with_context(&mut self, stop: &StrategyStopContextV1) -> Result<(), String> {
                *self.0.lock().unwrap() = Some(stop.clone());
                Ok(())
            }
        }

        let received = Arc::new(Mutex::new(None));
        let mut adapter =
            NativeBarStrategy::new(StopObserver(Arc::clone(&received)), warmup_context());
        adapter.initialize().unwrap();
        adapter
            .on_stop_with_reason(StrategyStopReason::Cancelled)
            .unwrap();

        let stop = received.lock().unwrap().clone().unwrap();
        assert_eq!(stop.reason, StrategyStopReason::Cancelled);
        assert_eq!(
            stop.context
                .as_ref()
                .map(|context| context.strategy_id.as_str()),
            Some("warmup-test")
        );
    }

    #[test]
    fn strategy_context_accepts_subnano_position_with_zero_legacy_projection() {
        let instrument = InstrumentId::parse("DOGEUSDT.BINANCE").unwrap().to_string();
        let mut context = warmup_context();
        context.positions.insert(instrument.clone(), 0);
        context.positions_exact.insert(
            instrument.clone(),
            ExactDecimal::parse("0.0000000001").unwrap(),
        );
        context.cash.insert("USDT".into(), 1_000_000_000);
        context
            .cash_exact
            .insert("USDT".into(), ExactDecimal::parse("1").unwrap());
        context.available_margin_raw = Some(1_000_000_000);
        context.available_margin_exact = Some(ExactDecimal::parse("1").unwrap());
        context.validate().unwrap();

        context.positions.insert(instrument, 1);
        assert!(context.validate().is_err());
    }

    #[test]
    fn native_bar_strategy_suppresses_intents_until_each_instrument_is_warm() {
        let callbacks = Arc::new(Mutex::new(Vec::new()));
        let strategy = WarmupStrategy {
            required: 2,
            ready: false,
            callbacks: Arc::clone(&callbacks),
        };
        let mut adapter = NativeBarStrategy::new(strategy, warmup_context());
        adapter.initialize().unwrap();
        adapter.on_start().unwrap();
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();

        let first = adapter
            .on_bar_orders_checked(&[Bar::new(100, 1, 1, 1, 1, 1)], &instrument, 110, 0)
            .unwrap();
        assert!(first.is_empty());
        assert!(adapter.emitted_decisions.last().unwrap().intents.is_empty());

        let second = adapter
            .on_bar_orders_checked(
                &[Bar::new(100, 1, 1, 1, 1, 1), Bar::new(110, 1, 1, 1, 1, 1)],
                &instrument,
                120,
                0,
            )
            .unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].instrument, instrument);
        assert_eq!(
            *callbacks.lock().unwrap(),
            vec![
                "event:false:100",
                "ready:BTCUSDT.BINANCE:2",
                "event:true:110"
            ]
        );
    }

    #[test]
    fn native_bar_strategy_counts_warmup_per_instrument() {
        let callbacks = Arc::new(Mutex::new(Vec::new()));
        let strategy = WarmupStrategy {
            required: 2,
            ready: false,
            callbacks: Arc::clone(&callbacks),
        };
        let mut adapter = NativeBarStrategy::new(strategy, warmup_context());
        adapter.initialize().unwrap();
        adapter.on_start().unwrap();
        let fast = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let slow = InstrumentId::parse("ETHUSDT.BINANCE").unwrap();

        assert!(adapter
            .on_bar_orders_checked(&[Bar::new(100, 1, 1, 1, 1, 1)], &fast, 110, 0)
            .unwrap()
            .is_empty());
        assert_eq!(
            adapter
                .on_bar_orders_checked(
                    &[Bar::new(100, 1, 1, 1, 1, 1), Bar::new(110, 1, 1, 1, 1, 1)],
                    &fast,
                    120,
                    0,
                )
                .unwrap()
                .len(),
            1
        );
        assert!(adapter
            .on_bar_orders_checked(&[Bar::new(105, 1, 1, 1, 1, 1)], &slow, 130, 0)
            .unwrap()
            .is_empty());
        assert_eq!(
            adapter
                .on_bar_orders_checked(
                    &[Bar::new(105, 1, 1, 1, 1, 1), Bar::new(115, 1, 1, 1, 1, 1)],
                    &slow,
                    140,
                    0,
                )
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            callbacks
                .lock()
                .unwrap()
                .iter()
                .filter(|callback| callback.starts_with("ready:"))
                .count(),
            2
        );
    }

    #[test]
    fn native_bar_strategy_notifies_zero_warmup_once_after_start() {
        let callbacks = Arc::new(Mutex::new(Vec::new()));
        let strategy = WarmupStrategy {
            required: 0,
            ready: false,
            callbacks: Arc::clone(&callbacks),
        };
        let mut adapter = NativeBarStrategy::new(strategy, warmup_context());
        adapter.initialize().unwrap();
        adapter.initialize().unwrap();
        adapter.on_start().unwrap();
        adapter.on_start().unwrap();
        assert_eq!(*callbacks.lock().unwrap(), vec!["ready::0"]);
    }

    #[test]
    fn native_bar_strategy_fails_stop_when_declared_warmup_is_incomplete() {
        let strategy = WarmupStrategy {
            required: 2,
            ready: false,
            callbacks: Arc::new(Mutex::new(Vec::new())),
        };
        let mut adapter = NativeBarStrategy::new(strategy, warmup_context());
        adapter.initialize().unwrap();
        adapter.on_start().unwrap();
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        adapter
            .on_bar_orders_checked(&[Bar::new(100, 1, 1, 1, 1, 1)], &instrument, 110, 0)
            .unwrap();
        let error = adapter.on_stop().unwrap_err().to_string();
        assert!(error.contains("1/2"));
    }
}
