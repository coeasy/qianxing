use super::super::{
    JobRun, JobSpec, JobStatus, JobWindow, RetryPolicy, Scheduler, Trigger, TERMINAL_RUN_RETENTION,
};
use super::is_canonical_trading_day;
use qx_core::Fnv1a;

fn job(id: &str) -> JobSpec {
    JobSpec {
        job_id: id.into(),
        job_version: "v1".into(),
        owner: "research".into(),
        enabled: true,
        trigger: Trigger::Manual,
        window: JobWindow::Any,
        depends_on: Vec::new(),
        input_refs: vec!["bars".into()],
        output_refs: vec![format!("out-{id}")],
        timeout_seconds: 60,
        retry_policy: RetryPolicy::default(),
        concurrency_key: id.into(),
        idempotency_key: format!("{id}-daily"),
        permission_scope: "research".into(),
        audit_reason: format!("test {id}"),
        dry_run: true,
    }
}

fn test_trading_day(offset: usize) -> String {
    let days_per_year = 12 * 28;
    let year = 2020 + offset / days_per_year;
    let within_year = offset % days_per_year;
    format!(
        "{year:04}{:02}{:02}",
        within_year / 28 + 1,
        within_year % 28 + 1
    )
}

#[test]
fn terminal_runs_are_capped_and_evicted_history_keeps_a_watermark_anchor() {
    let mut scheduler = Scheduler::default();
    let history_job = job("history");
    let active_job = job("active");
    scheduler.register(history_job.clone()).unwrap();
    scheduler.register(active_job.clone()).unwrap();
    scheduler.completed.insert(history_job.job_id.clone());
    for offset in 0..=TERMINAL_RUN_RETENTION {
        let trading_day = test_trading_day(offset);
        let run_id = history_job.stable_key(&trading_day);
        scheduler.runs.insert(
            run_id,
            JobRun {
                run_id,
                job_id: history_job.job_id.clone(),
                trading_day,
                attempt: 1,
                status: JobStatus::Succeeded,
                manifest_digest: Some(offset as u64 + 1),
                error_code: None,
                next_retry_ts: None,
                started_ts: offset as u64,
                deadline_ts: offset as u64,
            },
        );
    }
    let active_day = test_trading_day(TERMINAL_RUN_RETENTION + 1);
    let active_id = active_job.stable_key(&active_day);
    scheduler
        .active_keys
        .insert(active_job.concurrency_key.clone());
    scheduler.runs.insert(
        active_id,
        JobRun {
            run_id: active_id,
            job_id: active_job.job_id,
            trading_day: active_day,
            attempt: 1,
            status: JobStatus::Running,
            manifest_digest: Some(9),
            error_code: None,
            next_retry_ts: None,
            started_ts: 1,
            deadline_ts: 61,
        },
    );

    let mut legacy_json: serde_json::Value =
        serde_json::from_str(&scheduler.to_json().unwrap()).unwrap();
    legacy_json
        .as_object_mut()
        .unwrap()
        .remove("retention_anchors");
    let migrated = Scheduler::from_json(&legacy_json.to_string()).unwrap();
    assert_eq!(migrated.runs.len(), TERMINAL_RUN_RETENTION + 1);
    assert_eq!(migrated.retention_anchors()["history"].evicted_count, 1);
    assert!(migrated.run(active_id).is_some());

    scheduler.prune_terminal_runs();
    assert_eq!(
        scheduler
            .runs
            .values()
            .filter(|run| run.status != JobStatus::Running)
            .count(),
        TERMINAL_RUN_RETENTION
    );
    assert!(scheduler.run(active_id).is_some());
    let anchor = &scheduler.retention_anchors["history"];
    assert_eq!(anchor.through_trading_day, test_trading_day(0));
    assert_eq!(anchor.evicted_count, 1);
    assert_ne!(anchor.digest, Fnv1a::new().finish());
    assert!(anchor.has_success);
    assert!(scheduler
        .start_run("history", &test_trading_day(0), 9)
        .is_err());

    let restored = Scheduler::from_json(&scheduler.to_json().unwrap()).unwrap();
    assert_eq!(restored.retention_anchors(), scheduler.retention_anchors());
    assert_eq!(restored.completed_jobs(), ["history".to_string()].into());
}

#[test]
fn trading_day_must_be_a_real_canonical_date_before_it_can_advance_the_watermark() {
    assert!(is_canonical_trading_day("20240229"));
    assert!(!is_canonical_trading_day("20230229"));
    assert!(!is_canonical_trading_day("2026-09-10"));
    assert!(!is_canonical_trading_day("20261301"));
}
