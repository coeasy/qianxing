//! Constrained materialization of immutable Strategy worker runtime revisions.

use crate::{RuntimeConfig, WorkerRole};
use qx_spec::StrategyDeploymentRevisionV1;

/// Parse a frozen RuntimeConfig payload and prove it changes only one Strategy worker.
pub fn deployment_runtime_config(
    base: &RuntimeConfig,
    revision: &StrategyDeploymentRevisionV1,
) -> Result<RuntimeConfig, String> {
    revision.validate()?;
    let payload = revision
        .runtime_config_json
        .as_deref()
        .ok_or_else(|| "new deployment revision requires runtime_config_json".to_string())?;
    let candidate = RuntimeConfig::from_json(payload)
        .map_err(|error| format!("invalid deployment runtime config: {error}"))?;
    if base.config_fingerprint.is_some() && candidate.config_fingerprint.is_none() {
        return Err("deployment payload cannot remove an existing config fingerprint".into());
    }
    let base_worker_index = base
        .workers
        .iter()
        .position(|worker| worker.id == revision.worker_id)
        .ok_or_else(|| format!("strategy worker {} is not configured", revision.worker_id))?;
    let candidate_worker_index = candidate
        .workers
        .iter()
        .position(|worker| worker.id == revision.worker_id)
        .ok_or_else(|| format!("deployment payload omits worker {}", revision.worker_id))?;
    let base_worker = &base.workers[base_worker_index];
    let candidate_worker = &candidate.workers[candidate_worker_index];
    if base_worker.role != WorkerRole::Strategy || candidate_worker.role != WorkerRole::Strategy {
        return Err(format!(
            "worker {} is not a Strategy worker",
            revision.worker_id
        ));
    }
    if candidate_worker.enabled != revision.desired_running {
        return Err(
            "revision desired_running does not match the payload worker enabled state".into(),
        );
    }
    if base_worker.account_id != candidate_worker.account_id
        || base_worker.venue_id != candidate_worker.venue_id
    {
        return Err("deployment revisions cannot change worker account or venue identity".into());
    }
    if base.workers.len() != candidate.workers.len() {
        return Err("deployment payload cannot add or remove runtime workers".into());
    }

    let mut expected = base.clone();
    expected.workers[base_worker_index] = candidate_worker.clone();
    if base.strategies.is_empty() {
        let strategy_worker_count = base
            .workers
            .iter()
            .filter(|worker| worker.role == WorkerRole::Strategy)
            .count();
        if strategy_worker_count != 1 || !candidate.strategies.is_empty() {
            return Err(
                "legacy single-strategy revision cannot target a multi-strategy runtime".into(),
            );
        }
        if base.strategy.account_id != candidate.strategy.account_id
            || base.strategy.venue_id != candidate.strategy.venue_id
        {
            return Err(
                "deployment revisions cannot change strategy account or venue identity".into(),
            );
        }
        expected.strategy = candidate.strategy.clone();
    } else {
        if base.strategies.len() != candidate.strategies.len() {
            return Err("deployment payload cannot add or remove strategy instances".into());
        }
        let base_strategy_index = base
            .strategies
            .iter()
            .position(|strategy| strategy.id.as_deref() == Some(revision.worker_id.as_str()))
            .ok_or_else(|| format!("runtime has no strategy for worker {}", revision.worker_id))?;
        let candidate_strategy_index = candidate
            .strategies
            .iter()
            .position(|strategy| strategy.id.as_deref() == Some(revision.worker_id.as_str()))
            .ok_or_else(|| {
                format!(
                    "deployment payload has no strategy for worker {}",
                    revision.worker_id
                )
            })?;
        let base_strategy = &base.strategies[base_strategy_index];
        let candidate_strategy = &candidate.strategies[candidate_strategy_index];
        if base_strategy.account_id != candidate_strategy.account_id
            || base_strategy.venue_id != candidate_strategy.venue_id
        {
            return Err(
                "deployment revisions cannot change strategy account or venue identity".into(),
            );
        }
        expected.strategies[base_strategy_index] = candidate_strategy.clone();
    }
    for (index, worker) in candidate.workers.iter().enumerate() {
        if index != candidate_worker_index && expected.workers[index] != *worker {
            return Err(format!(
                "deployment payload changes unrelated worker {}",
                worker.id
            ));
        }
    }
    expected.config_fingerprint = candidate.config_fingerprint.clone();
    if expected != candidate {
        return Err(
            "deployment payload changes runtime settings outside its target strategy worker".into(),
        );
    }
    Ok(candidate)
}
