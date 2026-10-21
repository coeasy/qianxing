//! Deterministic event ordering for multiple Bar streams.

use qx_core::InstrumentId;
use qx_guanxing::{Bar, QualityGate, Verdict};
use std::cmp::Reverse;
use std::collections::{binary_heap::PeekMut, BTreeSet, BinaryHeap};

pub struct PortfolioBarSeries<'a> {
    pub instrument: &'a InstrumentId,
    pub priority: u32,
    pub bars: &'a [Bar],
}

pub struct PortfolioBarEvent<'a> {
    pub instrument: &'a InstrumentId,
    pub priority: u32,
    pub bar: &'a Bar,
}

/// Merges independently sampled markets into one deterministic event sequence.
/// Equal timestamps are ordered by declared instrument priority, then canonical
/// `InstrumentId`; input series order therefore cannot change callback ordering.
pub struct PortfolioBarTimeline<'a> {
    events: Vec<PortfolioBarEvent<'a>>,
    event_indexes: Vec<(usize, usize)>,
    instruments: Vec<&'a InstrumentId>,
}

impl<'a> PortfolioBarTimeline<'a> {
    pub fn new(series: &[PortfolioBarSeries<'a>]) -> Result<Self, String> {
        if series.len() < 2 {
            return Err("组合 Bar 时间线至少需要两个不同标的".into());
        }
        let event_count = series
            .iter()
            .try_fold(0_usize, |total, input| total.checked_add(input.bars.len()))
            .ok_or_else(|| "组合 Bar 时间线输入数量溢出".to_owned())?;
        let mut seen = BTreeSet::new();
        let mut instruments = Vec::with_capacity(series.len());
        let mut events = Vec::with_capacity(event_count);
        let mut event_indexes = Vec::with_capacity(event_count);
        let mut merge_heads = BinaryHeap::with_capacity(series.len());
        for (series_index, input) in series.iter().enumerate() {
            if !seen.insert(input.instrument) {
                return Err(format!("组合 Bar 时间线包含重复标的 {}", input.instrument));
            }
            let quality = QualityGate::check(input.bars);
            if matches!(quality.verdict(), Verdict::Fail | Verdict::Quarantine) {
                return Err(format!(
                    "组合 Bar 时间线输入未通过质量门 {}: {:?}",
                    input.instrument, quality.issues
                ));
            }
            instruments.push(input.instrument);
            if series.len() > 2 {
                if let Some(bar) = input.bars.first() {
                    merge_heads.push(Reverse((
                        bar.ts,
                        input.priority,
                        input.instrument,
                        series_index,
                        0_usize,
                    )));
                }
            }
        }
        if series.len() == 2 {
            let (mut left_index, mut right_index) = (0_usize, 0_usize);
            while left_index < series[0].bars.len() && right_index < series[1].bars.len() {
                let left = &series[0];
                let right = &series[1];
                let left_bar = &left.bars[left_index];
                let right_bar = &right.bars[right_index];
                if (left_bar.ts, left.priority, left.instrument)
                    <= (right_bar.ts, right.priority, right.instrument)
                {
                    events.push(PortfolioBarEvent {
                        instrument: left.instrument,
                        priority: left.priority,
                        bar: left_bar,
                    });
                    event_indexes.push((0, left_index));
                    left_index += 1;
                } else {
                    events.push(PortfolioBarEvent {
                        instrument: right.instrument,
                        priority: right.priority,
                        bar: right_bar,
                    });
                    event_indexes.push((1, right_index));
                    right_index += 1;
                }
            }
            while left_index < series[0].bars.len() {
                events.push(PortfolioBarEvent {
                    instrument: series[0].instrument,
                    priority: series[0].priority,
                    bar: &series[0].bars[left_index],
                });
                event_indexes.push((0, left_index));
                left_index += 1;
            }
            while right_index < series[1].bars.len() {
                events.push(PortfolioBarEvent {
                    instrument: series[1].instrument,
                    priority: series[1].priority,
                    bar: &series[1].bars[right_index],
                });
                event_indexes.push((1, right_index));
                right_index += 1;
            }
        } else {
            while let Some(mut head) = merge_heads.peek_mut() {
                let Reverse((_, priority, instrument, series_index, bar_index)) = *head;
                let input = &series[series_index];
                let bar = &input.bars[bar_index];
                events.push(PortfolioBarEvent {
                    instrument,
                    priority,
                    bar,
                });
                event_indexes.push((series_index, bar_index));
                let next_index = bar_index + 1;
                if let Some(next) = input.bars.get(next_index) {
                    *head = Reverse((
                        next.ts,
                        input.priority,
                        input.instrument,
                        series_index,
                        next_index,
                    ));
                } else {
                    let _ = PeekMut::pop(head);
                }
            }
        }
        instruments.sort();
        Ok(Self {
            events,
            event_indexes,
            instruments,
        })
    }

    pub fn events(&self) -> &[PortfolioBarEvent<'a>] {
        &self.events
    }

    pub(crate) fn indexed_events(
        &self,
    ) -> impl Iterator<Item = (usize, usize, &PortfolioBarEvent<'a>)> + '_ {
        self.events
            .iter()
            .zip(&self.event_indexes)
            .map(|(event, (series_index, bar_index))| (*series_index, *bar_index, event))
    }

    pub fn instruments(&self) -> &[&'a InstrumentId] {
        &self.instruments
    }

    pub fn clock_start(&self) -> Option<u64> {
        self.events.first().map(|event| event.bar.ts)
    }

    pub fn clock_end(&self) -> Option<u64> {
        self.events.last().map(|event| event.bar.ts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn instrument(value: &str) -> InstrumentId {
        InstrumentId::parse(value).unwrap()
    }

    #[test]
    fn interleaves_unaligned_markets_and_stably_orders_equal_timestamps() {
        let btc = instrument("BTCUSDT.BINANCE");
        let eth = instrument("ETHUSDT.BINANCE");
        let btc_bars = [
            Bar::new(10, 100, 110, 90, 105, 1),
            Bar::new(30, 105, 120, 100, 115, 1),
        ];
        let eth_bars = [
            Bar::new(10, 200, 210, 190, 205, 1),
            Bar::new(20, 205, 220, 200, 215, 1),
        ];
        let timeline = PortfolioBarTimeline::new(&[
            PortfolioBarSeries {
                instrument: &eth,
                priority: 0,
                bars: &eth_bars,
            },
            PortfolioBarSeries {
                instrument: &btc,
                priority: 1,
                bars: &btc_bars,
            },
        ])
        .unwrap();

        let observed = timeline
            .events()
            .iter()
            .map(|event| (event.bar.ts, event.instrument.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            vec![
                (10, "ETHUSDT.BINANCE".into()),
                (10, "BTCUSDT.BINANCE".into()),
                (20, "ETHUSDT.BINANCE".into()),
                (30, "BTCUSDT.BINANCE".into()),
            ]
        );
        let reversed = PortfolioBarTimeline::new(&[
            PortfolioBarSeries {
                instrument: &btc,
                priority: 1,
                bars: &btc_bars,
            },
            PortfolioBarSeries {
                instrument: &eth,
                priority: 0,
                bars: &eth_bars,
            },
        ])
        .unwrap();
        assert_eq!(
            observed,
            reversed
                .events()
                .iter()
                .map(|event| (event.bar.ts, event.instrument.to_string()))
                .collect::<Vec<_>>()
        );
        assert_eq!(timeline.clock_start(), Some(10));
        assert_eq!(timeline.clock_end(), Some(30));
        assert_eq!(timeline.instruments().len(), 2);
    }

    #[test]
    fn rejects_duplicate_instruments_and_bad_per_instrument_time_order() {
        let btc = instrument("BTCUSDT.BINANCE");
        let valid = [
            Bar::new(10, 100, 110, 90, 105, 1),
            Bar::new(20, 105, 120, 100, 115, 1),
        ];
        assert!(PortfolioBarTimeline::new(&[
            PortfolioBarSeries {
                instrument: &btc,
                priority: 0,
                bars: &valid,
            },
            PortfolioBarSeries {
                instrument: &btc,
                priority: 1,
                bars: &valid,
            },
        ])
        .err()
        .unwrap()
        .contains("重复标的"));

        let eth = instrument("ETHUSDT.BINANCE");
        let invalid = [
            Bar::new(20, 100, 110, 90, 105, 1),
            Bar::new(10, 105, 120, 100, 115, 1),
        ];
        assert!(PortfolioBarTimeline::new(&[
            PortfolioBarSeries {
                instrument: &btc,
                priority: 0,
                bars: &valid,
            },
            PortfolioBarSeries {
                instrument: &eth,
                priority: 1,
                bars: &invalid,
            },
        ])
        .err()
        .unwrap()
        .contains("未通过质量门"));
    }
}
