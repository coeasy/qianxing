//! Caller-authored strategy entrypoints for the shared Tick and OrderBook engines.

use super::run_inner_with_strategy;
use crate::cases::{attach, guard::guard_panics};
use crate::{
    AppError, AppErrorCategory, CallerCapability, DepthBacktestOutcome, DepthBacktestSpec,
    RunContext, StrategyArtifactIdentity,
};
use qx_core::InstrumentId;
use qx_guanxing::QuoteTick;
use qx_strategy::{Strategy, StrategyContext};
use qx_xingban::{OrderBookSnapshot, OrderBookStrategy};

pub(in crate::cases) struct InjectedDepthStrategy {
    pub(super) identity: StrategyArtifactIdentity,
    pub(super) strategy: Box<dyn Strategy>,
}

pub(super) struct CallbackStrategy(pub(super) Box<dyn Strategy>);

pub(super) struct CallbackOrderBookStrategy {
    strategy: CallbackStrategy,
    context: StrategyContext,
    instrument: InstrumentId,
    tier: String,
}

impl CallbackOrderBookStrategy {
    pub(super) fn new(
        strategy: Box<dyn Strategy>,
        context: StrategyContext,
        instrument: InstrumentId,
        tier: impl Into<String>,
    ) -> Result<Self, String> {
        let mut strategy = Self {
            strategy: CallbackStrategy(strategy),
            context,
            instrument,
            tier: tier.into(),
        };
        strategy.strategy.on_init(&strategy.context)?;
        Ok(strategy)
    }
}

impl OrderBookStrategy for CallbackOrderBookStrategy {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
    ) -> Result<Vec<qx_core::Order>, String> {
        self.context.as_of = snapshot.ts;
        self.context
            .positions
            .insert(snapshot.instrument.to_string(), position);
        let event = if self.tier == "l1" {
            let (Some(bid), Some(ask)) = (snapshot.bids.first(), snapshot.asks.first()) else {
                return Ok(Vec::new());
            };
            let tick = QuoteTick::new(
                snapshot.ts,
                bid.price,
                bid.qty,
                ask.price,
                ask.qty,
                snapshot.sequence,
            );
            qx_strategy::MarketEvent::Tick {
                instrument: self.instrument.clone(),
                ts: tick.ts,
                bid_raw: tick.bid.raw(),
                ask_raw: tick.ask.raw(),
                last_raw: tick.mid().map(|price| price.raw()),
                volume_raw: Some(tick.bid_qty.raw().saturating_add(tick.ask_qty.raw())),
            }
        } else {
            qx_strategy::MarketEvent::OrderBook {
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
            }
        };
        let decision = self.strategy.on_event(&self.context, &event)?;
        decision
            .to_orders(&self.context)
            .map_err(|error| error.to_string())
    }
}

impl Strategy for CallbackStrategy {
    fn on_init(&mut self, context: &StrategyContext) -> Result<(), String> {
        self.0.on_init(context)
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &qx_strategy::MarketEvent,
    ) -> Result<qx_strategy::StrategyDecision, String> {
        self.0.on_event(context, event)
    }

    fn on_order_update(
        &mut self,
        context: &StrategyContext,
        update: &str,
    ) -> Result<Option<qx_strategy::StrategyDecision>, String> {
        self.0.on_order_update(context, update)
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.0.on_stop()
    }
}

/// Execute caller-authored decisions through the shared Rust Tick/OrderBook risk and matching path.
pub(super) fn run_custom_strategies(
    spec: &DepthBacktestSpec,
    strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
    context: &RunContext,
) -> Result<DepthBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "多语言策略 Tick/OrderBook 回测")?;
    let injected = prepare_injected(spec, strategies)?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        run_inner_with_strategy(spec, context, None, injected)
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| {
        attach(
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "同步多语言策略深度回测未产生报告",
            ),
            &correlation_id,
        )
    })
}

/// Start a cancellable custom strategy on the shared Tick/OrderBook run lifecycle.
pub(super) fn start_custom_strategies(
    spec: DepthBacktestSpec,
    strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
    context: RunContext,
) -> Result<crate::RunHandle<DepthBacktestOutcome>, AppError> {
    context.require(CallerCapability::Research, "多语言策略 Tick/OrderBook 回测")?;
    spec.validate()?;
    let injected = prepare_injected(&spec, strategies)?;
    let run_id = context.correlation_id().to_string();
    Ok(crate::RunHandle::spawn(run_id, move |cancellation| {
        let correlation_id = context.correlation_id().to_string();
        guard_panics(&correlation_id, || {
            run_inner_with_strategy(&spec, &context, Some(&cancellation), injected)
        })
        .map_err(|error| attach(error, &correlation_id))
    }))
}

fn prepare_injected(
    spec: &DepthBacktestSpec,
    strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
) -> Result<Vec<InjectedDepthStrategy>, AppError> {
    if strategies.is_empty() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "至少需要一个自定义 Tick/OrderBook 策略回调",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    strategies
        .into_iter()
        .map(|(identity, strategy)| {
            if !seen.insert(identity.strategy_id.clone()) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!(
                        "自定义 Tick/OrderBook 策略身份重复: {}",
                        identity.strategy_id
                    ),
                ));
            }
            identity.validate_for_depth(spec)?;
            Ok(InjectedDepthStrategy { identity, strategy })
        })
        .collect()
}
