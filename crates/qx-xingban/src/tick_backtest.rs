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
    BookLevel, DataTier, DepthBarStrategy, DepthFrame, OrderBookBacktestConfig,
    OrderBookBacktestEngine, OrderBookBacktestReport, OrderBookExecutionModel, OrderBookSnapshot,
    OrderBookStrategy,
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

impl<S: Strategy> TickStrategy for DepthBarStrategy<S> {
    fn on_tick(&mut self, tick: &QuoteTick, position: i128) -> Result<Vec<Order>, String> {
        let Some(mid) = tick.mid() else {
            return Ok(Vec::new());
        };
        let instrument = self.instrument.clone();
        self.on_mid(
            &instrument,
            tick.ts,
            position,
            mid,
            tick.bid_qty.raw().saturating_add(tick.ask_qty.raw()),
        )
    }
}

/// L1 档位只接受买卖一档快照；出现多档说明数据与所选档位不符。
pub fn depth_frame_to_ticks(frame: &DepthFrame) -> Result<Vec<QuoteTick>, String> {
    frame.validate()?;
    let mut ticks = Vec::with_capacity(frame.snapshots.len());
    for snapshot in &frame.snapshots {
        let ([bid], [ask]) = (snapshot.bids.as_slice(), snapshot.asks.as_slice()) else {
            return Err(format!(
                "L1 档位回测只接受一档盘口，实际 bid={} ask={} (ts={})",
                snapshot.bids.len(),
                snapshot.asks.len(),
                snapshot.ts
            ));
        };
        ticks.push(QuoteTick::new(
            snapshot.ts,
            bid.price,
            bid.qty,
            ask.price,
            ask.qty,
            snapshot.sequence,
        ));
    }
    Ok(ticks)
}

pub struct TickBacktestConfig {
    pub instrument: InstrumentId,
    pub account_id: String,
    pub currency: String,
    pub initial_cash: Money,
    pub fee_bps: i128,
    pub instrument_spec: Option<TradingInstrumentSpec>,
    /// 与 Bar/Paper/Live 同源的风控门禁。
    pub risk: RiskGate,
}

pub type TickBacktestReport = OrderBookBacktestReport;

pub struct TickBacktestEngine {
    config: TickBacktestConfig,
    execution_model: Option<OrderBookExecutionModel>,
}

