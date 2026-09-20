//! 基于 L1 QuoteTick 的因果回测。
//!
//! QuoteTick 没有完整深度，因此只把 bid/ask 一档转换为订单簿快照；成交、
//! 流动性扣减、OMS、Ledger 和重放全部复用 `orderbook_backtest`，不会产生
//! 第二套订单语义。策略在 tick(t) 之后提交的订单只能在 tick(t+1) 及以后
//! 看到并成交。

use qx_core::{InstrumentId, Order, TradingInstrumentSpec};
use qx_guanxing::QuoteTick;
use qx_strategy::{MarketEvent, Strategy, StrategyContext, StrategyDecision};

use crate::{
    BookLevel, OrderBookBacktestConfig, OrderBookBacktestEngine, OrderBookBacktestReport,
    OrderBookExecutionModel, OrderBookSnapshot, OrderBookStrategy,
};
use qx_core::Money;
use qx_zhenlu::RiskGate;

pub trait TickStrategy {
    fn on_tick(&mut self, tick: &QuoteTick, position: i128) -> Result<Vec<Order>, String>;
}

pub struct NativeTickStrategy<S: Strategy> {
    pub strategy: S,
    pub context: StrategyContext,
    pub instrument: InstrumentId,
}

impl<S: Strategy> NativeTickStrategy<S> {
    pub fn new(strategy: S, context: StrategyContext, instrument: InstrumentId) -> Self {
        Self {
            strategy,
            context,
            instrument,
        }
    }

    pub fn initialize(&mut self) -> Result<(), String> {
        self.strategy.on_init(&self.context)
    }
}

impl<S: Strategy> TickStrategy for NativeTickStrategy<S> {
    fn on_tick(&mut self, tick: &QuoteTick, position: i128) -> Result<Vec<Order>, String> {
        self.context.as_of = tick.ts;
        self.context
            .positions
            .insert(self.instrument.to_string(), position);
        let event = MarketEvent::Tick {
            instrument: self.instrument.clone(),
            ts: tick.ts,
            bid_raw: tick.bid.raw(),
            ask_raw: tick.ask.raw(),
            last_raw: tick.mid().map(|price| price.raw()),
            volume_raw: Some(tick.bid_qty.raw().saturating_add(tick.ask_qty.raw())),
        };
        let decision: StrategyDecision = self.strategy.on_event(&self.context, &event)?;
        decision.to_orders(&self.context)
    }
}

struct TickAdapter<'a> {
    strategy: &'a mut dyn TickStrategy,
    instrument: InstrumentId,
}

impl TickAdapter<'_> {
    fn snapshot_to_tick(&self, snapshot: &OrderBookSnapshot) -> Result<QuoteTick, String> {
        let bid = snapshot
            .bids
            .first()
            .ok_or_else(|| "L1 tick 缺少 bid".to_string())?;
        let ask = snapshot
            .asks
            .first()
            .ok_or_else(|| "L1 tick 缺少 ask".to_string())?;
        Ok(QuoteTick::new(
            snapshot.ts,
            bid.price,
            bid.qty,
            ask.price,
            ask.qty,
            snapshot.sequence,
        ))
    }
}

