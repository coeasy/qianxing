//! Caller-authored strategy entrypoints for the shared Tick and OrderBook engines.

use super::run_inner_with_strategy;
use crate::cases::{attach, guard::guard_panics};
use crate::{
    AppError, AppErrorCategory, CallerCapability, DepthBacktestOutcome, DepthBacktestSpec,
    RunContext, StrategyArtifactIdentity,
};
use qx_core::{ExactDecimal, InstrumentId, Order};
use qx_guanxing::QuoteTick;
use qx_strategy::{
    Strategy, StrategyContext, StrategyFillUpdateV2, StrategyOrderStatusUpdate, StrategyUpdate,
};
use qx_xingban::{OrderBookSnapshot, OrderBookStrategy};

pub(in crate::cases) struct InjectedDepthStrategy {
    pub(super) identity: StrategyArtifactIdentity,
    pub(super) strategy: Box<dyn Strategy>,
    pub(super) experiment_parameters: bool,
}

pub(super) struct CallbackStrategy(pub(super) Box<dyn Strategy>);

pub(super) struct CallbackOrderBookStrategy {
    strategy: CallbackStrategy,
    context: StrategyContext,
    instrument: InstrumentId,
    tier: String,
    pending_orders: Vec<Order>,
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
            pending_orders: Vec::new(),
        };
        strategy.strategy.on_init(&strategy.context)?;
        Ok(strategy)
    }
}

impl OrderBookStrategy for CallbackOrderBookStrategy {
    fn on_start(&mut self) -> Result<(), String> {
        let requirements = self.strategy.0.history_requirements()?;
        if requirements.warmup_bars > 0 {
            return Err(
                "Depth callback strategy 不支持 Bar warmup_bars；请使用 Bar 宿主，或实现专属就绪声明".into(),
            );
        }
        self.strategy.0.on_start(&self.context)?;
        self.strategy.0.on_warmup_complete(&self.context, None, 0)
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.strategy.0.on_stop()
    }

    fn on_stop_with_reason(
        &mut self,
        reason: qx_strategy::StrategyStopReason,
    ) -> Result<(), String> {
        let stop_context =
            qx_strategy::StrategyStopContextV1::new(reason, Some(self.context.clone()));
        self.strategy.0.on_stop_with_context(&stop_context)
    }