impl TickBacktestEngine {
    pub fn new(config: TickBacktestConfig) -> Self {
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
            risk: self.config.risk,
            data_tier: DataTier::L1,
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
    use qx_strategy::{StrategyOrderIntent, STRATEGY_API_VERSION};

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
            risk: RiskGate::conservative_default(),
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

    fn level(price: i64, qty: i64) -> BookLevel {
        BookLevel {
            price: Price::from_i64(price),
            qty: Quantity::from_i64(qty),
        }
    }

    fn book(ts: u64, bid: i64, ask: i64) -> OrderBookSnapshot {
        OrderBookSnapshot {
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            ts,
            sequence: ts,
            bids: vec![level(bid, 2)],
            asks: vec![level(ask, 2)],
        }
    }

    fn frame(snapshots: Vec<OrderBookSnapshot>) -> DepthFrame {
        DepthFrame {
            schema_version: DepthFrame::SUPPORTED_SCHEMA_VERSION,
            source: "fixture:tick-test".into(),
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            snapshots,
        }
    }

    fn l1_config(risk: RiskGate) -> TickBacktestConfig {
        TickBacktestConfig {
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            account_id: "main".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_i64(10_000),
            fee_bps: 0,
            instrument_spec: None,
            risk,
        }
    }

    #[test]
    fn depth_frame_to_ticks_requires_single_level_books() {
        let ticks =
            depth_frame_to_ticks(&frame(vec![book(1, 99, 100), book(2, 100, 101)])).unwrap();
        assert_eq!(ticks.len(), 2);
        assert_eq!(ticks[1].source_seq, 2);
        let deep = OrderBookSnapshot {
            bids: vec![level(99, 2), level(98, 2)],
            ..book(1, 99, 100)
        };
        assert!(depth_frame_to_ticks(&frame(vec![deep])).is_err());
    }

    #[test]
    fn tick_backtest_shares_the_configured_risk_gate() {
        let ticks =
            depth_frame_to_ticks(&frame(vec![book(1, 99, 100), book(2, 100, 101)])).unwrap();
        let allowed = TickBacktestEngine::new(l1_config(RiskGate::conservative_default()))
            .run(&ticks, &mut BuyOnce { done: false })
            .unwrap();
        assert_eq!(allowed.fills.len(), 1);
        assert!(matches!(allowed.data_tier, DataTier::L1));
        let mut rule_set = qx_risk::RuleSet::with_version("tick-test-max-qty");
        rule_set.add(Box::new(qx_risk::MaxQtyRule { max_qty: 0 }));
        let blocked = TickBacktestEngine::new(l1_config(RiskGate::from_rule_set(rule_set)))
            .run(&ticks, &mut BuyOnce { done: false })
            .unwrap();
        assert!(blocked.fills.is_empty());
        assert_eq!(blocked.risk_rule_set_version, "tick-test-max-qty");
        assert!(matches!(
            blocked.event_log.events().last().map(|event| &event.kind),
            Some(qx_core::EventKind::Rejected { .. })
        ));
    }

    struct BarOnlyBuy {
        emitted: bool,
        last_close_raw: i128,
    }

    impl Strategy for BarOnlyBuy {
        fn on_event(
            &mut self,
            context: &StrategyContext,
            event: &MarketEvent,
        ) -> Result<StrategyDecision, String> {
            let MarketEvent::Bar {
                instrument,
                ts,
                close_raw,
                ..
            } = event
            else {
                return Err("BarOnlyBuy 只接受 Bar 事件".into());
            };
            self.last_close_raw = *close_raw;
            if self.emitted {
                return Ok(StrategyDecision {
                    schema_version: STRATEGY_API_VERSION,
                    request_id: format!("idle:{ts}"),
                    strategy_id: context.strategy_id.clone(),
                    signal_id: *ts,
                    confidence: 0,
                    priority: 0,
                    expires_at: *ts,
                    intents: Vec::new(),
                });
            }
            self.emitted = true;
            Ok(StrategyDecision {
                schema_version: STRATEGY_API_VERSION,
                request_id: format!("buy:{ts}"),
                strategy_id: context.strategy_id.clone(),
                signal_id: *ts,
                confidence: 1,
                priority: 0,
                expires_at: *ts,
                intents: vec![StrategyOrderIntent {
                    intent_id: *ts,
                    instrument: instrument.clone(),
                    side: qx_core::Side::Buy,
                    qty: Quantity::from_i64(1),
                    limit: None,
                    policy: None,
                    reduce_only: false,
                    post_only: false,
                }],
            })
        }
    }

    #[test]
    fn depth_bar_view_folds_l1_ticks_into_bars() {
        let ticks =
            depth_frame_to_ticks(&frame(vec![book(1, 99, 100), book(2, 100, 101)])).unwrap();
        let context = StrategyContext {
            strategy_id: "tick-bar".into(),
            strategy_version: "v1".into(),
            account_id: "main".into(),
            venue_id: "paper".into(),
            data_fingerprint: "tick-1".into(),
            as_of: 1,
            positions: std::collections::BTreeMap::new(),
            cash: std::collections::BTreeMap::new(),
            available_margin_raw: Some(10_000),
            risk_state: "ready".into(),
        };
        let mut strategy = DepthBarStrategy::new(
            BarOnlyBuy {
                emitted: false,
                last_close_raw: 0,
            },
            context,
            InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        );
        let report = TickBacktestEngine::new(l1_config(RiskGate::conservative_default()))
            .run(&ticks, &mut strategy)
            .unwrap();
        // 快照 (100,101) 的中间价 100.5 作为最后一根合成 Bar 的收盘价。
        assert_eq!(
            strategy.strategy.last_close_raw,
            Price::from_i64(100).raw() + 500_000_000
        );
        assert_eq!(report.fills.len(), 1);
    }
}
