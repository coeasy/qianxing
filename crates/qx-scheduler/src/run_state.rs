use super::{JobRun, JobSpec, JobStatus, Scheduler, SchedulerRunRetentionAnchor};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
struct PersistedScheduler {
    jobs: BTreeMap<String, JobSpec>,
    runs: BTreeMap<u64, JobRun>,
    #[serde(default)]
    retention_anchors: BTreeMap<String, SchedulerRunRetentionAnchor>,
    active_keys: BTreeSet<String>,
    completed: BTreeSet<String>,
}

impl<'de> Deserialize<'de> for Scheduler {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let persisted = PersistedScheduler::deserialize(deserializer)?;
        let active_run_ids = persisted
            .runs
            .iter()
            .filter_map(|(run_id, run)| (run.status == JobStatus::Running).then_some(*run_id))
            .collect();
        Ok(Self {
            jobs: persisted.jobs,
            runs: persisted.runs,
            retention_anchors: persisted.retention_anchors,
            active_keys: persisted.active_keys,
            active_run_ids,
            completed: persisted.completed,
        })
    }
}

impl Scheduler {
    /// Return all running records and at most `TERMINAL_RUN_RETENTION` terminal records.
    ///
    /// Callers receive cloned snapshots; the Scheduler remains the sole owner of run state.
    pub fn runs(&self) -> Vec<JobRun> {
        self.runs.values().cloned().collect()
    }

    /// Return running job IDs without cloning or scanning retained terminal history.
    pub fn active_run_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.active_run_ids.iter().copied()
    }
}
