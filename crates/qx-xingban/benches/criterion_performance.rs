//! Criterion benchmarks for order-book matching and queue depth lookup.

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use qx_core::{InstrumentId, Order, OrderStatus, Price, Quantity, Side};
use qx_guanxing::{Bar, QualityGate, Verdict};
use qx_xingban::{
    AshareCorporateActionEvent, AshareCorporateActionType, AshareRuleConfig, BookLevel,
    OrderBookExecutionModel, OrderBookMatchingEngine, OrderBookSnapshot, PortfolioBarEvent,
    PortfolioBarSeries, PortfolioBarTimeline, QueuePositionModel,
};
use std::collections::{BTreeMap, BTreeSet};
use std::hint::black_box;

const SIZES: [usize; 3] = [1_000, 8_000, 64_000];

fn bench_l2_matching(c: &mut Criterion) {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").expect("valid instrument");
    let mut group = c.benchmark_group("l2_matching");
    for size in SIZES {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_function(BenchmarkId::from_parameter(size), |b| {
            b.iter(|| {
                let mut matcher = OrderBookMatchingEngine::new(0).expect("valid fee model");
                for index in 0..size {
                    let id = index as u64 + 1;
                    matcher
                        .submit(Order {
                            client_id: id,
                            instrument: instrument.clone(),
                            side: Side::Buy,
                            qty: Quantity::from_i64(1),
                            limit: None,
                            status: OrderStatus::PendingSubmit,
                            filled: Quantity::ZERO,
                            account_id: "criterion".into(),
                            trace: None,
                            policy: None,
                        })
                        .expect("valid deterministic order");
                    let snapshot = OrderBookSnapshot {
                        instrument: instrument.clone(),
                        ts: id,
                        sequence: id,
                        bids: vec![BookLevel {
                            price: Price::from_i64(99),
                            qty: Quantity::from_i64(1),
                        }],
                        asks: vec![BookLevel {
                            price: Price::from_i64(100),
                            qty: Quantity::from_i64(1),
                        }],
                    };
                    black_box(
                        matcher
                            .on_snapshot(black_box(&snapshot))
                            .expect("valid deterministic book"),
                    );
                }
            });
        });
    }
    group.finish();
}

