//! Criterion suite for the four WP-0.2 kernel measurements.

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use qx_core::{
    Event, EventKind, InstrumentId, LedgerEntry, LedgerEntryKind, Money, Price, Priority, Quantity,
    ReplayVerifier, Side,
};
use qx_guanxing::{Bar, QuoteTick};
use qx_runtime::{LiveEventPipeline, RuntimeEventEnvelope};
use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const SIZES: [usize; 3] = [1_000, 8_000, 64_000];
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn events(size: usize) -> Vec<Event> {
    (0..size)
        .map(|seq| {
            Event::new(
                seq as u64,
                seq as u64 + 1,
                Priority::POST,
                EventKind::Settle,
            )
        })
        .collect()
}

fn deterministic_adjustment_entries(size: usize) -> Vec<LedgerEntry> {
    (0..size)
        .map(|id| LedgerEntry {
            id: id as u64,
            account_id: "criterion-ledger".into(),
            currency: "USD".into(),
            kind: LedgerEntryKind::Adjustment,
            amount: Money::from_raw(1),
            instrument: None,
            quantity: Quantity::ZERO,
            price: None,
            order_id: None,
            ts: id as u64,
            multiplier: 1,
            position_side: None,

            leverage: None,
            exact_amounts: None,
        })
        .collect()
}

fn quote_events(size: usize) -> Vec<RuntimeEventEnvelope> {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").expect("valid instrument");
    (0..size)
        .map(|index| {
            let ts = index as u64 + 1;
            let quote = QuoteTick::new(
                ts,
                Price::from_i64(99),
                Quantity::from_i64(10),
                Price::from_i64(100),
                Quantity::from_i64(10),
                ts,
            );
            RuntimeEventEnvelope::market_quote(
                instrument.clone(),
                quote,
                ts,
                ts,
                format!("criterion-{ts}"),
            )
        })
        .collect()
}

fn bench_full_replay(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_replay");
    for size in SIZES {
        let batch = events(size);
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &batch, |b, batch| {
            b.iter(|| {
                let (log, ledger) =
                    ReplayVerifier::replay(black_box(batch)).expect("valid deterministic replay");
                black_box((log.digest(), ledger.entries().len()));
            });
        });
    }
    group.finish();
}

fn ledger_events(size: usize) -> Vec<Event> {
    deterministic_adjustment_entries(size)
        .into_iter()
        .map(|entry| {
            Event::new(
                entry.id,
                entry.ts + 1,
                Priority::POST,
                EventKind::LedgerApplied { entry },
            )
        })
        .collect()
}

