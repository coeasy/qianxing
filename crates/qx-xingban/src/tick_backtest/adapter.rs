use super::TickStrategy;
use crate::{OrderBookActions, OrderBookSnapshot, OrderBookStrategy};
use qx_core::{InstrumentId, Order};
use qx_guanxing::QuoteTick;

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
