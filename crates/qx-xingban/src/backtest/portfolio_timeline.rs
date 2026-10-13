//! Deterministic event ordering for multiple Bar streams.

use qx_core::InstrumentId;
use qx_guanxing::{Bar, QualityGate, Verdict};
use std::collections::BTreeSet;

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
    instruments: Vec<&'a InstrumentId>,
}

impl<'a> PortfolioBarTimeline<'a> {
    pub fn new(series: &[PortfolioBarSeries<'a>]) -> Result<Self, String> {
        if series.len() < 2 {
            return Err("组合 Bar 时间线至少需要两个不同标的".into());
        }
        let mut seen = BTreeSet::new();
        let mut instruments = Vec::with_capacity(series.len());
        let mut events = Vec::new();
        for input in series {
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
            events.extend(input.bars.iter().map(|bar| PortfolioBarEvent {
                instrument: input.instrument,
                priority: input.priority,
                bar,
            }));
        }
        events.sort_by(|left, right| {
            left.bar
                .ts
                .cmp(&right.bar.ts)
                .then_with(|| left.priority.cmp(&right.priority))
                .then_with(|| left.instrument.cmp(right.instrument))
        });
        instruments.sort();
        Ok(Self {
            events,
            instruments,
        })
    }

    pub fn events(&self) -> &[PortfolioBarEvent<'a>] {
        &self.events
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
