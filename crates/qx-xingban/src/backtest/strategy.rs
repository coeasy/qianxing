//! Strategy adapters for the deterministic bar backtest engine.

use qx_core::{Fill, InstrumentId, Order};
use qx_guanxing::Bar;
use qx_strategy::{MarketEvent, Strategy, StrategyContext};

pub trait BarStrategy {
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
}

impl<S: Strategy> NativeBarStrategy<S> {
    pub fn new(strategy: S, context: StrategyContext) -> Self {
        Self {
            strategy,
            context,
            virtual_position: false,
        }
    }

    pub(super) fn with_virtual_position(mut self) -> Self {
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
        if !self.virtual_position {
            return Ok(());
        }
        let Some(owner) = order
            .trace
            .as_ref()
            .and_then(|trace| trace.strategy_id.as_deref())
        else {
            return Err(qx_core::QxError::Invariant(
                "组合策略成交缺少 strategy_id trace".into(),
            ));
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
        if order.policy.unwrap_or_default().position_mode != qx_core::PositionMode::OneWay {
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
    fn on_fill(&mut self, order: &Order, fill: &Fill) -> Result<(), qx_core::QxError> {
        self.apply_strategy_fill(order, fill)
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
        self.context.as_of = visible.ts;
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
        decision
            .to_orders(&self.context)
            .map_err(qx_core::QxError::BusinessViolation)
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
