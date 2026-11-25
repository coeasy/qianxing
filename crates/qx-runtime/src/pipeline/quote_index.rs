//! Latest L1 quote projections for latency-sensitive Paper and recovery paths.
//!
//! The EventLog remains the source of truth. This derived index keeps lookups scoped to the
//! requested instrument instead of scanning an ever-growing account history on every order.

use qx_core::{Event, EventKind, InstrumentId};
use qx_guanxing::QuoteTick;
use std::collections::BTreeMap;
use std::sync::OnceLock;

const PAPER_EXECUTION_MARKET_PREFIX: &str = "paper-execution:market:";

#[derive(Clone, Default)]
pub(super) struct RuntimeQuoteIndex {
    by_instrument: OnceLock<BTreeMap<InstrumentId, LatestQuoteState>>,
}

#[derive(Clone, Copy)]
struct LatestQuoteState {
    latest: QuoteTick,
    latest_received: Option<(QuoteTick, u64)>,
}

impl RuntimeQuoteIndex {
    pub(super) fn remember(&mut self, event: &Event) {
        let Some(by_instrument) = self.by_instrument.get_mut() else {
            // Build this projection only after a caller asks for a latest quote.
            return;
        };
        Self::remember_in(by_instrument, event);
    }

    fn remember_in(by_instrument: &mut BTreeMap<InstrumentId, LatestQuoteState>, event: &Event) {
        let EventKind::MarketQuote {
            instrument,
            bid,
            ask,
            bid_qty,
            ask_qty,
            bid_exact,
            ask_exact,
            bid_qty_exact,
            ask_qty_exact,
        } = &event.kind
        else {
            return;
        };

        let quote = QuoteTick::new(event.ts, *bid, *bid_qty, *ask, *ask_qty, event.source_seq)
            .with_optional_exact_prices(*bid_exact, *ask_exact)
            .with_optional_exact_sizes(*bid_qty_exact, *ask_qty_exact);

        // Paper execution records the exact market quote used by a simulated fill. It is useful
        // audit evidence and remains visible through `latest_quote`, but it is not a new venue
        // observation and must not extend the source quote's freshness window.
        let received = (!event
            .correlation_id
            .starts_with(PAPER_EXECUTION_MARKET_PREFIX))
        .then_some((quote, event.receive_time));
        if let Some(state) = by_instrument.get_mut(instrument) {
            state.latest = quote;
            if let Some(received) = received {
                state.latest_received = Some(received);
            }
        } else {
            by_instrument.insert(
                instrument.clone(),
                LatestQuoteState {
                    latest: quote,
                    latest_received: received,
                },
            );
        }
    }

    fn index(&self, events: &[Event]) -> &BTreeMap<InstrumentId, LatestQuoteState> {
        self.by_instrument.get_or_init(|| {
            let mut by_instrument = BTreeMap::new();
            for event in events {
                Self::remember_in(&mut by_instrument, event);
            }
            by_instrument
        })
    }

    pub(super) fn latest_quote(
        &self,
        events: &[Event],
        instrument: &InstrumentId,
    ) -> Option<QuoteTick> {
        self.index(events).get(instrument).map(|state| state.latest)
    }

    pub(super) fn latest_received_quote(
        &self,
        events: &[Event],
        instrument: &InstrumentId,
    ) -> Option<(QuoteTick, u64)> {
        self.index(events)
            .get(instrument)
            .and_then(|state| state.latest_received)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{Event, EventKind, InstrumentId, Price, Priority, Quantity};

    fn quote_event(
        seq: u64,
        ts: u64,
        instrument: InstrumentId,
        correlation_id: &str,
        bid: i64,
    ) -> Event {
        Event::new(
            seq,
            ts,
            Priority::MARKET,
            EventKind::MarketQuote {
                instrument,
                bid: Price::from_i64(bid),
                ask: Price::from_i64(bid + 1),
                bid_qty: Quantity::from_i64(2),
                ask_qty: Quantity::from_i64(3),
                bid_exact: None,
                ask_exact: None,
                bid_qty_exact: None,
                ask_qty_exact: None,
            },
        )
        .received_at(ts + 10)
        .sourced_by(seq + 100)
        .correlated(correlation_id)
    }

    #[test]
    fn quote_index_is_per_instrument_and_execution_echo_keeps_source_freshness() {
        let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let eth = InstrumentId::parse("ETHUSDT.BINANCE").unwrap();
        let mut index = RuntimeQuoteIndex::default();

        let btc_first = quote_event(0, 1, btc.clone(), "feed:btc-1", 100);
        let eth_first = quote_event(1, 2, eth.clone(), "feed:eth-1", 200);
        let execution_echo = quote_event(2, 3, btc.clone(), "paper-execution:market:run-1", 300);

        index.remember(&btc_first);
        assert!(index.by_instrument.get().is_none());
        let history = [btc_first, eth_first, execution_echo];

        let latest = index.latest_quote(&history, &btc).unwrap();
        assert_eq!(latest.ts, 3);
        assert_eq!(latest.bid, Price::from_i64(300));
        let (received, receive_time) = index.latest_received_quote(&history, &btc).unwrap();
        assert_eq!(received.ts, 1);
        assert_eq!(received.bid, Price::from_i64(100));
        assert_eq!(receive_time, 11);
        assert_eq!(
            index.latest_quote(&history, &eth).unwrap().bid,
            Price::from_i64(200)
        );

        index.remember(&quote_event(3, 4, btc.clone(), "feed:btc-2", 400));
        let (received, receive_time) = index.latest_received_quote(&history, &btc).unwrap();
        assert_eq!(received.ts, 4);
        assert_eq!(received.bid, Price::from_i64(400));
        assert_eq!(receive_time, 14);
    }
}
