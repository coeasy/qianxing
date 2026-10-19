use super::*;

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

#[test]
fn active_run_index_tracks_lifecycle_and_rebuilds_after_restore() {
    let mut scheduler = Scheduler::default();
    let mut retryable = job("retryable-active");
    retryable.retry_policy.max_attempts = 2;
    scheduler.register(retryable).unwrap();
    let first = scheduler
        .start_run_at("retryable-active", "20260910", 1, 100)
        .unwrap();
    assert_eq!(
        scheduler.active_run_ids().collect::<Vec<_>>(),
        [first.run_id]
    );

    let stored = scheduler.to_json().unwrap();
    assert!(
        !stored.contains("active_run_ids"),
        "the derived index must stay out of the persisted schema"
    );
    let mut restored = Scheduler::from_json(&stored).unwrap();
    assert_eq!(
        restored.active_run_ids().collect::<Vec<_>>(),
        [first.run_id]
    );

    let failed = restored
        .finish_run_with_code(first.run_id, false, Some("WORKER_FAILED"), 110)
        .unwrap();
    assert_eq!(failed.status, JobStatus::Failed);
    assert!(restored.active_run_ids().next().is_none());

    let retried = restored.retry_run_at(first.run_id, u64::MAX).unwrap();
    assert_eq!(retried.attempt, 2);
    assert_eq!(
        restored.active_run_ids().collect::<Vec<_>>(),
        [first.run_id]
    );
    restored
        .finish_run_with_code(first.run_id, true, None, 120)
        .unwrap();
    assert!(restored.active_run_ids().next().is_none());

    let mut timed = job("timed-active");
    timed.timeout_seconds = 30;
    restored.register(timed).unwrap();
    let timed = restored
        .start_run_at("timed-active", "20260910", 2, 200)
        .unwrap();
    assert_eq!(
        restored.active_run_ids().collect::<Vec<_>>(),
        [timed.run_id]
    );
    restored.mark_timed_out(timed.run_id, 230).unwrap();
    assert!(restored.active_run_ids().next().is_none());
    restored.validate_state().unwrap();
}

#[test]
fn run_records_deadline_for_timeout_monitoring() {
    let mut scheduler = Scheduler::default();
    let mut timed = job("timed");
    timed.timeout_seconds = 30;
    scheduler.register(timed).unwrap();
    let run = scheduler.start_run_at("timed", "20260910", 1, 100).unwrap();
    assert_eq!(run.started_ts, 100);
    assert_eq!(run.deadline_ts, 130);
    assert!(!scheduler.is_timed_out(run.run_id, 129).unwrap());
    assert!(scheduler.is_timed_out(run.run_id, 130).unwrap());
    let timeout = scheduler.mark_timed_out(run.run_id, 130).unwrap();
    assert_eq!(timeout.status, JobStatus::NeedsIntervention);
    assert_eq!(timeout.error_code.as_deref(), Some("TIMEOUT"));
    assert!(!scheduler.is_timed_out(run.run_id, 999).unwrap());
}
