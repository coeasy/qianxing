//! Strategy adapters for the deterministic bar backtest engine.

use qx_core::{InstrumentId, Order};
use qx_guanxing::Bar;
use qx_strategy::{MarketEvent, Strategy, StrategyContext};

pub trait BarStrategy {
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
}

impl<S: Strategy> NativeBarStrategy<S> {
    pub fn new(strategy: S, context: StrategyContext) -> Self {
        Self { strategy, context }
    }

    pub fn initialize(&mut self) -> Result<(), qx_core::QxError> {
        self.strategy
            .on_init(&self.context)
            .map_err(qx_core::QxError::BusinessViolation)
    }
}

impl<S: Strategy> BarStrategy for NativeBarStrategy<S> {
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
        self.context
            .positions
            .insert(instrument.to_string(), position);
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
