//! Criterion suite for the four WP-0.2 kernel measurements.

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use qx_core::{
    Event, EventKind, EventLog, Fnv1a, InstrumentId, Ledger, LedgerEntry, LedgerEntryKind, Money,
    Order, OrderStatus, Price, Priority, Quantity, ReplayVerifier, Side,
};
use qx_guanxing::{Bar, QualityGate, QuoteTick, Verdict};
use qx_runtime::{LiveEventPipeline, RuntimeEventEnvelope};
use qx_scheduler::{JobSpec, JobStatus, JobWindow, RetryPolicy, Scheduler, Trigger};
use qx_xingban::{
    AshareCorporateActionEvent, AshareCorporateActionType, AshareRuleConfig, BookLevel,
    OrderBookExecutionModel, OrderBookMatchingEngine, OrderBookSnapshot, PortfolioBarEvent,
    PortfolioBarSeries, PortfolioBarTimeline, QueuePositionModel,
};
use std::collections::{BTreeMap, BTreeSet};
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

fn bench_append_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("append_batch");
    for size in SIZES {
        let batch = events(size);
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &batch, |b, batch| {
            b.iter(|| {
                let mut log = EventLog::new();
                log.append_batch(black_box(batch))
                    .expect("valid deterministic batch");
                black_box(log);
            });
        });
    }
    group.finish();
}

fn bench_digest(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_log_digest");
    for size in SIZES {
        let mut log = EventLog::new();
        log.append_batch(&events(size))
            .expect("valid deterministic batch");
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &log, |b, log| {
            b.iter(|| black_box(log.digest()));
        });
    }
    group.finish();
}

fn legacy_full_log_digest(log: &EventLog) -> u64 {
    let mut hasher = Fnv1a::new();
    hasher.write_u64(log.len() as u64);
    for event in log.events() {
        event.digest(&mut hasher);
    }
    hasher.finish()
}

fn bench_event_log_manifest_digest(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_log_manifest_digest");
    for size in SIZES {
        let mut log = EventLog::new();
        log.append_batch(&events(size))
            .expect("valid deterministic event log");
        assert_eq!(legacy_full_log_digest(&log), log.digest());
        assert_eq!(log.event_digest(), legacy_event_digest(&log));
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            BenchmarkId::new("legacy_full_rehash", size),
            &log,
            |b, log| b.iter(|| black_box(legacy_full_log_digest(black_box(log)))),
        );
        group.bench_with_input(BenchmarkId::new("rolling_v2", size), &log, |b, log| {
            b.iter(|| black_box(black_box(log).event_digest()))
        });
    }
    group.finish();
}

fn legacy_event_digest(log: &EventLog) -> u64 {
    let mut hasher = Fnv1a::new();
    for event in log.events() {
        event.digest(&mut hasher);
    }
    hasher.finish()
}

fn bench_prefix_digest(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_log_prefix_digest");
    for size in SIZES {
        let batch = events(size + 1);
        let mut cached = EventLog::new();
        cached
            .append_batch(&batch[..size])
            .expect("valid deterministic prefix");
        let expected = cached.digest();
        cached
            .append_checked(batch[size].clone())
            .expect("valid deterministic suffix");
        assert_eq!(cached.digest_of_prefix(size), Some(expected));
        assert_eq!(cached.next_seq_of_prefix(size), Some(size as u64));
        assert_eq!(
            legacy_prefix_digest_and_next_seq(&cached, size),
            Some((expected, size as u64))
        );

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            BenchmarkId::new("cached_after_append", size),
            &cached,
            |b, log| {
                b.iter(|| black_box((log.digest_of_prefix(size), log.next_seq_of_prefix(size))));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("prior_two_scans", size),
            &cached,
            |b, log| b.iter(|| black_box(legacy_prefix_digest_and_next_seq(log, size))),
        );
    }
    group.finish();
}

fn legacy_prefix_digest_and_next_seq(log: &EventLog, len: usize) -> Option<(u64, u64)> {
    if len > log.len() {
        return None;
    }
    let mut hasher = Fnv1a::new();
    hasher.write_u64(len as u64);
    for event in &log.events()[..len] {
        event.digest(&mut hasher);
    }
    let next_seq = log.events()[..len]
        .iter()
        .map(|event| event.seq)
        .max()
        .map_or(0, |max_seq| max_seq.saturating_add(1));
    Some((hasher.finish(), next_seq))
}

