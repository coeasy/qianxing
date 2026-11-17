use super::execution_updates;
use qx_runtime::{RuntimeConfig, StrategyRuntimeConfig};
use qx_service::configured_backends::ConfiguredJobQueue;
use std::path::Path;

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_and_enqueue_live_input(
    strategy_runtime_config: &RuntimeConfig,
    root: &Path,
    worker_id: &str,
    now: u64,
    queue: &ConfiguredJobQueue,
    execution_cursor: &mut execution_updates::CursorState,
    execution_cursor_path: &Path,
    live_digest_path: &Path,
    last_live_digest: &mut Option<u64>,
    can_receive_execution_updates: bool,
    has_python_module: bool,
    has_native_checkpoint: bool,
    has_external_checkpoint: bool,
) -> Result<(), String> {
    let strategy_config: &StrategyRuntimeConfig = &strategy_runtime_config.strategy;
    let has_strategy_checkpoint =
        has_python_module || has_native_checkpoint || has_external_checkpoint;
    let needs_strategy_checkpoint_bootstrap = has_strategy_checkpoint
        && execution_updates::strategy_checkpoint(execution_cursor).is_none();
    if let Some(data_fingerprint) =
        live_strategy_input_digest(strategy_runtime_config, root, strategy_config, now)?
    {
        let batch = if can_receive_execution_updates {
            let strategy_id = strategy_config
                .id
                .clone()
                .unwrap_or_else(|| strategy_config.version.clone());
            execution_updates::collect_pending(
                strategy_runtime_config,
                root,
                &strategy_id,
                execution_cursor,
            )?
        } else {
            execution_updates::PendingExecutionBatch::default()
        };

        // Ranges with no strategy-facing events still need a durable acknowledgement.
        if batch.updates.is_empty()
            && batch.fill_updates.is_empty()
            && batch.market_ticks.is_empty()
            && batch.market_order_books.is_empty()
            && batch.account_updates.is_empty()
            && !batch.ranges.is_empty()
            && !needs_strategy_checkpoint_bootstrap
        {
            execution_updates::advance_cursor(execution_cursor, &batch.ranges)?;
            execution_updates::save_cursor(execution_cursor_path, execution_cursor)?;
        }

        if *last_live_digest != Some(data_fingerprint)
            || !batch.updates.is_empty()
            || !batch.fill_updates.is_empty()
            || !batch.market_ticks.is_empty()
            || !batch.market_order_books.is_empty()
            || !batch.account_updates.is_empty()
            || needs_strategy_checkpoint_bootstrap
        {
            let (job, run) = live_strategy_job(
                strategy_config,
                worker_id,
                data_fingerprint,
                now,
                &strategy_runtime_config.environment,
            );
            let (mut job, mut run) = (job, run);
            if !batch.ranges.is_empty()
                && (!batch.updates.is_empty()
                    || !batch.fill_updates.is_empty()
                    || !batch.market_ticks.is_empty()
                    || !batch.market_order_books.is_empty()
                    || !batch.account_updates.is_empty()
                    || has_strategy_checkpoint)
            {
                execution_updates::attach_ranges_to_job(&mut job, &mut run, &batch.ranges)?;
            }
            queue
                .enqueue(job, run, lease_clock(now))
                .map_err(|error| format!("写入实时 Strategy JobQueue 失败: {error:?}"))?;
            save_live_strategy_digest(live_digest_path, data_fingerprint)?;
            *last_live_digest = Some(data_fingerprint);
        }
    }
    Ok(())
}

use crate::{
    lease_clock, live_strategy_input_digest, live_strategy_job, save_live_strategy_digest,
};
