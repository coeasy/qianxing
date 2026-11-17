use super::execution_updates;
use crate::{lease_clock, runtime_timestamp_ms};
use qx_runtime::RuntimeConfig;
use qx_service::configured_backends::ConfiguredJobQueue;
use qx_storage::{JobLease, QueuedJob};
use sha2::{Digest, Sha256};
use std::path::Path;

#[allow(clippy::too_many_arguments)]
pub(super) fn load_fenced_prepared_evaluation(
    prepared_evaluation_path: &Path,
    strategy_runtime_config: &RuntimeConfig,
    root: &Path,
    worker_id: &str,
    queue: &ConfiguredJobQueue,
    queued: &QueuedJob,
    lease_owner: &str,
    lease_seconds: u64,
    lease: &mut JobLease,
) -> Result<Option<execution_updates::PreparedEvaluationV3>, String> {
    let Some(mut prepared) = execution_updates::load_prepared_evaluation(prepared_evaluation_path)?
    else {
        return Ok(None);
    };

    let job_bytes = serde_json::to_vec(&queued.job)
        .map_err(|error| format!("编码待恢复 Strategy job 失败: {error}"))?;
    let config_bytes = serde_json::to_vec(&strategy_runtime_config.strategy)
        .map_err(|error| format!("编码待恢复 Strategy 配置失败: {error}"))?;
    let recovery_ranges = execution_updates::ranges_from_job(&queued.job)?;
    let recovery_strategy_id = strategy_runtime_config
        .strategy
        .id
        .clone()
        .unwrap_or_else(|| strategy_runtime_config.strategy.version.clone());
    let recovery_facts = execution_updates::execution_updates_for_ranges(
        strategy_runtime_config,
        root,
        &recovery_strategy_id,
        &recovery_ranges,
    )?;
    let recovery_fact_bytes = serde_json::to_vec(&(&recovery_ranges, &recovery_facts))
        .map_err(|error| format!("编码恢复执行事实失败: {error}"))?;
    let job_digest = format!("{:x}", Sha256::digest(&job_bytes));
    let facts_digest = format!("{:x}", Sha256::digest(&recovery_fact_bytes));
    let config_digest = format!("{:x}", Sha256::digest(&config_bytes));
    let worker_matches = prepared.worker_id == worker_id;
    let job_matches = prepared.job_id == queued.job.job_id;
    let run_matches = prepared.run_id == queued.run.run_id;
    let input_matches = prepared.input_sha256 == job_digest;
    let facts_match = prepared.facts_sha256 == facts_digest;
    let config_matches = prepared.config_sha256 == config_digest;
    if !worker_matches
        || !job_matches
        || !run_matches
        || !input_matches
        || !facts_match
        || !config_matches
    {
        return Err(format!(
            "PreparedEvaluation 与当前 worker/job/config 不匹配: saved_worker={} saved_job={} saved_run={} match(worker/job/run/input/facts/config)={}/{}/{}/{}/{}/{}",
            prepared.worker_id,
            prepared.job_id,
            prepared.run_id,
            worker_matches,
            job_matches,
            run_matches,
            input_matches,
            facts_match,
            config_matches,
        ));
    }

    let recovery_now = lease_clock(runtime_timestamp_ms());
    *lease = queue
        .claim(queued.run.run_id, lease_owner, recovery_now, lease_seconds)
        .map_err(|error| format!("恢复 PreparedEvaluation 前重新领取租约失败: {error:?}"))?;
    prepared.lease_fencing_token = lease.fencing_token;
    // Upgrade legacy records and persist the renewed fence before replaying prepared effects.
    execution_updates::save_prepared_evaluation(prepared_evaluation_path, &prepared)?;
    Ok(Some(prepared))
}
