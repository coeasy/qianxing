//! Latest order-book EventLog positions by instrument.
//!
//! L1 quote values and freshness are indexed by `RuntimeQuoteIndex`; this positional index keeps
//! duplicate order-book checks independent of total account/order history. Positions point into
//! the append-only EventLog and are rebuilt by replay.

use qx_core::{Event, EventKind, InstrumentId};
use std::collections::BTreeMap;

#[derive(Clone, Default)]
pub(super) struct RuntimeMarketFactIndex {
    latest_order_books: BTreeMap<InstrumentId, usize>,
}

impl RuntimeMarketFactIndex {
    pub(super) fn remember(&mut self, event: &Event, event_index: usize) {
        if let EventKind::MarketOrderBook { instrument, .. } = &event.kind {
            self.latest_order_books.insert(instrument.clone(), event_index);
        }
    }

    pub(super) fn latest_order_book_index(&self, instrument: &InstrumentId) -> Option<usize> {
        self.latest_order_books.get(instrument).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{MarketDepthLevel, Price, Priority, Quantity, VenueId};

    fn instrument(symbol: &str) -> InstrumentId {
        InstrumentId::new(symbol, VenueId::new("BINANCE")).expect("valid instrument")
    }

    fn order_book(instrument: InstrumentId, sequence: u64) -> Event {
        Event::new(
            sequence,
            sequence,
            Priority::COMMAND,
            EventKind::MarketOrderBook {
                instrument,
                sequence,
                bids: vec![MarketDepthLevel {
                    price: Price::from_i64(100),
                    quantity: Quantity::from_i64(1),
                    price_exact: None,
                    quantity_exact: None,
                }],
                asks: Vec::new(),
            },
        )
    }

    #[test]
    fn order_book_index_is_instrument_scoped_and_keeps_the_latest_fact() {
        let btc = instrument("BTCUSDT");
        let eth = instrument("ETHUSDT");
        let mut index = RuntimeMarketFactIndex::default();

        index.remember(&order_book(btc.clone(), 1), 0);
        index.remember(&order_book(eth.clone(), 2), 1);
        index.remember(&order_book(btc.clone(), 3), 2);

        assert_eq!(index.latest_order_book_index(&btc), Some(2));
        assert_eq!(index.latest_order_book_index(&eth), Some(1));
        assert_eq!(index.latest_order_book_index(&instrument("SOLUSDT")), None);
    }
}