    fn supports_exact_orders(&self) -> bool {
        true
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
            }
        };
        let decision = self.strategy.on_event(&self.context, &event)?;
        decision
            .to_orders(&self.context)
            .map_err(|error| error.to_string())
    }

    fn on_order_book_actions_with_margin(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
        _open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<qx_xingban::OrderBookActions, String> {
        self.context.available_margin_raw = available_margin_raw;
        let mut orders = std::mem::take(&mut self.pending_orders);
        orders.extend(self.on_order_book(snapshot, position)?);
        Ok(qx_xingban::OrderBookActions {
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
    ) -> Result<qx_xingban::OrderBookActions, String> {
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
        self.deliver_fill_update(order, fill, None)
    }

    fn on_fill_with_event_seq(
        &mut self,
        order: &Order,
        fill: &qx_core::Fill,
        event_seq: u64,
    ) -> Result<(), qx_core::QxError> {
        self.deliver_fill_update(order, fill, Some(event_seq))
    }
}

impl CallbackOrderBookStrategy {
    fn deliver_fill_update(
        &mut self,
        order: &Order,
        fill: &qx_core::Fill,
        event_seq: Option<u64>,
    ) -> Result<(), qx_core::QxError> {
        let owned = order
            .trace
            .as_ref()
            .and_then(|trace| trace.strategy_id.as_deref())
            == Some(self.context.strategy_id.as_str());
        if !owned {
            return Ok(());
        }
        self.context.as_of = self.context.as_of.max(fill.ts);
        let key = order.instrument.to_string();
        let current_raw = self
            .context
            .positions
            .get(&key)
            .copied()
            .unwrap_or_default();
        let current_exact = self
            .context
            .positions_exact
            .get(&key)
            .copied()
            .or_else(|| ExactDecimal::from_parts(current_raw, 9).ok())
            .ok_or_else(|| qx_core::QxError::Invariant("策略 exact 持仓初始化失败".into()))?;
        let quantity = match fill.exact_amounts {
            Some(amounts) => amounts.quantity,
            None => ExactDecimal::from_parts(fill.qty.raw(), 9)
                .map_err(|error| qx_core::QxError::Invariant(error.into()))?,
        };
        let signed_quantity = if order.side == qx_core::Side::Buy {
            quantity
        } else {
            quantity
                .checked_neg()
                .ok_or_else(|| qx_core::QxError::Invariant("策略 exact 成交取反溢出".into()))?
        };
        let position_exact = current_exact
            .checked_add(signed_quantity)
            .ok_or_else(|| qx_core::QxError::Invariant("策略 exact 持仓累计溢出".into()))?;
        let position_raw = position_exact
            .checked_mul_at_scale(ExactDecimal::ONE, 9, qx_core::DecimalRounding::TowardZero)
            .and_then(|value| value.to_raw(9).ok())
            .ok_or_else(|| qx_core::QxError::Invariant("策略 exact 持仓投影失败".into()))?;
        self.context.positions.insert(key.clone(), position_raw);
        self.context.positions_exact.insert(key, position_exact);

        let decision = if let Some(event_seq) = event_seq {
            let update =
                StrategyFillUpdateV2::from_fill(order, fill, &self.context.strategy_id, event_seq)
                    .map_err(qx_core::QxError::BusinessViolation)?;
            self.strategy
                .0
                .on_strategy_fill_update_v2(&self.context, &update)
        } else {
            let update = StrategyUpdate::from_fill(order, fill, &self.context.strategy_id)
                .map_err(qx_core::QxError::BusinessViolation)?;
            self.strategy.0.on_strategy_update(&self.context, &update)
        }
        .map_err(qx_core::QxError::BusinessViolation)?;
        if let Some(decision) = decision {
            self.pending_orders.extend(
                decision
                    .to_orders(&self.context)
                    .map_err(qx_core::QxError::BusinessViolation)?,
            );
        }
        Ok(())
    }
}

impl Strategy for CallbackStrategy {
    fn on_init(&mut self, context: &StrategyContext) -> Result<(), String> {
        self.0.on_init(context)
    }

    fn history_requirements(&self) -> Result<qx_strategy::StrategyHistoryRequirements, String> {
        self.0.history_requirements()
    }

    fn on_warmup_complete(
        &mut self,
        context: &StrategyContext,
        instrument: Option<&InstrumentId>,
        observed_bars: u64,
    ) -> Result<(), String> {
        self.0
            .on_warmup_complete(context, instrument, observed_bars)
    }

    fn on_start(&mut self, context: &StrategyContext) -> Result<(), String> {
        self.0.on_start(context)
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

    fn on_strategy_update(
        &mut self,
        context: &StrategyContext,
        update: &StrategyUpdate,
    ) -> Result<Option<qx_strategy::StrategyDecision>, String> {
        self.0.on_strategy_update(context, update)
    }

    fn on_strategy_fill_update_v2(
        &mut self,
        context: &StrategyContext,
        update: &StrategyFillUpdateV2,
    ) -> Result<Option<qx_strategy::StrategyDecision>, String> {
        self.0.on_strategy_fill_update_v2(context, update)
    }

    fn on_strategy_order_status(
        &mut self,
        context: &StrategyContext,
        update: &StrategyOrderStatusUpdate,
    ) -> Result<Option<qx_strategy::StrategyDecision>, String> {
        self.0.on_strategy_order_status(context, update)
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.0.on_stop()
    }

    fn on_stop_with_context(
        &mut self,
        stop: &qx_strategy::StrategyStopContextV1,
    ) -> Result<(), String> {
        self.0.on_stop_with_context(stop)
    }

    fn diagnostic_state(&self) -> Option<serde_json::Value> {
        self.0.diagnostic_state()
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
            Ok(InjectedDepthStrategy {
                identity,
                strategy,
                experiment_parameters: false,
            })
        })
        .collect()
}