fn legacy_validate(log: &EventLog) -> bool {
    let mut seen = BTreeSet::new();
    let mut previous = None;
    let mut max_seq = None;
    for event in log.events() {
        if event.metadata.validate().is_err() || !seen.insert(event.seq) {
            return false;
        }
        let key = (event.ts, event.prio, event.seq);
        if previous.is_some_and(|value| key <= value) {
            return false;
        }
        previous = Some(key);
        max_seq = Some(max_seq.map_or(event.seq, |max: u64| max.max(event.seq)));
    }
    let expected_next = match max_seq {
        Some(seq) => match seq.checked_add(1) {
            Some(next) => next,
            None => return false,
        },
        None => 0,
    };
    log.next_seq() == expected_next
}

fn bench_event_log_validate(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_log_validate");
    for size in SIZES {
        let mut log = EventLog::new();
        log.append_batch(&events(size))
            .expect("valid deterministic event log");
        assert!(log.validate().is_ok() && legacy_validate(&log));
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::new("cached", size), &log, |b, log| {
            b.iter(|| black_box(black_box(log).validate().is_ok()));
        });
        group.bench_with_input(
            BenchmarkId::new("duplicate_tree_baseline", size),
            &log,
            |b, log| b.iter(|| black_box(legacy_validate(black_box(log)))),
        );
    }
    group.finish();
}

fn bench_ledger_entry_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("ledger_entry_lookup");
    for size in SIZES {
        let mut ledger = Ledger::new();
        for _ in 0..size {
            ledger
                .deposit("criterion", "USD", Money::from_raw(1), 1)
                .expect("append deterministic ledger entry");
        }
        let id = size as u64 - 1;
        assert_eq!(ledger.entry(id).map(|entry| entry.id), Some(id));
        assert_eq!(
            ledger
                .entries()
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| entry.id),
            Some(id),
            "indexed and linear lookup must return the same entry"
        );

        group.bench_with_input(BenchmarkId::new("indexed", size), &ledger, |b, ledger| {
            b.iter(|| {
                black_box(ledger)
                    .entry(black_box(id))
                    .map(|entry| black_box(entry.id))
            });
        });
        group.bench_with_input(BenchmarkId::new("linear", size), &ledger, |b, ledger| {
            b.iter(|| {
                black_box(ledger)
                    .entries()
                    .iter()
                    .find(|entry| entry.id == black_box(id))
                    .map(|entry| black_box(entry.id))
            });
        });
    }
    group.finish();
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
        })
        .collect()
}

fn apply_ledger_entries(entries: &[LedgerEntry], clone_full_ledger_per_entry: bool) -> Ledger {
    let mut ledger = Ledger::new();
    for entry in entries {
        if clone_full_ledger_per_entry {
            // Reproduce the old apply_entry transaction boundary: clone every historical entry
            // and projection before applying this fact, then replace the old ledger.
            let mut staged = ledger.clone();
            staged
                .apply_entry(entry.clone())
                .expect("valid adjustment entry");
            ledger = staged;
        } else {
            ledger
                .apply_entry(entry.clone())
                .expect("valid adjustment entry");
        }
    }
    ledger
}

fn bench_ledger_entry_commit(c: &mut Criterion) {
    let mut group = c.benchmark_group("ledger_entry_commit");
    for size in [64_usize, 256, 1_024, 8_000, 64_000] {
        let entries = deterministic_adjustment_entries(size);
        if size <= 1_024 {
            let legacy = apply_ledger_entries(&entries, true);
            let current = apply_ledger_entries(&entries, false);
            assert_eq!(legacy.entries(), current.entries());
            assert_eq!(legacy.cash_for("criterion-ledger", "USD"), size as i128);
        }
        group.throughput(Throughput::Elements(size as u64));
        if size <= 1_024 {
            group.bench_with_input(
                BenchmarkId::new("clone_full_ledger_each_entry", size),
                &entries,
                |b, entries| {
                    b.iter(|| black_box(apply_ledger_entries(black_box(entries), true)));
                },
            );
        }
        group.bench_with_input(
            BenchmarkId::new("commit_affected_state", size),
            &entries,
            |b, entries| {
                b.iter(|| black_box(apply_ledger_entries(black_box(entries), false)));
            },
        );
    }
    group.finish();
}

