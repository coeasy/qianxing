use super::{DepthBarStrategy, NativeTickStrategy, TickStrategy};
use qx_core::Order;
use qx_guanxing::QuoteTick;
use qx_strategy::{MarketEvent, Strategy, StrategyDecision};

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

    fn on_tick_actions_with_margin(
        &mut self,
        tick: &QuoteTick,
        position: i128,
        _open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<crate::OrderBookActions, String> {
        self.context.available_margin_raw = available_margin_raw;
        Ok(crate::OrderBookActions {
            orders: self.on_tick(tick, position)?,
            cancel_client_ids: Vec::new(),
        })
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
            None,
        )
    }

    fn on_tick_actions_with_margin(
        &mut self,
        tick: &QuoteTick,
        position: i128,
        _open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<crate::OrderBookActions, String> {
        let Some(mid) = tick.mid() else {
            return Ok(crate::OrderBookActions::default());
        };
        let instrument = self.instrument.clone();
        Ok(crate::OrderBookActions {
            orders: self.on_mid(
                &instrument,
                tick.ts,
                position,
                mid,
                tick.bid_qty.raw().saturating_add(tick.ask_qty.raw()),
                available_margin_raw,
            )?,
            cancel_client_ids: Vec::new(),
        })
    }
}
