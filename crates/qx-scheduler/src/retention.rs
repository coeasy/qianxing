use super::{JobRun, JobStatus, Scheduler};
use qx_core::Fnv1a;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// 终态运行被驱逐后的紧凑血缘锚点。摘要按驱逐顺序递归计算；它不替代被归档明细，
/// 也不是密码学签名。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SchedulerRunRetentionAnchor {
    pub through_trading_day: String,
    pub evicted_count: u64,
    pub digest: u64,
    pub last_run_id: u64,
    pub has_success: bool,
}

/// 最多保留的终态 JobRun 数；仍在运行的记录不占用该额度。
pub const TERMINAL_RUN_RETENTION: usize = 4_096;

pub(super) fn is_canonical_trading_day(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 8 || !bytes.iter().all(u8::is_ascii_digit) {
        return false;
    }
    let Ok(year) = value[..4].parse::<u32>() else {
        return false;
    };
    let Ok(month) = value[4..6].parse::<u32>() else {
        return false;
    };
    let Ok(day) = value[6..8].parse::<u32>() else {
        return false;
    };
    if year == 0 || !(1..=12).contains(&month) {
        return false;
    }
    let leap_year = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        2 if leap_year => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=month_days).contains(&day)
}

impl Scheduler {
    pub(super) fn anchored_completed_jobs(&self) -> BTreeSet<String> {
        self.retention_anchors
            .iter()
            .filter(|(_, anchor)| anchor.has_success)
            .map(|(job_id, _)| job_id.clone())
            .collect()
    }

    pub(super) fn validate_retention_state(&self) -> Result<(), super::SchedulerError> {
        for (job_id, anchor) in &self.retention_anchors {
            if !self.jobs.contains_key(job_id)
                || !is_canonical_trading_day(&anchor.through_trading_day)
                || anchor.evicted_count == 0
            {
                return Err(super::SchedulerError::Invalid(format!(
                    "非法 Scheduler 运行保留锚点: {job_id}"
                )));
            }
        }
        if self.runs.values().any(|run| {
            self.retention_anchors
                .get(&run.job_id)
                .is_some_and(|anchor| run.trading_day <= anchor.through_trading_day)
        }) {
            return Err(super::SchedulerError::Invalid(
                "Scheduler 运行早于或等于其保留水位".into(),
            ));
        }
        let terminal_count = self
            .runs
            .values()
            .filter(|run| run.status != JobStatus::Running)
            .count();
        if terminal_count > TERMINAL_RUN_RETENTION {
            return Err(super::SchedulerError::Invalid(
                "终态运行数超过 Scheduler 保留上限".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn prune_terminal_runs(&mut self) {
        let terminal_count = self
            .runs
            .values()
            .filter(|run| run.status != JobStatus::Running)
            .count();
        let excess = terminal_count.saturating_sub(TERMINAL_RUN_RETENTION);
        for _ in 0..excess {
            let Some(run_id) = self
                .runs
                .values()
                .filter(|run| run.status != JobStatus::Running)
                .min_by(|left, right| {
                    (&left.trading_day, left.run_id).cmp(&(&right.trading_day, right.run_id))
                })
                .map(|run| run.run_id)
            else {
                break;
            };
            let Some(run) = self.runs.remove(&run_id) else {
                continue;
            };
            self.extend_anchor(run);
        }
    }

    fn extend_anchor(&mut self, run: JobRun) {
        let anchor = self
            .retention_anchors
            .entry(run.job_id.clone())
            .or_insert_with(|| SchedulerRunRetentionAnchor {
                through_trading_day: String::new(),
                evicted_count: 0,
                digest: Fnv1a::new().finish(),
                last_run_id: 0,
                has_success: false,
            });
        let mut digest = Fnv1a::new();
        digest.write_u64(anchor.digest);
        digest.write_u64(run.run_id);
        digest.write_text(&run.job_id);
        digest.write_text(&run.trading_day);
        digest.write_u64(run.attempt as u64);
        digest.write_text(match run.status {
            JobStatus::Running => "running",
            JobStatus::Succeeded => "succeeded",
            JobStatus::Failed => "failed",
            JobStatus::NeedsIntervention => "needs_intervention",
        });
        digest.write_u64(u64::from(run.manifest_digest.is_some()));
        digest.write_u64(run.manifest_digest.unwrap_or_default());
        digest.write_text(run.error_code.as_deref().unwrap_or_default());
        digest.write_u64(u64::from(run.next_retry_ts.is_some()));
        digest.write_u64(run.next_retry_ts.unwrap_or_default());
        digest.write_u64(run.started_ts);
        digest.write_u64(run.deadline_ts);
        anchor.digest = digest.finish();
        anchor.evicted_count = anchor.evicted_count.saturating_add(1);
        anchor.through_trading_day = anchor.through_trading_day.clone().max(run.trading_day);
        anchor.last_run_id = run.run_id;
        anchor.has_success |= run.status == JobStatus::Succeeded;
    }
}

#[cfg(test)]
mod tests;
