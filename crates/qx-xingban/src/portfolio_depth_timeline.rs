//! Deterministic event ordering for independently sampled L1/L2 market books.

use crate::OrderBookSnapshot;
use qx_core::InstrumentId;
use std::collections::BTreeSet;

pub(crate) const MIN_DEPTH_MARKETS: usize = 2;
pub(crate) const MAX_DEPTH_MARKETS: usize = 32;
pub(crate) const MAX_DEPTH_EVENTS: usize = 2_000_000;

#[derive(Clone)]
pub(crate) struct PortfolioDepthSeries<'a> {
    pub(crate) instrument: InstrumentId,
    pub(crate) priority: u32,
    pub(crate) snapshots: &'a [OrderBookSnapshot],
}

#[derive(Debug)]
pub(crate) struct PortfolioDepthEvent<'a> {
    pub(crate) instrument: InstrumentId,
    pub(crate) priority: u32,
    pub(crate) snapshot: &'a OrderBookSnapshot,
}

#[derive(Debug)]
pub(crate) struct PortfolioDepthTimeline<'a> {
    events: Vec<PortfolioDepthEvent<'a>>,
}

impl<'a> PortfolioDepthTimeline<'a> {
    pub(crate) fn new(series: &[PortfolioDepthSeries<'a>]) -> Result<Self, String> {
        if !(MIN_DEPTH_MARKETS..=MAX_DEPTH_MARKETS).contains(&series.len()) {
            return Err(format!(
                "共享深度时间线市场数量必须在 {MIN_DEPTH_MARKETS}..={MAX_DEPTH_MARKETS}"
            ));
        }
        let mut instruments = BTreeSet::new();
        let event_count = series
            .iter()
            .try_fold(0_usize, |count, market| {
                count.checked_add(market.snapshots.len())
            })
            .ok_or_else(|| "共享深度时间线事件数量溢出".to_string())?;
        if event_count > MAX_DEPTH_EVENTS {
            return Err(format!("共享深度时间线超过事件上限 {MAX_DEPTH_EVENTS}"));
        }
        let mut events = Vec::with_capacity(event_count);
        for market in series {
            if !instruments.insert(market.instrument.clone()) {
                return Err(format!("共享深度时间线标的重复: {}", market.instrument));
            }
            if market.snapshots.len() < 2 {
                return Err(format!(
                    "共享深度时间线市场 {} 至少需要两份盘口快照",
                    market.instrument
                ));
            }
            for (index, snapshot) in market.snapshots.iter().enumerate() {
                snapshot.validate()?;
                if snapshot.instrument != market.instrument {
                    return Err(format!(
                        "共享深度时间线快照标的不一致: {} != {}",
                        snapshot.instrument, market.instrument
                    ));
                }
                if index > 0 {
                    let previous = &market.snapshots[index - 1];
                    if snapshot.ts <= previous.ts || snapshot.sequence <= previous.sequence {
                        return Err(format!(
                            "共享深度时间线 {} 的 ts/sequence 必须严格递增",
                            market.instrument
                        ));
                    }
                }
                events.push(PortfolioDepthEvent {
                    instrument: market.instrument.clone(),
                    priority: market.priority,
                    snapshot,
                });
            }
        }
        events.sort_by(|left, right| {
            left.snapshot
                .ts
                .cmp(&right.snapshot.ts)
                .then_with(|| left.priority.cmp(&right.priority))
                .then_with(|| left.instrument.cmp(&right.instrument))
                .then_with(|| left.snapshot.sequence.cmp(&right.snapshot.sequence))
        });
        Ok(Self { events })
    }

    pub(crate) fn events(&self) -> &[PortfolioDepthEvent<'a>] {
        &self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BookLevel;
    use qx_core::{Price, Quantity};

    fn instrument(value: &str) -> InstrumentId {
        InstrumentId::parse(value).unwrap()
    }

    fn book(instrument: &InstrumentId, ts: u64, sequence: u64) -> OrderBookSnapshot {
        OrderBookSnapshot {
            instrument: instrument.clone(),
            ts,
            sequence,
            bids: vec![BookLevel {
                price_exact: None,
                qty_exact: None,
                price: Price::from_i64(99),
                qty: Quantity::from_i64(2),
            }],
            asks: vec![BookLevel {
                price_exact: None,
                qty_exact: None,
                price: Price::from_i64(100),
                qty: Quantity::from_i64(2),
            }],
        }
    }

    #[test]
    fn interleaves_unaligned_books_deterministically() {
        let btc = instrument("BTCUSDT.BINANCE");
        let eth = instrument("ETHUSDT.BYBIT");
        let btc_books = [book(&btc, 10, 1), book(&btc, 30, 2)];
        let eth_books = [book(&eth, 10, 1), book(&eth, 20, 2)];
        let series = [
            PortfolioDepthSeries {
                instrument: eth.clone(),
                priority: 0,
                snapshots: &eth_books,
            },
            PortfolioDepthSeries {
                instrument: btc.clone(),
                priority: 1,
                snapshots: &btc_books,
            },
        ];

        let timeline = PortfolioDepthTimeline::new(&series).unwrap();
        let events = timeline
            .events()
            .iter()
            .map(|event| (event.snapshot.ts, event.instrument.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(
            events,
            vec![
                (10, "ETHUSDT.BYBIT".into()),
                (10, "BTCUSDT.BINANCE".into()),
                (20, "ETHUSDT.BYBIT".into()),
                (30, "BTCUSDT.BINANCE".into()),
            ]
        );
        let reversed =
            PortfolioDepthTimeline::new(&[series[1].clone(), series[0].clone()]).unwrap();
        assert_eq!(
            events,
            reversed
                .events()
                .iter()
                .map(|event| (event.snapshot.ts, event.instrument.to_string()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn rejects_duplicate_markets_and_non_monotonic_book_sequences() {
        let btc = instrument("BTCUSDT.BINANCE");
        let valid = [book(&btc, 10, 1), book(&btc, 20, 2)];
        let duplicate = [
            PortfolioDepthSeries {
                instrument: btc.clone(),
                priority: 0,
                snapshots: &valid,
            },
            PortfolioDepthSeries {
                instrument: btc.clone(),
                priority: 1,
                snapshots: &valid,
            },
        ];
        assert!(PortfolioDepthTimeline::new(&duplicate)
            .unwrap_err()
            .contains("标的重复"));

        let eth = instrument("ETHUSDT.BYBIT");
        let invalid = [book(&eth, 20, 2), book(&eth, 10, 1)];
        let series = [
            PortfolioDepthSeries {
                instrument: btc.clone(),
                priority: 0,
                snapshots: &valid,
            },
            PortfolioDepthSeries {
                instrument: eth.clone(),
                priority: 1,
                snapshots: &invalid,
            },
        ];
        assert!(PortfolioDepthTimeline::new(&series)
            .unwrap_err()
            .contains("严格递增"));
    }
}
