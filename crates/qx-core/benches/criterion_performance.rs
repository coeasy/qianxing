//! Criterion measurements owned by qx-core: EventLog hashing/validation and Ledger access/commit.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use qx_core::{
    Event, EventKind, EventLog, Fnv1a, Ledger, LedgerEntry, LedgerEntryKind, Money, Priority,
    Quantity,
};
use std::collections::{BTreeMap, BTreeSet};
use std::hint::black_box;

const SIZES: [usize; 3] = [1_000, 8_000, 64_000];

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

fn bench_append_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("qx_core_append_batch");
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
    let mut group = c.benchmark_group("qx_core_event_log_digest");
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
    let mut group = c.benchmark_group("qx_core_event_log_manifest_digest");
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
    let mut group = c.benchmark_group("qx_core_event_log_prefix_digest");
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
    let mut group = c.benchmark_group("qx_core_event_log_validate");
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
    let mut group = c.benchmark_group("qx_core_ledger_entry_lookup");
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

            leverage: None,
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
    let mut group = c.benchmark_group("qx_core_ledger_entry_commit");
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
    let mut group = c.benchmark_group("qx_core_ledger_cash_lookup");
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
    let mut group = c.benchmark_group("qx_core_ledger_cash_balances_snapshot");
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
        bench_event_log_manifest_digest, bench_ledger_entry_lookup, bench_ledger_cash_lookup,
        bench_ledger_entry_commit, bench_ledger_cash_balances_snapshot
}
criterion_main!(benches);
