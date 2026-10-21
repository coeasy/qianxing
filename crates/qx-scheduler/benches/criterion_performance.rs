//! Criterion benchmarks for scheduler run-state lookup.

use criterion::{criterion_group, criterion_main, Criterion};
use qx_scheduler::{JobSpec, JobStatus, JobWindow, RetryPolicy, Scheduler, Trigger};
use std::hint::black_box;

fn scheduler_day(mut offset: usize) -> String {
    let mut year = 2020_u32;
    let mut month = 1_u32;
    loop {
        let leap_year =
            year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
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

fn configure_criterion() -> Criterion {
    Criterion::default()
        .sample_size(10)
        .warm_up_time(std::time::Duration::from_secs(1))
        .measurement_time(std::time::Duration::from_secs(2))
}

criterion_group! {
    name = benches;
    config = configure_criterion();
    targets = bench_scheduler_active_run_selection
}
criterion_main!(benches);
