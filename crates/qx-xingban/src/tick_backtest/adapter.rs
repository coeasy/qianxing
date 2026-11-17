use super::TickStrategy;
use crate::{OrderBookActions, OrderBookSnapshot, OrderBookStrategy};
use qx_core::{InstrumentId, Order};
use qx_guanxing::QuoteTick;
use qx_strategy::StrategyStopReason;

pub(super) struct TickAdapter<'a> {
    pub(super) strategy: &'a mut dyn TickStrategy,
    pub(super) instrument: InstrumentId,
}

impl TickAdapter<'_> {
    fn snapshot_to_tick(&self, snapshot: &OrderBookSnapshot) -> Result<QuoteTick, String> {
        let bid = snapshot.bids.first().ok_or("L1 tick 缺少 bid")?;
        let ask = snapshot.asks.first().ok_or("L1 tick 缺少 ask")?;
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
    fn on_start(&mut self) -> Result<(), String> {
        self.strategy.on_start()
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.strategy.on_stop()
    }

    fn on_stop_with_reason(&mut self, reason: StrategyStopReason) -> Result<(), String> {
        self.strategy.on_stop_with_reason(reason)
    }

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

    fn on_order_book_actions(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
        open_order_ids: &[u64],
    ) -> Result<OrderBookActions, String> {
        if snapshot.instrument != self.instrument {
            return Err("Tick adapter instrument 不一致".into());
        }
        self.strategy
            .on_tick_actions(&self.snapshot_to_tick(snapshot)?, position, open_order_ids)
    }

    fn on_order_book_actions_with_margin(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
        open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        if snapshot.instrument != self.instrument {
            return Err("Tick adapter instrument 不一致".into());
        }
        self.strategy.on_tick_actions_with_margin(
            &self.snapshot_to_tick(snapshot)?,
            position,
            open_order_ids,
            available_margin_raw,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct StopReasonProbe(Arc<Mutex<Option<StrategyStopReason>>>);

    impl super::super::TickStrategy for StopReasonProbe {
        fn on_tick(&mut self, _tick: &QuoteTick, _position: i128) -> Result<Vec<Order>, String> {
            Ok(Vec::new())
        }

        fn on_stop_with_reason(&mut self, reason: StrategyStopReason) -> Result<(), String> {
            *self.0.lock().unwrap() = Some(reason);
            Ok(())
        }
    }

    #[test]
    fn tick_adapter_forwards_stop_reason() {
        let received = Arc::new(Mutex::new(None));
        let mut strategy = StopReasonProbe(Arc::clone(&received));
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let mut adapter = TickAdapter {
            strategy: &mut strategy,
            instrument,
        };

        OrderBookStrategy::on_stop_with_reason(&mut adapter, StrategyStopReason::Cancelled)
            .unwrap();

        assert_eq!(
            *received.lock().unwrap(),
            Some(StrategyStopReason::Cancelled)
        );
    }
}
