//! Durable, account-scoped EventLog cursor and typed execution facts for live strategy jobs.

mod checkpoint;
mod cursor_state;
mod execution_facts;
mod prepared_evaluation;
mod replay;

#[cfg(feature = "bench-prepared-evaluation")]
pub(super) use prepared_evaluation::run_prepared_evaluation_bench;

use qx_control::ControlCommand;
use qx_core::Order;
use qx_runtime::StrategyRuntimeConfig;
use qx_scheduler::JobSpec;
use qx_strategy::contract::{
    StrategyAccountUpdateV1, StrategyCheckpointV1, StrategyContractMarketOrderBookV1,
    StrategyContractMarketTickV1, StrategyContractOrderBookLevelV1,
};
use qx_strategy::{StrategyFillUpdateV2, StrategyOrderStatusUpdate};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const CURSOR_SCHEMA_VERSION: u32 = 3;
const LEGACY_CURSOR_SCHEMA_VERSION: u32 = 1;
const PREVIOUS_CURSOR_SCHEMA_VERSION: u32 = 2;
const CURSOR_V1_TO_V3_MIGRATION_RESULT: &str = "cursor_v1_to_v3_commit_generation";
const CURSOR_V2_TO_V3_MIGRATION_RESULT: &str = "cursor_v2_to_v3_commit_generation";
const CURSOR_DEPLOYMENT_INITIAL_BINDING: &str = "cursor_state_preserved";
const CURSOR_DEPLOYMENT_CHECKPOINT_BINDING: &str = "checkpoint_identity_revalidated";
const CURSOR_DEPLOYMENT_STATE_CONVERTED: &str = "authorized_checkpoint_state_migration";
const CURSOR_REFERENCE_SCHEMA_VERSION: u32 = 1;
const MAX_CURSOR_STATE_BYTES: usize = 2 * 1024 * 1024;
const MAX_STRATEGY_CHECKPOINT_BYTES: usize = 64 * 1024;
const JOB_INPUT_REF_PREFIX: &str = "strategy-execution-cursors-v1:";
const MAX_RUNTIME_TICK_BATCH: usize = 10_000;
const MAX_RUNTIME_ORDER_BOOK_BATCH: usize = 10_000;
const MAX_RUNTIME_ACCOUNT_UPDATE_BATCH: usize = 10_000;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CursorRange {
    pub(super) log_name: String,
    pub(super) from_seq: u64,
    pub(super) through_seq: u64,
}

#[cfg(test)]
use cursor_state::backup_path;
#[cfg(test)]
pub(super) use cursor_state::cursor_path;
pub(super) use cursor_state::{
    checkpoint_sha256, configured_cursor_path, load_cursor, save_cursor,
    CursorDeploymentMigrationV1, CursorState,
};
pub(in crate::workers::strategy_worker) use prepared_evaluation::benchmark_crash_after;
#[cfg(test)]
pub(super) use prepared_evaluation::prepared_evaluation_payload_sha256;
pub(super) use prepared_evaluation::{
    clear_committed_prepared_evaluation, clear_prepared_evaluation,
    clear_prepared_evaluation_for_run, load_prepared_evaluation, prepared_evaluation_path,
    save_prepared_evaluation, unknown_command_result, PreparedEvaluationV3,
    PREPARED_EVALUATION_SCHEMA_VERSION,
};
#[cfg(test)]
use prepared_evaluation::{
    prepared_evaluation_backup_path, LEGACY_PREPARED_EVALUATION_SCHEMA_V2,
    MAX_PREPARED_COMMAND_RESULT_BYTES,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PendingExecutionBatch {
    pub(super) ranges: Vec<CursorRange>,
    pub(super) updates: Vec<StrategyOrderStatusUpdate>,
    pub(super) fill_updates: Vec<StrategyFillUpdateV2>,
    pub(super) market_ticks: Vec<StrategyContractMarketTickV1>,
    pub(super) market_order_books: Vec<StrategyContractMarketOrderBookV1>,
    pub(super) account_updates: Vec<StrategyAccountUpdateV1>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct ReplayedExecutionUpdates {
    pub(super) order_updates: Vec<StrategyOrderStatusUpdate>,
    pub(super) fill_updates: Vec<StrategyFillUpdateV2>,
    pub(super) market_ticks: Vec<StrategyContractMarketTickV1>,
    pub(super) market_order_books: Vec<StrategyContractMarketOrderBookV1>,
    pub(super) account_updates: Vec<StrategyAccountUpdateV1>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct CursorReference {
    schema_version: u32,
    ranges: Vec<CursorRange>,
}

#[cfg(test)]
use execution_facts::replay_account_updates;
/// Read every configured execution account/venue log and build a deterministic snapshot.
/// Cursor ranges include unrelated events too, so a successful strategy job can advance past
/// them without repeatedly scanning the same log tail.
pub(super) use execution_facts::{collect_pending, execution_updates_for_ranges};

pub(super) use checkpoint::{
    advance_cursor, bind_cursor_to_deployment_revision, commit_strategy_checkpoint,
    is_committed_run, mark_run_committed, strategy_checkpoint,
    validate_checkpoint_binding_for_mode,
};
#[cfg(test)]
use checkpoint::{strategy_binding_sha256, validate_checkpoint_binding, validate_run_fence};

pub(super) fn attach_ranges_to_job(
    job: &mut JobSpec,
    run: &mut qx_scheduler::JobRun,
    ranges: &[CursorRange],
) -> Result<(), String> {
    if ranges.is_empty() {
        return Ok(());
    }
    let reference = CursorReference {
        schema_version: CURSOR_REFERENCE_SCHEMA_VERSION,
        ranges: ranges.to_vec(),
    };
    let payload = serde_json::to_string(&reference)
        .map_err(|error| format!("编码 Strategy EventLog 游标范围失败: {error}"))?;
    let digest = qx_strategy::sha256_hex(payload.as_bytes());
    job.input_refs
        .push(format!("{JOB_INPUT_REF_PREFIX}{payload}"));
    job.idempotency_key = format!("{}:execution:{digest}", job.idempotency_key);
    run.run_id = job.stable_key(&run.trading_day);
    Ok(())
}

pub(super) fn ranges_from_job(job: &JobSpec) -> Result<Vec<CursorRange>, String> {
    let refs = job
        .input_refs
        .iter()
        .filter_map(|reference| reference.strip_prefix(JOB_INPUT_REF_PREFIX))
        .collect::<Vec<_>>();
    if refs.is_empty() {
        return Ok(Vec::new());
    }
    if refs.len() != 1 {
        return Err("Strategy Job 含多个 execution cursor 引用".into());
    }
    let reference: CursorReference = serde_json::from_str(refs[0])
        .map_err(|error| format!("Strategy Job execution cursor 引用无效: {error}"))?;
    if reference.schema_version != CURSOR_REFERENCE_SCHEMA_VERSION || reference.ranges.is_empty() {
        return Err("Strategy Job execution cursor 版本或范围非法".into());
    }
    Ok(reference.ranges)
}