fn bench_l2_queue_position_depth_lookup(c: &mut Criterion) {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").expect("valid instrument");
    let mut group = c.benchmark_group("l2_queue_position_depth_lookup");
    for depth in [16_usize, 128, 512] {
        let model = OrderBookExecutionModel {
            fee_bps: 0,
            latency_snapshots: 0,
            queue_position_bps: 1_000,
            queue_position_model: QueuePositionModel::FifoVolumeEstimate {
                volume_advance_bps: 0,
            },
            market_impact_bps: 0,
        };
        let snapshot = OrderBookSnapshot {
            instrument: instrument.clone(),
            ts: 1,
            sequence: 1,
            bids: (0..depth)
                .map(|index| BookLevel {
                    price: Price::from_i64(10_000 - index as i64),
                    qty: Quantity::from_i64(100),
                })
                .collect(),
            asks: (0..depth)
                .map(|index| BookLevel {
                    price: Price::from_i64(20_000 + index as i64),
                    qty: Quantity::from_i64(100),
                })
                .collect(),
        };
        group.throughput(Throughput::Elements(depth as u64));
        group.bench_function(BenchmarkId::from_parameter(depth), |b| {
            b.iter_batched(
                || {
                    let mut matcher = OrderBookMatchingEngine::with_model(model)
                        .expect("valid queue-position model");
                    for index in 0..depth {
                        matcher
                            .submit(Order {
                                client_id: index as u64 + 1,
                                instrument: instrument.clone(),
                                side: Side::Buy,
                                qty: Quantity::from_i64(1),
                                limit: Some(snapshot.bids[index].price),
                                status: OrderStatus::PendingSubmit,
                                filled: Quantity::ZERO,
                                account_id: "criterion-queue".into(),
                                trace: None,
                                policy: None,
                            })
                            .expect("valid passive order");
                    }
                    (matcher, snapshot.clone())
                },
                |(mut matcher, snapshot)| {
                    black_box(
                        matcher
                            .on_snapshot(black_box(&snapshot))
                            .expect("valid passive queue snapshot"),
                    );
                },
                BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

fn queue_level_quantity(
    levels: &[BookLevel],
    limit: Price,
    side: Side,
    binary: bool,
) -> Option<i128> {
    if binary {
        let index = match side {
            Side::Buy => levels.binary_search_by(|level| limit.raw().cmp(&level.price.raw())),
            Side::Sell => levels.binary_search_by(|level| level.price.raw().cmp(&limit.raw())),
        };
        index.ok().map(|index| levels[index].qty.raw())
    } else {
        levels
            .iter()
            .find(|level| level.price == limit)
            .map(|level| level.qty.raw())
    }
}

fn bench_l2_queue_price_lookup_algorithm(c: &mut Criterion) {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").expect("valid instrument");
    let mut group = c.benchmark_group("l2_queue_price_lookup_algorithm");
    for depth in [16_usize, 128, 512, 2_048] {
        let snapshot = OrderBookSnapshot {
            instrument: instrument.clone(),
            ts: 1,
            sequence: 1,
            bids: (0..depth)
                .map(|index| BookLevel {
                    price: Price::from_i64(10_000 - index as i64),
                    qty: Quantity::from_i64(index as i64 + 1),
                })
                .collect(),
            asks: (0..depth)
                .map(|index| BookLevel {
                    price: Price::from_i64(20_000 + index as i64),
                    qty: Quantity::from_i64(index as i64 + 1),
                })
                .collect(),
        };
        for (side, levels) in [(Side::Buy, &snapshot.bids), (Side::Sell, &snapshot.asks)] {
            let limits: Vec<Price> = levels.iter().rev().map(|level| level.price).collect();
            let linear: Vec<Option<i128>> = limits
                .iter()
                .map(|limit| queue_level_quantity(levels, *limit, side, false))
                .collect();
            let binary: Vec<Option<i128>> = limits
                .iter()
                .map(|limit| queue_level_quantity(levels, *limit, side, true))
                .collect();
            assert_eq!(linear, binary, "线性与二分盘口档位结果必须逐项相等");
            group.throughput(Throughput::Elements(depth as u64));
            for (name, use_binary) in [("linear", false), ("binary", true)] {
                group.bench_function(format!("{name}/{side:?}/{depth}"), |b| {
                    b.iter(|| {
                        let quantities: i128 = limits
                            .iter()
                            .filter_map(|limit| {
                                queue_level_quantity(levels, *limit, side, use_binary)
                            })
                            .sum();
                        black_box(quantities)
                    });
                });
            }
        }
    }
    group.finish();
}

fn ashare_minute_bars(size: usize) -> Vec<Bar> {
    const BARS_PER_DAY: usize = 240;
    const MINUTE_MS: u64 = 60_000;
    const DAY_MS: u64 = 86_400_000;
    const FIRST_BAR_TS: u64 = 1_704_159_000_000; // 2024-01-02 09:30 Asia/Shanghai

    (0..size)
        .map(|index| {
            let day = index / BARS_PER_DAY;
            let minute = index % BARS_PER_DAY;
            let session_minute = if minute < 120 { minute } else { minute + 90 };
            let ts = FIRST_BAR_TS + day as u64 * DAY_MS + session_minute as u64 * MINUTE_MS;
            let close = 10 * qx_core::SCALE + (index as i128 % 31 - 15) * 10_000_000;
            Bar::new(
                ts,
                close,
                close + 10_000_000,
                close - 10_000_000,
                close,
                1_000,
            )
        })
        .collect()
}

fn bench_ashare_previous_close(c: &mut Criterion) {
    let rules = AshareRuleConfig::default();
    let mut group = c.benchmark_group("ashare_previous_close");
    for size in SIZES {
        let bars = ashare_minute_bars(size);
        let pointwise: Vec<_> = (0..bars.len())
            .map(|index| rules.previous_close(&bars, index))
            .collect();
        let precomputed = rules.previous_close_series(&bars);
        assert_eq!(
            pointwise, precomputed,
            "A-share anchors must remain equivalent"
        );

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::new("pointwise", size), &bars, |b, bars| {
            b.iter(|| {
                let anchors: Vec<_> = (0..bars.len())
                    .map(|index| rules.previous_close(black_box(bars), index))
                    .collect();
                black_box(anchors);
            });
        });
        group.bench_with_input(BenchmarkId::new("precomputed", size), &bars, |b, bars| {
            b.iter(|| black_box(rules.previous_close_series(black_box(bars))));
        });
    }
    group.finish();

    let mut group = c.benchmark_group("ashare_ex_rights_daily_sweep");
    for size in SIZES {
        let bars = ashare_minute_bars(size);
        let mut rules = AshareRuleConfig::default();
        let day_count = bars.len().div_ceil(240);
        let action_count = 4_096;
        let actions_per_day = action_count / day_count;
        let extra_actions = action_count % day_count;
        rules.corporate_actions = (0..day_count)
            .flat_map(|day| {
                let count = actions_per_day + usize::from(day < extra_actions);
                let ts = bars[day * 240].ts - 9 * 60 * 60 * 1_000 - 30 * 60 * 1_000;
                (0..count).map(move |_| AshareCorporateActionEvent {
                    // The fixture starts at 09:30; convert each session's first bar to Shanghai
                    // midnight so the action is visible to the same ex-date lookup as production.
                    ts,
                    action_type: AshareCorporateActionType::CashDividend,
                    cash_dividend_raw: 10_000_000,
                    ..AshareCorporateActionEvent::default()
                })
            })
            .collect();
        let legacy = ashare_previous_close_series_legacy(&rules, &bars);
        let indexed = rules.previous_close_series(&bars);
        assert_eq!(legacy, indexed, "ex-rights anchors must remain equivalent");

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            BenchmarkId::new("legacy_daily_scan", size),
            &bars,
            |b, bars| {
                b.iter(|| {
                    black_box(ashare_previous_close_series_legacy(
                        black_box(&rules),
                        black_box(bars),
                    ));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("production_adaptive", size),
            &bars,
            |b, bars| {
                b.iter(|| black_box(rules.previous_close_series(black_box(bars))));
            },
        );
    }
    group.finish();
}

fn ashare_previous_close_series_legacy(
    rules: &AshareRuleConfig,
    bars: &[Bar],
) -> Vec<Option<i128>> {
    let mut output = Vec::with_capacity(bars.len());
    let mut current_day = None;
    let mut adjusted_previous_close = None;
    for (index, bar) in bars.iter().enumerate() {
        let day = AshareRuleConfig::day_key(bar.ts);
        if current_day != Some(day) {
            adjusted_previous_close = current_day.and_then(|_| rules.previous_close(bars, index));
            current_day = Some(day);
        }
        output.push(
            rules
                .previous_close_raw
                .get(&bar.ts)
                .copied()
                .or(adjusted_previous_close),
        );
    }
    output
}

fn bench_portfolio_bar_timeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("portfolio_bar_timeline_64k_bars");
    for market_count in [2_usize, 8, 32, 64, 128] {
        let bars_per_market = 64_000 / market_count;
        let instruments = (0..market_count)
            .map(|index| {
                InstrumentId::parse(&format!("ASSET{index:02}USDT.BINANCE"))
                    .expect("valid benchmark instrument")
            })
            .collect::<Vec<_>>();
        let bars = (0..market_count)
            .map(|market| {
                (0..bars_per_market)
                    .map(|index| {
                        Bar::new((index * market_count + market) as u64, 100, 101, 99, 100, 1)
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let series = instruments
            .iter()
            .enumerate()
            .map(|(index, instrument)| PortfolioBarSeries {
                instrument,
                priority: index as u32,
                bars: &bars[index],
            })
            .collect::<Vec<_>>();
        let mut expected = series
            .iter()
            .flat_map(|input| {
                input.bars.iter().map(|bar| PortfolioBarEvent {
                    instrument: input.instrument,
                    priority: input.priority,
                    bar,
                })
            })
            .collect::<Vec<_>>();
        expected.sort_by(|left, right| {
            left.bar
                .ts
                .cmp(&right.bar.ts)
                .then_with(|| left.priority.cmp(&right.priority))
                .then_with(|| left.instrument.cmp(right.instrument))
        });
        let timeline = PortfolioBarTimeline::new(&series).expect("valid benchmark timeline");
        assert_eq!(
            timeline
                .events()
                .iter()
                .map(|event| (event.instrument, event.priority, event.bar.ts))
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|event| (event.instrument, event.priority, event.bar.ts))
                .collect::<Vec<_>>(),
            "k-way merge must preserve full-sort timestamp/priority/instrument ordering"
        );
        group.throughput(Throughput::Elements(64_000));
        group.bench_with_input(
            BenchmarkId::new("timeline_constructor", market_count),
            &series,
            |b, series| {
                b.iter(|| black_box(PortfolioBarTimeline::new(black_box(series)).unwrap()));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("flatten_then_sort", market_count),
            &series,
            |b, series| {
                b.iter(|| {
                    let mut seen = BTreeSet::new();
                    for input in series {
                        assert!(seen.insert(input.instrument));
                        assert!(!matches!(
                            QualityGate::check(input.bars).verdict(),
                            Verdict::Fail | Verdict::Quarantine
                        ));
                    }
                    let mut events = series
                        .iter()
                        .flat_map(|input| {
                            input.bars.iter().map(|bar| PortfolioBarEvent {
                                instrument: input.instrument,
                                priority: input.priority,
                                bar,
                            })
                        })
                        .collect::<Vec<_>>();
                    events.sort_by(|left, right| {
                        left.bar
                            .ts
                            .cmp(&right.bar.ts)
                            .then_with(|| left.priority.cmp(&right.priority))
                            .then_with(|| left.instrument.cmp(right.instrument))
                    });
                    black_box(events);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("preallocated_flatten_then_sort", market_count),
            &series,
            |b, series| {
                b.iter(|| {
                    let mut seen = BTreeSet::new();
                    let mut event_count = 0_usize;
                    for input in series {
                        assert!(seen.insert(input.instrument));
                        assert!(!matches!(
                            QualityGate::check(input.bars).verdict(),
                            Verdict::Fail | Verdict::Quarantine
                        ));
                        event_count += input.bars.len();
                    }
                    let mut events = Vec::with_capacity(event_count);
                    for input in series {
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
                    black_box(events);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("preallocated_flatten_sort_with_indexes", market_count),
            &series,
            |b, series| {
                b.iter(|| {
                    let mut seen = BTreeSet::new();
                    let mut event_count = 0_usize;
                    for input in series {
                        assert!(seen.insert(input.instrument));
                        assert!(!matches!(
                            QualityGate::check(input.bars).verdict(),
                            Verdict::Fail | Verdict::Quarantine
                        ));
                        event_count += input.bars.len();
                    }
                    let mut events = Vec::with_capacity(event_count);
                    for (series_index, input) in series.iter().enumerate() {
                        events.extend(input.bars.iter().enumerate().map(|(bar_index, bar)| {
                            (
                                PortfolioBarEvent {
                                    instrument: input.instrument,
                                    priority: input.priority,
                                    bar,
                                },
                                (series_index, bar_index),
                            )
                        }));
                    }
                    events.sort_by(|(left, _), (right, _)| {
                        left.bar
                            .ts
                            .cmp(&right.bar.ts)
                            .then_with(|| left.priority.cmp(&right.priority))
                            .then_with(|| left.instrument.cmp(right.instrument))
                    });
                    let mut ordered_events = Vec::with_capacity(events.len());
                    let mut ordered_indexes = Vec::with_capacity(events.len());
                    for (event, index) in events {
                        ordered_events.push(event);
                        ordered_indexes.push(index);
                    }
                    black_box((ordered_events, ordered_indexes));
                });
            },
        );
    }
    group.finish();
}

fn bench_aligned_portfolio_bar_timeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("portfolio_bar_timeline_aligned_64k_bars");
    for market_count in [2_usize, 8, 32] {
        let bars_per_market = 64_000 / market_count;
        let instruments = (0..market_count)
            .map(|index| {
                InstrumentId::parse(&format!("ASSET{index:02}USDT.BINANCE"))
                    .expect("valid benchmark instrument")
            })
            .collect::<Vec<_>>();
        let bars = (0..market_count)
            .map(|_| {
                (0..bars_per_market)
                    .map(|index| Bar::new(index as u64, 100, 101, 99, 100, 1))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let series = instruments
            .iter()
            .enumerate()
            .map(|(index, instrument)| PortfolioBarSeries {
                instrument,
                priority: (index % 2) as u32,
                bars: &bars[index],
            })
            .collect::<Vec<_>>();

        let mut expected = series
            .iter()
            .enumerate()
            .flat_map(|(series_index, input)| {
                input.bars.iter().enumerate().map(move |(bar_index, bar)| {
                    (
                        PortfolioBarEvent {
                            instrument: input.instrument,
                            priority: input.priority,
                            bar,
                        },
                        (series_index, bar_index),
                    )
                })
            })
            .collect::<Vec<_>>();
        expected.sort_by(|(left, _), (right, _)| {
            left.bar
                .ts
                .cmp(&right.bar.ts)
                .then_with(|| left.priority.cmp(&right.priority))
                .then_with(|| left.instrument.cmp(right.instrument))
        });
        let timeline = PortfolioBarTimeline::new(&series).expect("valid aligned timeline");
        assert_eq!(timeline.events().len(), expected.len());
        assert!(timeline
            .events()
            .iter()
            .zip(&expected)
            .all(|(actual, (expected, _))| {
                actual.instrument == expected.instrument
                    && actual.priority == expected.priority
                    && actual.bar.ts == expected.bar.ts
                    && std::ptr::eq(actual.bar, expected.bar)
            }));

        group.throughput(Throughput::Elements(64_000));
        group.bench_with_input(
            BenchmarkId::new("timeline_constructor", market_count),
            &series,
            |b, series| {
                b.iter(|| black_box(PortfolioBarTimeline::new(black_box(series)).unwrap()));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("preallocated_flatten_sort_with_indexes", market_count),
            &series,
            |b, series| {
                b.iter(|| {
                    let mut seen = BTreeSet::new();
                    let mut event_count = 0_usize;
                    for input in series {
                        assert!(seen.insert(input.instrument));
                        assert!(!matches!(
                            QualityGate::check(input.bars).verdict(),
                            Verdict::Fail | Verdict::Quarantine
                        ));
                        event_count += input.bars.len();
                    }
                    let mut events = Vec::with_capacity(event_count);
                    for (series_index, input) in series.iter().enumerate() {
                        events.extend(input.bars.iter().enumerate().map(|(bar_index, bar)| {
                            (
                                PortfolioBarEvent {
                                    instrument: input.instrument,
                                    priority: input.priority,
                                    bar,
                                },
                                (series_index, bar_index),
                            )
                        }));
                    }
                    events.sort_by(|(left, _), (right, _)| {
                        left.bar
                            .ts
                            .cmp(&right.bar.ts)
                            .then_with(|| left.priority.cmp(&right.priority))
                            .then_with(|| left.instrument.cmp(right.instrument))
                    });
                    let mut ordered_events = Vec::with_capacity(events.len());
                    let mut ordered_indexes = Vec::with_capacity(events.len());
                    for (event, index) in events {
                        ordered_events.push(event);
                        ordered_indexes.push(index);
                    }
                    black_box((ordered_events, ordered_indexes));
                });
            },
        );
    }
    group.finish();
}

fn bench_protective_exit_position_side_iteration(c: &mut Criterion) {
    let mut group = c.benchmark_group("protective_exit_position_sides_per_64k_bars");
    for sides in [
        vec![qx_core::PositionSide::Net],
        vec![qx_core::PositionSide::Long, qx_core::PositionSide::Short],
    ] {
        let policies = sides
            .into_iter()
            .map(|side| (side, ()))
            .collect::<BTreeMap<_, _>>();
        let fallback = None;
        let expected = policies.len();
        let old_count = policies.keys().copied().collect::<Vec<_>>().len();
        let new_count = policies.keys().copied().chain(fallback).count();
        assert_eq!(old_count, expected);
        assert_eq!(new_count, expected);
        group.bench_with_input(
            BenchmarkId::new("allocate_vec_per_bar", expected),
            &policies,
            |b, policies| {
                b.iter(|| {
                    let mut visited = 0_usize;
                    for _ in 0..64_000 {
                        let active = policies.keys().copied().collect::<Vec<_>>();
                        for side in active {
                            black_box(side);
                            visited += 1;
                        }
                    }
                    black_box(visited)
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("borrow_keys_per_bar", expected),
            &policies,
            |b, policies| {
                b.iter(|| {
                    let mut visited = 0_usize;
                    for _ in 0..64_000 {
                        for side in policies.keys().copied().chain(fallback) {
                            black_box(side);
                            visited += 1;
                        }
                    }
                    black_box(visited)
                });
            },
        );
    }
    group.finish();
}

fn visible_marks_snapshot(
    instruments: &[InstrumentId],
    bars_by_market: &[Vec<Bar>],
    visible_ts: u64,
) -> BTreeMap<InstrumentId, Price> {
    instruments
        .iter()
        .zip(bars_by_market)
        .filter_map(|(instrument, bars)| {
            let end = bars.partition_point(|bar| bar.ts <= visible_ts);
            end.checked_sub(1)
                .and_then(|index| bars.get(index))
                .map(|bar| (instrument.clone(), Price::from_raw(bar.close)))
        })
        .collect()
}

fn bench_portfolio_visible_marks_reuse(c: &mut Criterion) {
    let mut group = c.benchmark_group("portfolio_visible_marks_aligned");
    for market_count in [2_usize, 8, 32, 64] {
        let instruments = (0..market_count)
            .map(|index| {
                InstrumentId::parse(&format!("ASSET{index:02}USDT.BINANCE"))
                    .expect("valid benchmark instrument")
            })
            .collect::<Vec<_>>();
        let bars_by_market = (0..market_count)
            .map(|market| {
                (0..128)
                    .map(|index| {
                        let close = 100 + market as i128 + index as i128;
                        Bar::new(index as u64, close, close + 1, close - 1, close, 1)
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let visible_ts = 63;
        let expected = visible_marks_snapshot(&instruments, &bars_by_market, visible_ts);
        for _ in 1..market_count {
            assert_eq!(
                visible_marks_snapshot(&instruments, &bars_by_market, visible_ts),
                expected,
                "identical visible timestamps must produce identical portfolio marks"
            );
        }

        group.throughput(Throughput::Elements(market_count as u64));
        group.bench_with_input(
            BenchmarkId::new("rebuild_each_admission_batch", market_count),
            &market_count,
            |b, &batch_count| {
                b.iter(|| {
                    let mut observed = 0_i128;
                    for batch in 0..batch_count {
                        let marks = visible_marks_snapshot(
                            black_box(&instruments),
                            black_box(&bars_by_market),
                            visible_ts,
                        );
                        observed = observed.saturating_add(
                            marks
                                .get(&instruments[batch])
                                .expect("each market has a visible mark")
                                .raw(),
                        );
                    }
                    black_box(observed);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("reuse_same_visible_timestamp", market_count),
            &market_count,
            |b, &batch_count| {
                b.iter(|| {
                    let mut cache = None;
                    let mut observed = 0_i128;
                    for batch in 0..batch_count {
                        if cache.as_ref().map(|(cached_ts, _)| *cached_ts) != Some(visible_ts) {
                            cache = Some((
                                visible_ts,
                                visible_marks_snapshot(&instruments, &bars_by_market, visible_ts),
                            ));
                        }
                        let marks = &cache.as_ref().expect("marks cache populated").1;
                        observed = observed.saturating_add(
                            black_box(marks)
                                .get(&instruments[batch])
                                .expect("each market has a visible mark")
                                .raw(),
                        );
                    }
                    black_box(observed);
                });
            },
        );
    }
    group.finish();
}

fn bench_portfolio_funding_reservation_cache(c: &mut Criterion) {
    let mut group = c.benchmark_group("portfolio_funding_reservation_decision_reads");
    for market_count in [2_usize, 8, 32, 64] {
        for pending_per_market in [16_usize, 256] {
            let reservations = (0..market_count)
                .map(|market| {
                    (0..pending_per_market)
                        .map(|order| ((market + 1) * (order + 3)) as i128 * 1_000)
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let market_totals = reservations
                .iter()
                .map(|orders| orders.iter().map(|value| *value as u128).sum::<u128>())
                .collect::<Vec<_>>();
            let cached_total = market_totals.iter().sum::<u128>();
            let expected_total = reservations
                .iter()
                .flat_map(|orders| orders.iter())
                .fold(0_i128, |total, value| total.saturating_add(*value));
            assert_eq!(
                expected_total,
                cached_total.min(i128::MAX as u128) as i128,
                "cached whole-portfolio total must match the saturating reference"
            );
            for (own_market, market_total) in market_totals.iter().enumerate() {
                let expected_other = reservations
                    .iter()
                    .enumerate()
                    .filter(|(market, _)| *market != own_market)
                    .flat_map(|(_, orders)| orders.iter())
                    .fold(0_i128, |total, value| total.saturating_add(*value));
                let cached_other = cached_total
                    .saturating_sub(*market_total)
                    .min(i128::MAX as u128) as i128;
                assert_eq!(expected_other, cached_other);
            }

            group.throughput(Throughput::Elements(market_count as u64));
            let id = format!("{market_count}x{pending_per_market}");
            group.bench_with_input(
                BenchmarkId::new("scan_markets_and_orders_per_decision", &id),
                &market_count,
                |b, &decisions| {
                    b.iter(|| {
                        let mut observed = 0_i128;
                        for own_market in 0..decisions {
                            let total = reservations
                                .iter()
                                .flat_map(|orders| orders.iter())
                                .fold(0_i128, |sum, value| sum.saturating_add(*value));
                            let other = reservations
                                .iter()
                                .enumerate()
                                .filter(|(market, _)| *market != own_market)
                                .flat_map(|(_, orders)| orders.iter())
                                .fold(0_i128, |sum, value| sum.saturating_add(*value));
                            observed = observed.saturating_add(total).saturating_add(other);
                        }
                        black_box(observed)
                    });
                },
            );
            group.bench_with_input(
                BenchmarkId::new("read_cached_global_and_market_totals", &id),
                &market_count,
                |b, &decisions| {
                    b.iter(|| {
                        let mut observed = 0_i128;
                        for market_total in market_totals.iter().take(decisions) {
                            let total = cached_total.min(i128::MAX as u128) as i128;
                            let other = cached_total
                                .saturating_sub(*market_total)
                                .min(i128::MAX as u128)
                                as i128;
                            observed = observed.saturating_add(total).saturating_add(other);
                        }
                        black_box(observed)
                    });
                },
            );
        }
    }
    group.finish();
}

fn margin_contribution(position: (i128, i128, i128), mark: i128) -> i128 {
    let (net, long, short) = position;
    let qty = if long != 0 || short != 0 {
        long.abs() + short.abs()
    } else {
        net.abs()
    };
    qty * mark / 10
}

fn bench_portfolio_margin_snapshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("portfolio_margin_admission_snapshot");
    for market_count in [2_usize, 8, 32, 64] {
        let positions = (0..market_count)
            .map(|market| {
                let qty = market as i128 + 1;
                (qty, qty / 2, -(qty - qty / 2))
            })
            .collect::<Vec<_>>();
        let marks = (0..market_count)
            .map(|market| 100 + market as i128)
            .collect::<Vec<_>>();
        let contributions = positions
            .iter()
            .zip(&marks)
            .map(|(position, mark)| margin_contribution(*position, *mark))
            .collect::<Vec<_>>();
        let total = contributions.iter().sum::<i128>();
        let expected = (0..market_count)
            .map(|target| {
                positions
                    .iter()
                    .zip(&marks)
                    .enumerate()
                    .filter(|(index, _)| *index != target)
                    .map(|(_, (position, mark))| margin_contribution(*position, *mark))
                    .sum::<i128>()
            })
            .collect::<Vec<_>>();
        let cached = (0..market_count)
            .map(|target| total - contributions[target])
            .collect::<Vec<_>>();
        assert_eq!(expected, cached, "cached margin must match per-target scan");

        group.throughput(Throughput::Elements(market_count as u64));
        group.bench_with_input(
            BenchmarkId::new("recalculate_positions_for_each_admission", market_count),
            &market_count,
            |b, &decisions| {
                b.iter(|| {
                    let mut observed = 0_i128;
                    for target in 0..decisions {
                        let other = positions
                            .iter()
                            .zip(&marks)
                            .enumerate()
                            .filter(|(index, _)| *index != target)
                            .map(|(_, (position, mark))| margin_contribution(*position, *mark))
                            .sum::<i128>();
                        observed = observed.saturating_add(other);
                    }
                    black_box(observed)
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("reuse_margin_snapshot_for_admissions", market_count),
            &market_count,
            |b, &decisions| {
                b.iter(|| {
                    let snapshot = positions
                        .iter()
                        .zip(&marks)
                        .map(|(position, mark)| margin_contribution(*position, *mark))
                        .collect::<Vec<_>>();
                    let total = snapshot.iter().sum::<i128>();
                    let mut observed = 0_i128;
                    for contribution in snapshot.iter().take(decisions) {
                        observed = observed.saturating_add(total - contribution);
                    }
                    black_box(observed)
                });
            },
        );
    }
    group.finish();
}

fn configure_criterion() -> Criterion {
    Criterion::default()
        .sample_size(10)
        .warm_up_time(std::time::Duration::from_secs(1))
        .measurement_time(std::time::Duration::from_secs(2))
}

criterion_group! {
    name = benches;
    config = configure_criterion();
    targets = bench_l2_matching, bench_l2_queue_position_depth_lookup,
        bench_l2_queue_price_lookup_algorithm, bench_ashare_previous_close,
        bench_portfolio_bar_timeline, bench_aligned_portfolio_bar_timeline,
        bench_protective_exit_position_side_iteration, bench_portfolio_visible_marks_reuse,
        bench_portfolio_funding_reservation_cache, bench_portfolio_margin_snapshot
}
criterion_main!(benches);
