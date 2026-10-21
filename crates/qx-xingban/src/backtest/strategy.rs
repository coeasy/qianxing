//! Strategy adapters for the deterministic bar backtest engine.

use qx_core::{Fill, InstrumentId, Order};
use qx_guanxing::Bar;
use qx_strategy::{MarketEvent, Strategy, StrategyContext};
use std::collections::BTreeMap;

pub trait BarStrategy {
    /// Refresh account state from the authoritative ledger before strategy callbacks.
    fn update_account_state(
        &mut self,
        _cash: &BTreeMap<String, i128>,
        _available_margin_raw: Option<i128>,
    ) {
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
        self.update_account_state(cash, margin);
        margin
    }

    /// Per-strategy capital limit, when this adapter owns a strategy portfolio.
    fn strategy_budget_raw(&self, _strategy_id: &str) -> Option<i128> {
        None
    }

    /// Receive a fill only after the shared ledger and replay event log have accepted it.
    /// Portfolio adapters use this to reconcile strategy-owned virtual positions with
    /// the authoritative account fills before the next decision boundary.
    fn on_fill(&mut self, _order: &Order, _fill: &Fill) -> Result<(), qx_core::QxError> {
        Ok(())
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
    pending_order_updates: Vec<qx_strategy::StrategyDecision>,
}

impl<S: Strategy> NativeBarStrategy<S> {
    pub fn new(strategy: S, context: StrategyContext) -> Self {
        Self {
            strategy,
            context,
            virtual_position: false,
            pending_order_updates: Vec::new(),
        }
    }

    /// Use fill-confirmed positions for a strategy that can trade across instruments.
    /// Position state is updated only after the shared Ledger accepts a fill.
    pub fn with_virtual_position(mut self) -> Self {
        self.virtual_position = true;
        self
    }

    pub fn initialize(&mut self) -> Result<(), qx_core::QxError> {
        self.strategy
            .on_init(&self.context)
            .map_err(qx_core::QxError::BusinessViolation)
    }
}

impl<S: Strategy> NativeBarStrategy<S> {
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
        if order.account_id != self.context.account_id
            || fill.account_id != order.account_id
            || fill.order_id != order.client_id
            || fill.qty.raw() <= 0
            || fill.qty.raw()
                > order
                    .qty
                    .raw()
                    .checked_abs()
                    .ok_or_else(|| qx_core::QxError::Invariant("组合策略订单数量溢出".into()))?
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
            qx_core::Side::Buy => fill.qty.raw(),
            qx_core::Side::Sell => fill
                .qty
                .raw()
                .checked_neg()
                .ok_or_else(|| qx_core::QxError::Invariant("组合策略成交数量溢出".into()))?,
        };
        let position = self
            .context
            .positions
            .entry(order.instrument.to_string())
            .or_default();
        *position = position
            .checked_add(delta)
            .ok_or_else(|| qx_core::QxError::Invariant("组合策略虚拟持仓溢出".into()))?;
        Ok(())
    }
}

impl<S: Strategy> BarStrategy for NativeBarStrategy<S> {
    fn update_account_state(
        &mut self,
        cash: &BTreeMap<String, i128>,
        available_margin_raw: Option<i128>,
    ) {
        self.context.cash.clone_from(cash);
        self.context.available_margin_raw = available_margin_raw;
    }

    fn on_fill(&mut self, order: &Order, fill: &Fill) -> Result<(), qx_core::QxError> {
        self.apply_strategy_fill(order, fill)?;
        let is_owned = order
            .trace
            .as_ref()
            .and_then(|trace| trace.strategy_id.as_deref())
            == Some(self.context.strategy_id.as_str());
        if !is_owned {
            return Ok(());
        }

        // `on_fill` is called only after OMS, the authoritative ledger and replay
        // log have accepted the fill. Keep the wire event explicit and versioned
        // so native and language-bridge strategies see the same lifecycle fact.
        self.context.as_of = self.context.as_of.max(fill.ts);
        let update = serde_json::json!({
            "schema_version": qx_strategy::ORDER_UPDATE_SCHEMA_VERSION,
            "event": "fill",
            "order": {
                "order_id": order.client_id,
                "account_id": order.account_id.as_str(),
                "instrument": order.instrument.to_string(),
                "side": order.side,
                "qty_raw": order.qty.raw(),
                "filled_raw": order.filled.raw(),
                "strategy_id": self.context.strategy_id.as_str(),
            },
            "fill": fill,
        });
        if let Some(decision) = self
            .strategy
            .on_order_update(&self.context, &update.to_string())
            .map_err(qx_core::QxError::BusinessViolation)?
        {
            self.pending_order_updates.push(decision);
        }
        Ok(())
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
        let event = MarketEvent::Bar {
            instrument: instrument.clone(),
            ts: visible.ts,
            open_raw: visible.open,
            high_raw: visible.high,
            low_raw: visible.low,
            close_raw: visible.close,
            volume_raw: visible.volume,
        };
        let decision = self
            .strategy
            .on_event(&self.context, &event)
            .map_err(qx_core::QxError::BusinessViolation)?;
        let mut orders = Vec::new();
        for update_decision in self.pending_order_updates.drain(..) {
            update_decision
                .append_orders(&self.context, &mut orders)
                .map_err(qx_core::QxError::BusinessViolation)?;
        }
        decision
            .append_orders(&self.context, &mut orders)
            .map_err(qx_core::QxError::BusinessViolation)?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_strategy::{MarketEvent, Strategy, StrategyContext, StrategyDecision};

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
}
