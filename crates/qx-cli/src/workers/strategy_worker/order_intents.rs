use super::super::{build_strategy_contract_intent_order, build_strategy_target_order};
use crate::{
    spread_group_id, strategy_current_qty_exact_for, strategy_current_qty_for,
    strategy_submit_command,
};
use qx_core::{InstrumentId, Order};
use qx_runtime::RuntimeConfig;
use qx_strategy::contract::StrategyContractOutput;
use qx_zhenlu::StrategyRuntime;
use std::path::Path;

pub(super) fn project_strategy_orders(
    root: &Path,
    runtime_config: &RuntimeConfig,
    runtime_config_path: &Path,
    worker_id: &str,
    run_id: u64,
    now: u64,
    readiness_checkpoint_only: bool,
    instrument: &InstrumentId,
    target_qty: i128,
    contract_output: Option<&StrategyContractOutput>,
) -> Result<Vec<Order>, String> {
    if readiness_checkpoint_only {
        return Ok(Vec::new());
    }
    if let Some(output) = contract_output {
        if !output.intents.is_empty() {
            return output
                .intents
                .iter()
                .map(|intent| {
                    build_strategy_contract_intent_order(
                        root,
                        runtime_config,
                        runtime_config_path,
                        worker_id,
                        output.signal_id,
                        &output.request_id,
                        intent,
                        now,
                    )
                })
                .collect();
        }
        let current_qty = strategy_current_qty_for(root, runtime_config, instrument)?;
        let current_qty_exact = output
            .target_qty_exact
            .as_ref()
            .map(|_| strategy_current_qty_exact_for(root, runtime_config, instrument))
            .transpose()?;
        return Ok(build_strategy_target_order(
            runtime_config,
            runtime_config_path,
            worker_id,
            run_id,
            now,
            current_qty,
            target_qty,
            current_qty_exact.as_deref(),
            Some(output),
        )?
        .into_iter()
        .collect());
    }

    let current_qty = strategy_current_qty_for(root, runtime_config, instrument)?;
    Ok(build_strategy_target_order(
        runtime_config,
        runtime_config_path,
        worker_id,
        run_id,
        now,
        current_qty,
        target_qty,
        None,
        None,
    )?
    .into_iter()
    .collect())
}

pub(super) fn reserve_strategy_commands(
    orders: &[Order],
    strategy: &mut StrategyRuntime,
    worker_id: &str,
    run_id: u64,
    dry_run: bool,
    contract_output: Option<&StrategyContractOutput>,
) -> Result<(Option<String>, Vec<qx_control::ControlCommand>), String> {
    let spread_group_id = if orders.len() >= 2 {
        Some(spread_group_id(
            worker_id,
            run_id,
            contract_output
                .map(|output| output.signal_id)
                .unwrap_or(run_id),
        ))
    } else {
        None
    };
    let commands = orders
        .iter()
        .map(|order| {
            strategy
                .reserve_order()
                .map_err(|error| format!("Strategy 订单配额拒绝: {error:?}"))?;
            strategy_submit_command(worker_id, order, dry_run, spread_group_id.as_deref())
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok((spread_group_id, commands))
}