fn linear_cash_lookup(ledger: &Ledger, account_id: &str, currency: &str) -> i128 {
    ledger
        .entries()
        .iter()
        .filter(|entry| entry.account_id == account_id && entry.currency == currency)
        .map(|entry| entry.amount.raw())
        .sum()
}

fn composite_cash_lookup(
    cash: &BTreeMap<(String, String), i128>,
    account_id: &str,
    currency: &str,
) -> i128 {
    *cash
        .get(&(account_id.to_owned(), currency.to_owned()))
        .unwrap_or(&0)
}

fn bench_ledger_cash_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("ledger_cash_lookup");
    for size in SIZES {
        let mut ledger = Ledger::new();
        for index in 0..size {
            ledger
                .deposit(
                    &format!("criterion-{index:05}"),
                    "USD",
                    Money::from_raw(index as i128 + 1),
                    index as u64 + 1,
                )
                .expect("append deterministic account deposit");
        }
        let mut composite_cash = BTreeMap::new();
        for entry in ledger.entries() {
            let balance = composite_cash
                .entry((entry.account_id.clone(), entry.currency.clone()))
                .or_insert(0_i128);
            *balance += entry.amount.raw();
        }
        let account_id = format!("criterion-{:05}", size - 1);
        let indexed = ledger.cash_for(&account_id, "USD");
        let composite = composite_cash_lookup(&composite_cash, &account_id, "USD");
        let scanned = linear_cash_lookup(&ledger, &account_id, "USD");
        assert_eq!(indexed, composite, "new and prior cash indexes must match");
        assert_eq!(indexed, scanned, "indexed and scanned cash must match");
        group.bench_with_input(BenchmarkId::new("indexed", size), &ledger, |b, ledger| {
            b.iter(|| black_box(ledger.cash_for(black_box(&account_id), black_box("USD"))));
        });
        group.bench_with_input(
            BenchmarkId::new("composite_key_prior", size),
            &composite_cash,
            |b, cash| {
                b.iter(|| {
                    black_box(composite_cash_lookup(
                        black_box(cash),
                        black_box(&account_id),
                        black_box("USD"),
                    ))
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("entry_scan", size),
            &ledger,
            |b, ledger| {
                b.iter(|| {
                    black_box(linear_cash_lookup(
                        black_box(ledger),
                        black_box(&account_id),
                        black_box("USD"),
                    ))
                });
            },
        );
    }
    group.finish();
}

fn bench_ledger_cash_balances_snapshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("ledger_cash_balances_snapshot");
    for currency_count in [1_usize, 8, 64] {
        let mut ledger = Ledger::new();
        for index in 0..currency_count {
            ledger
                .deposit(
                    "criterion-cash-snapshot",
                    &format!("C{index:03}"),
                    Money::from_raw(index as i128 + 1),
                    index as u64 + 1,
                )
                .expect("append deterministic currency balance");
        }
        let owned = ledger.cash_balances_for("criterion-cash-snapshot");
        let borrowed = ledger.cash_balances_for_ref("criterion-cash-snapshot");
        assert_eq!(&owned, borrowed, "borrowed snapshot must match owned API");
        group.throughput(Throughput::Elements(currency_count as u64));
        group.bench_with_input(
            BenchmarkId::new("owned_allocating_snapshot", currency_count),
            &ledger,
            |b, ledger| {
                b.iter(|| {
                    black_box(ledger.cash_balances_for(black_box("criterion-cash-snapshot")))
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("borrowed_snapshot", currency_count),
            &ledger,
            |b, ledger| {
                b.iter(|| {
                    black_box(ledger.cash_balances_for_ref(black_box("criterion-cash-snapshot")))
                });
            },
        );
    }
    group.finish();
}

fn scheduler_day(mut offset: usize) -> String {
    let mut year = 2020_u32;
    let mut month = 1_u32;
    loop {
        let leap_year = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days_in_month = match month {
            2 if leap_year => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        if offset < days_in_month {
            return format!("{year:04}{month:02}{:02}", offset + 1);
        }
        offset -= days_in_month;
        month += 1;
        if month > 12 {
            month = 1;
            year += 1;
        }
    }
}

fn scheduler_history() -> Scheduler {
    let mut scheduler = Scheduler::default();
    scheduler
        .register(JobSpec {
            job_id: "criterion-history".into(),
            job_version: "v1".into(),
            owner: "criterion".into(),
            enabled: true,
            trigger: Trigger::Manual,
            window: JobWindow::Any,
            depends_on: Vec::new(),
            input_refs: vec!["fixture".into()],
            output_refs: vec!["fixture-output".into()],
            timeout_seconds: 60,
            retry_policy: RetryPolicy::default(),
            concurrency_key: "criterion-history".into(),
            idempotency_key: "criterion-history".into(),
            permission_scope: "research".into(),
            audit_reason: "scheduler active-run benchmark".into(),
            dry_run: true,
        })
        .expect("register benchmark job");
    for offset in 0..4_096 {
        let run = scheduler
            .start_run_at("criterion-history", &scheduler_day(offset), 1, 1)
            .expect("start historical run");
        scheduler
            .finish_run_with_code(run.run_id, true, None, 2)
            .expect("finish historical run");
    }
    scheduler
        .start_run_at("criterion-history", &scheduler_day(4_096), 1, 1)
        .expect("start active run");
    assert_eq!(scheduler.runs().len(), 4_097);
    assert_eq!(scheduler.active_run_ids().count(), 1);
    scheduler
}

fn bench_scheduler_active_run_selection(c: &mut Criterion) {
    let scheduler = scheduler_history();
    let expected = scheduler
        .runs()
        .into_iter()
        .filter(|run| run.status == JobStatus::Running)
        .map(|run| run.run_id)
        .collect::<Vec<_>>();
    let indexed = scheduler.active_run_ids().collect::<Vec<_>>();
    assert_eq!(indexed, expected, "active index matches scheduler history");

    let mut group = c.benchmark_group("scheduler_active_run_selection");
    group.bench_function("clone_and_scan_4097_runs", |b| {
        b.iter(|| {
            black_box(&scheduler)
                .runs()
                .into_iter()
                .filter(|run| run.status == JobStatus::Running)
                .map(|run| run.run_id)
                .collect::<Vec<_>>()
        });
    });
    group.bench_function("indexed_active_ids", |b| {
        b.iter(|| black_box(&scheduler).active_run_ids().collect::<Vec<_>>());
    });
    group.finish();
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
                        visited += active.into_iter().map(|side| black_box(side)).count();
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
                        visited += policies
                            .keys()
                            .copied()
                            .chain(fallback)
                            .map(|side| black_box(side))
                            .count();
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
            for own_market in 0..market_count {
                let expected_other = reservations
                    .iter()
                    .enumerate()
                    .filter(|(market, _)| *market != own_market)
                    .flat_map(|(_, orders)| orders.iter())
                    .fold(0_i128, |total, value| total.saturating_add(*value));
                let cached_other = cached_total
                    .saturating_sub(market_totals[own_market])
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
                        for own_market in 0..decisions {
                            let total = cached_total.min(i128::MAX as u128) as i128;
                            let other = cached_total
                                .saturating_sub(market_totals[own_market])
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
                    for target in 0..decisions {
                        observed = observed.saturating_add(total - snapshot[target]);
                    }
                    black_box(observed)
                });
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

fn configure_criterion() -> Criterion {
    Criterion::default()
        .sample_size(10)
        .warm_up_time(std::time::Duration::from_secs(1))
        .measurement_time(std::time::Duration::from_secs(2))
}

criterion_group! {
    name = benches;
    config = configure_criterion();
    targets = bench_append_batch, bench_digest, bench_prefix_digest, bench_event_log_validate,
        bench_event_log_manifest_digest,
        bench_ledger_entry_lookup, bench_ledger_cash_lookup,
        bench_ledger_entry_commit, bench_scheduler_active_run_selection, bench_full_replay,
        bench_ledger_replay, bench_l2_matching,
        bench_l2_queue_position_depth_lookup, bench_l2_queue_price_lookup_algorithm,
        bench_full_vs_incremental_replay, bench_ashare_previous_close,
        bench_backtest_risk_metrics, bench_benchmark_relative_columns,
        bench_strategy_decision_order_buffer,
    bench_strategy_close_scratch_reuse,
    bench_strategy_vwap_allocation,
        bench_ledger_cash_balances_snapshot,
        bench_portfolio_bar_timeline, bench_aligned_portfolio_bar_timeline,
        bench_protective_exit_position_side_iteration, bench_portfolio_visible_marks_reuse,
        bench_portfolio_funding_reservation_cache, bench_portfolio_margin_snapshot
}
criterion_main!(benches);
