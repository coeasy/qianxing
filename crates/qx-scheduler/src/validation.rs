use super::retention::is_canonical_trading_day;
use super::{Scheduler, SchedulerError};
use std::collections::BTreeSet;

impl Scheduler {
    pub(super) fn validate_state(&self) -> Result<(), SchedulerError> {
        let mut expected_active = BTreeSet::new();
        let mut expected_active_run_ids = BTreeSet::new();
        let mut expected_completed = self.anchored_completed_jobs();
        for (run_id, run) in &self.runs {
            let job = self
                .jobs
                .get(&run.job_id)
                .ok_or_else(|| SchedulerError::MissingDependency(run.job_id.clone()))?;
            if *run_id != run.run_id
                || run.run_id != job.stable_key(&run.trading_day)
                || !is_canonical_trading_day(&run.trading_day)
                || run.attempt == 0
                || run.started_ts > run.deadline_ts
            {
                return Err(SchedulerError::Invalid(format!(
                    "run {} 身份、尝试次数或截止时间非法",
                    run.run_id
                )));
            }
            match run.status {
                super::JobStatus::Running => {
                    if !expected_active.insert(job.concurrency_key.clone()) {
                        return Err(SchedulerError::Invalid("运行中的并发键重复".into()));
                    }
                    expected_active_run_ids.insert(*run_id);
                }
                super::JobStatus::Succeeded => {
                    expected_completed.insert(run.job_id.clone());
                }
                super::JobStatus::Failed | super::JobStatus::NeedsIntervention => {}
            }
            if run.status != super::JobStatus::Failed && run.next_retry_ts.is_some() {
                return Err(SchedulerError::Invalid(
                    "非 Failed 运行不能带重试时间".into(),
                ));
            }
        }
        if self.active_keys != expected_active
            || self.active_run_ids != expected_active_run_ids
            || self.completed != expected_completed
        {
            return Err(SchedulerError::Invalid(
                "active_keys/active_run_ids/completed 与 runs 不一致".into(),
            ));
        }
        self.validate_retention_state()
    }

    pub(super) fn validate_dependencies(&self) -> Result<(), SchedulerError> {
        for job in self.jobs.values() {
            for dependency in &job.depends_on {
                if !self.jobs.contains_key(dependency) {
                    return Err(SchedulerError::MissingDependency(dependency.clone()));
                }
            }
        }
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for id in self.jobs.keys() {
            self.visit(id, &mut visiting, &mut visited)?;
        }
        Ok(())
    }

    fn visit(
        &self,
        id: &str,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> Result<(), SchedulerError> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id.to_string()) {
            return Err(SchedulerError::Cycle(id.into()));
        }
        let job = self
            .jobs
            .get(id)
            .ok_or_else(|| SchedulerError::MissingDependency(id.to_string()))?;
        for dependency in &job.depends_on {
            self.visit(dependency, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id.into());
        Ok(())
    }
}