impl OrderBookStrategy for TickAdapter<'_> {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
    ) -> Result<Vec<Order>, String> {
        if snapshot.instrument != self.instrument {
            return Err("Tick adapter instrument 不一致".into());
        }
        self.strategy
            .on_tick(&self.snapshot_to_tick(snapshot)?, position)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TickBacktestConfig {
    pub instrument: InstrumentId,
    pub account_id: String,
    pub currency: String,
    pub initial_cash: Money,
    pub fee_bps: i128,
    pub instrument_spec: Option<TradingInstrumentSpec>,
}

pub type TickBacktestReport = OrderBookBacktestReport;

pub struct TickBacktestEngine {
    config: TickBacktestConfig,
    execution_model: Option<OrderBookExecutionModel>,
    risk: RiskGate,
}

impl TickBacktestEngine {
    pub fn new(config: TickBacktestConfig) -> Self {
        Self {
            config,
            execution_model: None,
            risk: RiskGate::new(),
        }
    }

    /// Tick 回测复用盘口回测的风险门，规则在这里注入而不是写进
    /// `TickBacktestConfig`：该配置派生 `Clone/PartialEq/Eq`，而 `RiskGate`
    /// 持有 `Box<dyn RiskRule>`，无法跟随派生。
    pub fn with_risk_gate(mut self, risk: RiskGate) -> Self {
        self.risk = risk;
        self
    }

    pub fn with_execution_model(mut self, model: OrderBookExecutionModel) -> Self {
        self.execution_model = Some(model);
        self
    }

    pub fn run(
        self,
        ticks: &[QuoteTick],
        strategy: &mut dyn TickStrategy,
    ) -> Result<TickBacktestReport, qx_core::QxError> {
        if ticks.windows(2).any(|window| {
            window[0].ts >= window[1].ts || window[0].source_seq >= window[1].source_seq
        }) {
            return Err(qx_core::QxError::BusinessViolation(
                "Tick 回测 ts/source_seq 必须严格递增".into(),
            ));
        }
        let snapshots = ticks
            .iter()
            .map(|tick| self.snapshot(tick))
            .collect::<Result<Vec<_>, _>>()?;
        let config = OrderBookBacktestConfig {
            instrument: self.config.instrument.clone(),
            account_id: self.config.account_id,
            currency: self.config.currency,
            initial_cash: self.config.initial_cash,
            fee_bps: self.config.fee_bps,
            instrument_spec: self.config.instrument_spec,
            risk: self.risk,
        };
        let mut adapter = TickAdapter {
            strategy,
            instrument: self.config.instrument,
        };
        let engine = OrderBookBacktestEngine::new(config);
        let engine = match self.execution_model {
            Some(model) => engine.with_execution_model(model),
            None => engine,
        };
        engine.run(&snapshots, &mut adapter)
    }

    fn snapshot(&self, tick: &QuoteTick) -> Result<OrderBookSnapshot, qx_core::QxError> {
        if tick.ts == 0
            || tick.source_seq == 0
            || tick.bid.raw() <= 0
            || tick.ask.raw() <= 0
            || tick.bid.raw() > tick.ask.raw()
            || tick.bid_qty.raw() <= 0
            || tick.ask_qty.raw() <= 0
        {
            return Err(qx_core::QxError::Permanent(
                "QuoteTick 时间、序列、价格或数量非法".into(),
            ));
        }
        Ok(OrderBookSnapshot {
            instrument: self.config.instrument.clone(),
            ts: tick.ts,
            sequence: tick.source_seq,
            bids: vec![BookLevel {
                price: tick.bid,
                qty: tick.bid_qty,
            }],
            asks: vec![BookLevel {
                price: tick.ask,
                qty: tick.ask_qty,
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{OrderStatus, Price, Quantity, Side};

    struct BuyOnce {
        done: bool,
    }

    impl TickStrategy for BuyOnce {
        fn on_tick(&mut self, _tick: &QuoteTick, _position: i128) -> Result<Vec<Order>, String> {
            if self.done {
                return Ok(Vec::new());
            }
            self.done = true;
            Ok(vec![Order {
                client_id: 1,
                instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
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

    #[test]
    fn tick_backtest_is_causal_and_reuses_l1_ask_liquidity() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let config = TickBacktestConfig {
            instrument,
            account_id: "main".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_i64(10_000),
            fee_bps: 0,
            instrument_spec: None,
        };
        let ticks = vec![
            QuoteTick::new(
                1,
                Price::from_i64(99),
                Quantity::from_i64(2),
                Price::from_i64(100),
                Quantity::from_i64(2),
                1,
            ),
            QuoteTick::new(
                2,
                Price::from_i64(100),
                Quantity::from_i64(2),
                Price::from_i64(101),
                Quantity::from_i64(2),
                2,
            ),
        ];
        let mut strategy = BuyOnce { done: false };
        let report = TickBacktestEngine::new(config)
            .run(&ticks, &mut strategy)
            .unwrap();
        assert_eq!(report.fills.len(), 1);
        assert_eq!(report.fills[0].ts, 2);
        assert_eq!(report.fills[0].price, Price::from_i64(101));
    }

    #[test]
    fn tick_backtest_applies_the_injected_risk_gate() {
        let config = TickBacktestConfig {
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            account_id: "main".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_i64(10_000),
            fee_bps: 0,
            instrument_spec: None,
        };
        let ticks = vec![QuoteTick::new(
            1,
            Price::from_i64(99),
            Quantity::from_i64(2),
            Price::from_i64(100),
            Quantity::from_i64(2),
            1,
        )];
        let mut gate = RiskGate::new();
        gate.add(Box::new(qx_zhenlu::MaxQtyRule { max_qty: 1 }));
        let mut strategy = BuyOnce { done: false };
        let report = TickBacktestEngine::new(config)
            .with_risk_gate(gate)
            .run(&ticks, &mut strategy)
            .unwrap();
        assert!(report.fills.is_empty());
        assert!(report
            .event_log
            .events()
            .iter()
            .any(|event| matches!(event.kind, qx_core::EventKind::Rejected { .. })));
    }
}
