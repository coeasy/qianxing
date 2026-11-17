use super::{DepthBarStrategy, NativeTickStrategy, TickStrategy};
use qx_core::Order;
use qx_guanxing::QuoteTick;
use qx_strategy::{
    MarketEvent, Strategy, StrategyDecision, StrategyErrorStage, StrategyStopContextV1,
    StrategyStopReason,
};

impl<S: Strategy> TickStrategy for NativeTickStrategy<S> {
    fn on_start(&mut self) -> Result<(), String> {
        self.initialize()?;
        let requirements = self
            .history_requirements
            .ok_or_else(|| "NativeTickStrategy history requirements are missing".to_string())?;
        if requirements.warmup_bars > 0 {
            return Err(
                "NativeTickStrategy 不支持 warmup_bars；该字段按 Bar 计数，不能按 Tick 数替代"
                    .into(),
            );
        }
        let result = self.strategy.on_start(&self.context);
        qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::Start,
            Some(self.context.as_of),
            Some(&self.instrument),
            result,
        )?;
        if !self.zero_warmup_notified {
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
        let stop_context = StrategyStopContextV1::new(reason, Some(self.context.clone()));
        let result = self.strategy.on_stop_with_context(&stop_context);
        qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::Stop,
            Some(self.context.as_of),
            Some(&self.instrument),
            result,
        )
    }

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
        let result = self.strategy.on_event(&self.context, &event);
        let decision: StrategyDecision = qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::MarketEvent,
            Some(tick.ts),
            Some(&self.instrument),
            result,
        )?;
        let orders = decision.to_orders(&self.context);
        qx_strategy::notify_strategy_callback_result(
            &mut self.strategy,
            &self.context,
            StrategyErrorStage::MarketEvent,
            Some(tick.ts),
            Some(&self.instrument),
            orders,
        )
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
    fn on_start(&mut self) -> Result<(), String> {
        crate::OrderBookStrategy::on_start(self)
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.on_stop_with_reason(StrategyStopReason::HostStop)
    }

    fn on_stop_with_reason(&mut self, reason: StrategyStopReason) -> Result<(), String> {
        crate::OrderBookStrategy::on_stop_with_reason(self, reason)
    }

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
