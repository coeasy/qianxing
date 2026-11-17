use super::{admission, execution_updates};
use crate::{evaluate_strategy_contract, StrategyContractClients};
use qx_core::InstrumentId;
use qx_runtime::RuntimeConfig;
use qx_strategy::contract::StrategyCheckpointV1;

pub(super) struct StrategyEvaluation {
    pub(super) target_qty: i128,
    pub(super) contract_output: Option<qx_strategy::contract::StrategyContractOutput>,
    pub(super) strategy_checkpoint: Option<StrategyCheckpointV1>,
    pub(super) live_warmup_complete: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn evaluate_strategy_for_job(
    root: &std::path::Path,
    runtime_config: &RuntimeConfig,
    instrument: &InstrumentId,
    run_id: u64,
    now: u64,
    execution_updates: &execution_updates::ReplayedExecutionUpdates,
    input_strategy_checkpoint: Option<&StrategyCheckpointV1>,
    live_job_digest: Option<u64>,
    live_checkpoint_bootstrap: bool,
    checkpoint_input_supplied: bool,
    checkpoint_restore_required: bool,
    host_lifecycle: &mut qx_api::StrategyHostLifecycleEvidenceV2,
    clients: StrategyContractClients<'_>,
) -> Result<StrategyEvaluation, String> {
    let (target_qty, contract_output, strategy_checkpoint) = evaluate_strategy_contract(
        root,
        runtime_config,
        instrument,
        run_id,
        now,
        &execution_updates.order_updates,
        &execution_updates.fill_updates,
        &execution_updates.market_ticks,
        &execution_updates.market_order_books,
        &execution_updates.account_updates,
        input_strategy_checkpoint,
        clients,
    )?;
    if live_checkpoint_bootstrap && strategy_checkpoint.is_none() {
        return Err(
            "LIVE_READINESS_CHECKPOINT_BOOTSTRAP_MISSING: strategy returned no durable checkpoint"
                .into(),
        );
    }
    let warmup_instruments = contract_output
        .as_ref()
        .filter(|output| !output.intents.is_empty())
        .map(|output| {
            output
                .intents
                .iter()
                .map(|intent| intent.instrument.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    host_lifecycle.initialized = true;
    host_lifecycle.checkpoint_input_supplied = checkpoint_input_supplied;
    host_lifecycle.checkpoint_restore_required = runtime_config
        .strategy
        .external_executable
        .as_ref()
        .map(|_| checkpoint_restore_required);
    // Successful contract evaluation implies the digest-bound restore acknowledgement passed.
    host_lifecycle.checkpoint_restore_acknowledged = checkpoint_restore_required.then_some(true);
    if host_lifecycle.warmup_applicable {
        host_lifecycle.warmup_required_instruments = if !warmup_instruments.is_empty() {
            warmup_instruments.clone()
        } else {
            runtime_config
                .strategy
                .instrument
                .as_deref()
                .map(str::to_owned)
                .into_iter()
                .collect()
        };
        host_lifecycle.warmup_completed_instruments = admission::runtime_strategy_warmup_evidence(
            &runtime_config.strategy,
            strategy_checkpoint.as_ref(),
        )?
        .unwrap_or_default();
    }
    let live_warmup_complete = if live_job_digest.is_some() {
        admission::require_live_checkpoint_restore_ack(
            host_lifecycle.checkpoint_restore_required,
            host_lifecycle.checkpoint_restore_acknowledged,
        )?;
        admission::runtime_python_warmup_complete(
            &runtime_config.strategy,
            strategy_checkpoint.as_ref(),
            &warmup_instruments,
        )?
    } else {
        true
    };

    Ok(StrategyEvaluation {
        target_qty,
        contract_output,
        strategy_checkpoint,
        live_warmup_complete,
    })
}