fn bench_ledger_replay(c: &mut Criterion) {
    let mut group = c.benchmark_group("ledger_replay");
    for size in SIZES {
        let batch = ledger_events(size);
        let (borrowed_log, borrowed_ledger) =
            ReplayVerifier::replay(&batch).expect("valid deterministic borrowed ledger replay");
        let (expected_log, expected_ledger) =
            ReplayVerifier::replay_owned(batch.clone()).expect("valid deterministic ledger replay");
        assert_eq!(borrowed_log.events(), expected_log.events());
        assert_eq!(borrowed_ledger.entries(), expected_ledger.entries());
        assert_eq!(
            borrowed_ledger.cash_for("criterion-ledger", "USD"),
            expected_ledger.cash_for("criterion-ledger", "USD")
        );
        assert_eq!(expected_log.events().len(), size);
        assert_eq!(expected_ledger.entries().len(), size);
        assert_eq!(
            expected_ledger.cash_for("criterion-ledger", "USD"),
            size as i128
        );
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            BenchmarkId::new("borrowed_events", size),
            &batch,
            |b, batch| {
                b.iter(|| {
                    let (log, ledger) = ReplayVerifier::replay(black_box(batch))
                        .expect("valid deterministic borrowed replay");
                    black_box((log.digest(), ledger.entries().len()));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("owned_events", size),
            &batch,
            |b, batch| {
                b.iter_batched(
                    || batch.clone(),
                    |events| {
                        let (log, ledger) = ReplayVerifier::replay_owned(black_box(events))
                            .expect("valid deterministic replay");
                        black_box((log.digest(), ledger.entries().len()));
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

struct SqliteFixture {
    root: PathBuf,
    database: PathBuf,
    expected_digest: u64,
}

impl SqliteFixture {
    fn new(size: usize) -> Self {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::current_dir()
            .expect("read current directory")
            .join(format!(
                ".codex-tmp-criterion-{}-{sequence}",
                std::process::id()
            ));
        std::fs::create_dir(&root).expect("create Criterion SQLite fixture directory");
        let database = root.join("events.sqlite");
        let mut writer = LiveEventPipeline::open_sqlite(&database, "criterion", "USDT")
            .expect("open benchmark writer");
        let events = quote_events(size);
        for event in &events {
            writer
                .ingest(event.clone())
                .expect("append benchmark quote");
        }
        let expected_digest = writer.log().digest();
        drop(writer);
        Self {
            root,
            database,
            expected_digest,
        }
    }

    fn with_stale_reader(size: usize) -> (Self, LiveEventPipeline) {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::current_dir()
            .expect("read current directory")
            .join(format!(
                ".codex-tmp-criterion-{}-{sequence}",
                std::process::id()
            ));
        std::fs::create_dir(&root).expect("create Criterion SQLite fixture directory");
        let database = root.join("events.sqlite");
        let mut writer = LiveEventPipeline::open_sqlite(&database, "criterion", "USDT")
            .expect("open benchmark writer");
        let reader = LiveEventPipeline::open_sqlite_read_only(&database, "criterion", "USDT")
            .expect("open stale reader");
        for event in quote_events(size) {
            writer.ingest(event).expect("append benchmark quote");
        }
        let fixture = Self {
            root,
            database,
            expected_digest: writer.log().digest(),
        };
        drop(writer);
        (fixture, reader)
    }
}

impl Drop for SqliteFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn bench_full_vs_incremental_replay(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_vs_incremental_replay");
    group.sample_size(10);
    let size = 1_000;
    group.throughput(Throughput::Elements(size as u64));

    group.bench_function("incremental_refresh", |b| {
        b.iter_batched(
            || SqliteFixture::with_stale_reader(size),
            |(fixture, mut reader)| {
                reader.refresh().expect("incremental refresh");
                assert_eq!(reader.log().digest(), fixture.expected_digest);
                (reader, fixture)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("full_replay", |b| {
        b.iter_batched(
            || SqliteFixture::new(size),
            |fixture| {
                let reader = LiveEventPipeline::open_sqlite_read_only(
                    &fixture.database,
                    "criterion",
                    "USDT",
                )
                .expect("full replay");
                assert_eq!(reader.log().digest(), fixture.expected_digest);
                (reader, fixture)
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn performance_equity(periods: usize) -> Vec<i128> {
    let mut equity = Vec::with_capacity(periods + 1);
    let mut value = 1_000_000_000_000_000_i128;
    equity.push(value);
    for index in 0..periods {
        let change_bps = match index % 7 {
            0 | 3 => -23_i128,
            1 | 5 => 17,
            2 => -8,
            4 => 31,
            _ => 6,
        };
        value += value * change_bps / 10_000;
        equity.push(value);
    }
    equity
}

fn legacy_allocating_risk_metrics(
    equity: &[i128],
    return_bps: i32,
    max_drawdown_bps: u32,
) -> qx_core::performance::BacktestRiskMetrics {
    use qx_core::performance::BacktestRiskMetrics;

    let returns: Vec<f64> = equity
        .windows(2)
        .filter_map(|pair| {
            let previous = pair[0] as f64;
            (previous != 0.0).then(|| (pair[1] as f64 - previous) / previous)
        })
        .collect();
    if returns.len() < 2 {
        return BacktestRiskMetrics::default();
    }
    let count = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / count;
    let sample_std = (returns
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / (count - 1.0))
        .sqrt();
    let downside_returns: Vec<f64> = returns.iter().map(|value| value.min(0.0).powi(2)).collect();
    let downside_std = (downside_returns.iter().sum::<f64>() / (count - 1.0)).sqrt();
    let wins = returns.iter().filter(|value| **value > 0.0).count() as f64;
    let gross_profit = returns.iter().filter(|value| **value > 0.0).sum::<f64>();
    let gross_loss = returns
        .iter()
        .filter(|value| **value < 0.0)
        .map(|value| value.abs())
        .sum::<f64>();
    BacktestRiskMetrics {
        sharpe: (sample_std > 0.0).then_some(mean / sample_std),
        sortino: (downside_std > 0.0).then_some(mean / downside_std),
        calmar: (max_drawdown_bps > 0).then_some(f64::from(return_bps) / max_drawdown_bps as f64),
        win_rate: Some(wins / count),
        profit_factor: (gross_loss > 0.0).then_some(gross_profit / gross_loss),
    }
}

fn bench_backtest_risk_metrics(c: &mut Criterion) {
    use qx_core::performance::backtest_risk_metrics;

    let mut group = c.benchmark_group("backtest_risk_metrics");
    for size in SIZES {
        let equity = performance_equity(size);
        let expected = legacy_allocating_risk_metrics(&equity, 137, 420);
        assert_eq!(backtest_risk_metrics(&equity, 137, 420), expected);
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            BenchmarkId::new("optimized_single_vector", size),
            &equity,
            |b, equity| {
                b.iter(|| black_box(backtest_risk_metrics(black_box(equity), 137, 420)));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("legacy_allocating", size),
            &equity,
            |b, equity| {
                b.iter(|| black_box(legacy_allocating_risk_metrics(black_box(equity), 137, 420)));
            },
        );
    }
    group.finish();
}

fn bench_benchmark_relative_columns(c: &mut Criterion) {
    let mut group = c.benchmark_group("benchmark_relative_columns");
    for size in SIZES {
        let portfolio = (0..size)
            .map(|index| (index as u64, 100_000_000_i128 + index as i128 * 101))
            .collect::<Vec<_>>();
        let timestamps = (0..size).map(|index| index as u64).collect::<Vec<_>>();
        let closes = (0..size)
            .map(|index| 4_000_000_000_i128 + index as i128 * 37)
            .collect::<Vec<_>>();
        let pairs = timestamps
            .iter()
            .copied()
            .zip(closes.iter().copied())
            .collect::<Vec<_>>();
        let legacy = legacy_two_pass_benchmark_metrics(&portfolio, &pairs, 17)
            .expect("valid aligned legacy benchmark series");
        let expected = qx_core::benchmark_relative_metrics(&portfolio, &pairs, 17)
            .expect("valid aligned benchmark series");
        assert_eq!(expected, legacy);
        assert_eq!(
            qx_core::benchmark_relative_metrics_from_columns(&portfolio, &timestamps, &closes, 17)
                .expect("valid columnar benchmark series"),
            legacy
        );
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            BenchmarkId::new("legacy_two_pass_tuple_prebuilt", size),
            &(&portfolio, &pairs),
            |b, (portfolio, pairs)| {
                b.iter(|| {
                    black_box(legacy_two_pass_benchmark_metrics(
                        black_box(portfolio),
                        black_box(pairs),
                        17,
                    ))
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("tuple_materialization", size),
            &(&portfolio, &timestamps, &closes),
            |b, (portfolio, timestamps, closes)| {
                b.iter(|| {
                    let pairs = timestamps
                        .iter()
                        .copied()
                        .zip(closes.iter().copied())
                        .collect::<Vec<_>>();
                    black_box(qx_core::benchmark_relative_metrics(
                        black_box(portfolio),
                        black_box(&pairs),
                        17,
                    ))
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("columnar_no_tuple_buffer", size),
            &(&portfolio, &timestamps, &closes),
            |b, (portfolio, timestamps, closes)| {
                b.iter(|| {
                    black_box(qx_core::benchmark_relative_metrics_from_columns(
                        black_box(portfolio),
                        black_box(timestamps),
                        black_box(closes),
                        17,
                    ))
                });
            },
        );
    }
    group.finish();
}

fn legacy_two_pass_benchmark_metrics(
    portfolio: &[(u64, i128)],
    benchmark: &[(u64, i128)],
    portfolio_return_bps: i64,
) -> Result<qx_core::BenchmarkRelativeMetrics, String> {
    if portfolio.len() < 2 || benchmark.len() < 2 {
        return Err("基准对照至少需要两个严格对齐的权益/收盘价观测".into());
    }
    if portfolio.len() != benchmark.len() {
        return Err(format!(
            "基准观测数量不匹配: portfolio={} benchmark={}",
            portfolio.len(),
            benchmark.len()
        ));
    }
    for (index, (portfolio_row, benchmark_row)) in portfolio.iter().zip(benchmark).enumerate() {
        if portfolio_row.0 != benchmark_row.0 {
            return Err(format!(
                "基准时间戳必须逐项精确对齐: index={index} portfolio={} benchmark={}",
                portfolio_row.0, benchmark_row.0
            ));
        }
        if portfolio_row.1 <= 0 || benchmark_row.1 <= 0 {
            return Err(format!("基准对照权益与收盘价必须为正数: index={index}"));
        }
    }
    let benchmark_return_bps =
        ((benchmark.last().unwrap().1 as f64 / benchmark.first().unwrap().1 as f64 - 1.0)
            * 10_000.0)
            .round() as i64;
    let active_return_bps = portfolio_return_bps
        .checked_sub(benchmark_return_bps)
        .ok_or_else(|| "组合超额收益 bps 溢出".to_string())?;
    let mut active_returns_bps = Vec::with_capacity(portfolio.len() - 1);
    let mut mean = 0.0;
    let mut squared_deviation = 0.0;
    for (portfolio_pair, benchmark_pair) in portfolio.windows(2).zip(benchmark.windows(2)) {
        let portfolio_return = portfolio_pair[1].1 as f64 / portfolio_pair[0].1 as f64 - 1.0;
        let benchmark_return = benchmark_pair[1].1 as f64 / benchmark_pair[0].1 as f64 - 1.0;
        let value = (portfolio_return - benchmark_return) * 10_000.0;
        let count = (active_returns_bps.len() + 1) as f64;
        let delta = value - mean;
        mean += delta / count;
        squared_deviation += delta * (value - mean);
        active_returns_bps.push(value);
    }
    let tracking_error_bps = (active_returns_bps.len() > 1)
        .then(|| (squared_deviation / (active_returns_bps.len() - 1) as f64).sqrt());
    let information_ratio = tracking_error_bps
        .filter(|value| *value > 0.0)
        .map(|value| mean / value);
    Ok(qx_core::BenchmarkRelativeMetrics {
        benchmark_return_bps,
        active_return_bps,
        mean_active_return_bps: Some(mean),
        tracking_error_bps,
        information_ratio,
        aligned_periods: active_returns_bps.len(),
        active_returns_bps,
    })
}

fn bench_strategy_decision_order_buffer(c: &mut Criterion) {
    use qx_strategy::{StrategyContext, StrategyDecision, StrategyOrderIntent};

    let instrument = InstrumentId::parse("600000.XSHG").expect("valid A-share instrument");
    let context = StrategyContext {
        strategy_id: "buffer-bench".into(),
        strategy_version: "v1".into(),
        account_id: "backtest".into(),
        venue_id: "XSHG".into(),
        data_fingerprint: "bench-bars".into(),
        as_of: 10,
        positions_exact: std::collections::BTreeMap::new(),
        cash_exact: std::collections::BTreeMap::new(),
        available_margin_exact: None,
        positions: BTreeMap::new(),
        cash: BTreeMap::from([("CNY".into(), 1_000_000_000)]),
        available_margin_raw: Some(1_000_000_000),
        risk_state: "backtest-verified".into(),
    };
    let mut group = c.benchmark_group("strategy_decision_order_buffer");
    for decision_count in [1_usize, 4, 16] {
        let decisions = (0..decision_count)
            .map(|index| StrategyDecision {
                schema_version: qx_strategy::STRATEGY_API_VERSION,
                request_id: format!("bench-{index}"),
                strategy_id: context.strategy_id.clone(),
                signal_id: index as u64 + 1,
                confidence: 10_000,
                priority: 0,
                expires_at: 0,
                intents: vec![StrategyOrderIntent {
                    intent_id: index as u64 + 1,
                    instrument: instrument.clone(),
                    side: Side::Buy,
                    qty: Quantity::from_i64(100),
                    limit: Some(Price::from_i64(10)),
                    policy: None,
                    reduce_only: false,
                    post_only: false,

                    qty_exact: None,
                    limit_price_exact: None,
                    quote_budget: None,
                }],
            })
            .collect::<Vec<_>>();
        let mut legacy = Vec::new();
        for decision in &decisions {
            legacy.extend(decision.to_orders(&context).expect("valid decision"));
        }
        let mut appended = Vec::new();
        for decision in &decisions {
            decision
                .append_orders(&context, &mut appended)
                .expect("valid decision");
        }
        assert_eq!(appended, legacy, "buffer path must preserve order output");

        group.bench_with_input(
            BenchmarkId::new("per_decision_vec", decision_count),
            &decisions,
            |b, decisions| {
                b.iter(|| {
                    let mut orders = Vec::new();
                    for decision in decisions {
                        orders.extend(decision.to_orders(black_box(&context)).expect("valid"));
                    }
                    black_box(orders);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("shared_output_buffer", decision_count),
            &decisions,
            |b, decisions| {
                b.iter(|| {
                    let mut orders = Vec::new();
                    for decision in decisions {
                        decision
                            .append_orders(black_box(&context), &mut orders)
                            .expect("valid");
                    }
                    black_box(orders);
                });
            },
        );
    }
    group.finish();
}

fn bench_strategy_close_scratch_reuse(c: &mut Criterion) {
    let mut group = c.benchmark_group("strategy_close_scratch_reuse");
    for size in [32_usize, 128, 512, 1_024] {
        let bars = (0..size)
            .map(|index| {
                let close = 10_000 + index as i128 * 3;
                Bar::new(index as u64, close, close + 2, close - 2, close, 1_000)
            })
            .collect::<Vec<_>>();
        let expected = bars.iter().map(|bar| bar.close).collect::<Vec<_>>();
        let mut scratch = Vec::with_capacity(size);
        scratch.extend(bars.iter().map(|bar| bar.close));
        assert_eq!(
            scratch, expected,
            "reused close buffer must preserve sequence"
        );

        group.bench_with_input(
            BenchmarkId::new("allocate_each_signal", size),
            &bars,
            |b, bars| {
                b.iter(|| {
                    black_box(
                        black_box(bars)
                            .iter()
                            .map(|bar| bar.close)
                            .collect::<Vec<_>>(),
                    )
                });
            },
        );
        let mut scratch = Vec::with_capacity(size);
        group.bench_with_input(
            BenchmarkId::new("reuse_strategy_buffer", size),
            &bars,
            |b, bars| {
                b.iter(|| {
                    scratch.clear();
                    scratch.extend(black_box(bars).iter().map(|bar| bar.close));
                    black_box(scratch.as_slice());
                });
            },
        );
    }
    group.finish();
}

fn vwap_accumulate<'a>(mut bars: impl Iterator<Item = &'a Bar>) -> Option<(i128, i128)> {
    bars.try_fold((0_i128, 0_i128), |(weighted, volume), bar| {
        Some((
            weighted.checked_add(bar.close.checked_mul(bar.volume)?)?,
            volume.checked_add(bar.volume)?,
        ))
    })
}

fn bench_strategy_vwap_allocation(c: &mut Criterion) {
    let bars = (0..1_024)
        .map(|index| {
            let close = 10_000 + index as i128 * 3;
            Bar::new(
                index as u64,
                close,
                close + 2,
                close - 2,
                close,
                1_000 + index as i128,
            )
        })
        .collect::<Vec<_>>();
    let mut group = c.benchmark_group("strategy_vwap_window_allocation");
    for period in [16_usize, 64, 256] {
        let expected = vwap_accumulate(bars.iter().rev().take(period));
        let collected = bars.iter().rev().take(period).collect::<Vec<_>>();
        assert_eq!(
            vwap_accumulate(collected.into_iter()),
            expected,
            "collected and direct VWAP reductions must match"
        );
        group.bench_with_input(
            BenchmarkId::new("collect_then_reduce", period),
            &period,
            |b, period| {
                b.iter(|| {
                    let values = black_box(&bars)
                        .iter()
                        .rev()
                        .take(*period)
                        .collect::<Vec<_>>();
                    black_box(vwap_accumulate(values.into_iter()));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("direct_reduce", period),
            &period,
            |b, period| {
                b.iter(|| {
                    black_box(vwap_accumulate(black_box(&bars).iter().rev().take(*period)));
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
    targets = bench_full_replay,
        bench_ledger_replay, bench_full_vs_incremental_replay,
        bench_backtest_risk_metrics, bench_benchmark_relative_columns,
        bench_strategy_decision_order_buffer,
    bench_strategy_close_scratch_reuse,
    bench_strategy_vwap_allocation,
}
criterion_main!(benches);
