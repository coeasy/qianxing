//! Live intent admission and execution evidence checks for Strategy workers.

#![allow(clippy::items_after_test_module)] // Keep the admission fixture adjacent to command resolution.

use super::*;

pub(super) fn native_strategy_supports_execution_updates(
    supports_order_status: bool,
    supports_fill: bool,
    supports_account: bool,
) -> bool {
    supports_order_status || supports_fill || supports_account
}

pub(super) fn require_live_intent_reconciliation(
    strategy: &qx_runtime::StrategyRuntimeConfig,
    root: &Path,
    now: u64,
) -> Result<(), String> {
    if !strategy.live_enabled {
        return Err("LIVE_READINESS_STRATEGY_NOT_ENABLED: live job has live_enabled=false".into());
    }
    let account_id = strategy
        .account_id
        .as_deref()
        .ok_or_else(|| "LIVE_READINESS_ACCOUNT_MISSING".to_string())?;
    let venue_id = strategy
        .venue_id
        .as_deref()
        .ok_or_else(|| "LIVE_READINESS_VENUE_MISSING".to_string())?;
    let reports = qx_service::api_read_model::load_reconcile_reports(root)?;
    qx_service::reconcile_report::require_fresh_clean_reconciliation(
        &reports, account_id, venue_id, now,
    )
}

pub(super) fn require_live_strategy_commands_resolved(
    config: &qx_runtime::RuntimeConfig,
    root: &Path,
    control: &qx_control::ControlPlane,
    commands: &[qx_control::ControlCommand],
) -> Result<(), String> {
    let live_commands = commands
        .iter()
        .filter(|command| !command.dry_run)
        .collect::<Vec<_>>();
    if live_commands.is_empty() {
        return Ok(());
    }
    let account_id = config
        .strategy
        .account_id
        .as_deref()
        .ok_or_else(|| "LIVE_COMMAND_ACCOUNT_MISSING".to_string())?;
    let venue_id = config
        .strategy
        .venue_id
        .as_deref()
        .ok_or_else(|| "LIVE_COMMAND_VENUE_MISSING".to_string())?;
    let log_name = qx_runtime::account_event_log_name(account_id, venue_id)
        .ok_or_else(|| "LIVE_COMMAND_EVENTLOG_IDENTITY_INVALID".to_string())?;
    if !crate::event_log_exists(config, root, &log_name)? {
        return Err(format!("LIVE_COMMAND_EVENTLOG_MISSING:{log_name}"));
    }
    let pipeline = crate::open_account_pipeline(
        config,
        root,
        &log_name,
        qx_runtime::OutboxRecovery::ReadOnly,
    )?;
    for command in commands.iter().filter(|command| !command.dry_run) {
        let stored = control
            .command(command.command_id)
            .ok_or_else(|| format!("LIVE_COMMAND_OUTCOME_MISSING:{}", command.command_id))?;
        if stored.digest() != command.digest() {
            return Err(format!(
                "LIVE_COMMAND_OUTCOME_DIGEST_MISMATCH:{}",
                command.command_id
            ));
        }
        let audit_record = control
            .audit()
            .iter()
            .rev()
            .find(|record| record.command_id == command.command_id)
            .cloned()
            .ok_or_else(|| format!("LIVE_COMMAND_AUDIT_MISSING:{}", command.command_id))?;
        let failed_ts = match audit_record.status {
            qx_control::CommandStatus::Executed => None,
            qx_control::CommandStatus::Accepted => {
                require_live_strategy_command_status(
                    command.command_id,
                    Some(audit_record.status),
                )?;
                None
            }
            qx_control::CommandStatus::Failed => Some(audit_record.ts),
        };
        let order = qx_control::order_from_submit_command(command).map_err(|error| {
            format!("LIVE_COMMAND_ORDER_INVALID:{}:{error}", command.command_id)
        })?;
        if order.account_id != account_id {
            return Err(format!(
                "LIVE_COMMAND_ORDER_ACCOUNT_MISMATCH:{}",
                command.command_id
            ));
        }
        let observation = pipeline
            .log()
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::OrderSubmitted { order: fact } if fact.client_id == order.client_id => {
                    Some((VenueOrderObservation::Submitted, event.ts))
                }
                EventKind::Submit { client_order_id } if *client_order_id == order.client_id => {
                    Some((VenueOrderObservation::Submitted, event.ts))
                }
                EventKind::Accepted {
                    client_order_id, ..
                } if *client_order_id == order.client_id => {
                    Some((VenueOrderObservation::Accepted, event.ts))
                }
                EventKind::OrderStatusReconciled {
                    client_order_id,
                    status,
                    ..
                } if *client_order_id == order.client_id => Some((
                    match status {
                        qx_core::OrderStatus::Accepted
                        | qx_core::OrderStatus::Working
                        | qx_core::OrderStatus::PartiallyFilled => VenueOrderObservation::Accepted,
                        qx_core::OrderStatus::Filled => VenueOrderObservation::Filled,
                        qx_core::OrderStatus::Rejected => VenueOrderObservation::Rejected,
                        qx_core::OrderStatus::Cancelled | qx_core::OrderStatus::Expired => {
                            VenueOrderObservation::Cancelled
                        }
                        _ => VenueOrderObservation::Unresolved,
                    },
                    event.ts,
                )),
                EventKind::Rejected {
                    client_order_id, ..
                } if *client_order_id == order.client_id => {
                    Some((VenueOrderObservation::Rejected, event.ts))
                }
                EventKind::Cancelled { client_order_id } if *client_order_id == order.client_id => {
                    Some((VenueOrderObservation::Cancelled, event.ts))
                }
                EventKind::CancelPending { client_order_id }
                    if *client_order_id == order.client_id =>
                {
                    Some((VenueOrderObservation::Unresolved, event.ts))
                }
                EventKind::ReconcileRequired { client_order_id }
                    if *client_order_id == order.client_id =>
                {
                    Some((VenueOrderObservation::Unresolved, event.ts))
                }
                EventKind::Filled { fill } if fill.order_id == order.client_id => {
                    Some((VenueOrderObservation::Filled, event.ts))
                }
                _ => None,
            })
            .next_back();
        let reports = if failed_ts.is_some() {
            qx_service::api_read_model::load_reconcile_reports(root)?
        } else {
            Vec::new()
        };
        require_live_command_resolution(
            order.client_id,
            observation.map(|(state, _)| state),
            failed_ts,
            &reports,
            account_id,
            venue_id,
            crate::runtime_timestamp_ms(),
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VenueOrderObservation {
    Submitted,
    Accepted,
    Rejected,
    Cancelled,
    Filled,
    Unresolved,
}

fn require_live_command_resolution(
    command_id: u64,
    observation: Option<VenueOrderObservation>,
    failed_ts: Option<u64>,
    reports: &[qx_api::ReconcileReportSnapshot],
    account_id: &str,
    venue_id: &str,
    now: u64,
) -> Result<(), String> {
    require_live_venue_order_observation(command_id, observation)?;
    if let Some(failed_ts) = failed_ts {
        qx_service::reconcile_report::require_fresh_clean_reconciliation_after(
            reports, account_id, venue_id, now, failed_ts,
        )?;
    }
    Ok(())
}

pub(super) fn require_live_venue_order_observation(
    client_order_id: u64,
    observation: Option<VenueOrderObservation>,
) -> Result<(), String> {
    match observation {
        Some(
            VenueOrderObservation::Accepted
            | VenueOrderObservation::Rejected
            | VenueOrderObservation::Cancelled
            | VenueOrderObservation::Filled,
        ) => Ok(()),
        Some(VenueOrderObservation::Submitted | VenueOrderObservation::Unresolved) => Err(format!(
            "LIVE_VENUE_ORDER_OUTCOME_UNRESOLVED:{client_order_id}"
        )),
        None => Err(format!("LIVE_VENUE_ORDER_FACT_MISSING:{client_order_id}")),
    }
}

pub(super) fn live_command_outcome_is_retryable(error: &str) -> bool {
    [
        "LIVE_COMMAND_OUTCOME_PENDING:",
        "LIVE_COMMAND_OUTCOME_FAILED:",
        "LIVE_COMMAND_OUTCOME_UNKNOWN:",
        "LIVE_COMMAND_EVENTLOG_MISSING:",
        "LIVE_VENUE_ORDER_OUTCOME_UNRESOLVED:",
        "LIVE_VENUE_ORDER_FACT_MISSING:",
        "LIVE_READINESS_CHECKPOINT_RESTORE_ACK_",
        "LIVE_READINESS_RECONCILE_",
    ]
    .iter()
    .any(|prefix| error.starts_with(prefix))
}

pub(super) fn require_live_strategy_command_status(
    command_id: u64,
    status: Option<qx_control::CommandStatus>,
) -> Result<(), String> {
    match status {
        Some(qx_control::CommandStatus::Executed) => Ok(()),
        Some(qx_control::CommandStatus::Accepted) => {
            Err(format!("LIVE_COMMAND_OUTCOME_PENDING:{command_id}"))
        }
        Some(qx_control::CommandStatus::Failed) => Err(format!(
            "LIVE_COMMAND_OUTCOME_FAILED:{command_id}; venue reconciliation is required"
        )),
        None => Err(format!("LIVE_COMMAND_AUDIT_MISSING:{command_id}")),
    }
}

pub(super) fn runtime_strategy_warmup_evidence(
    strategy: &qx_runtime::StrategyRuntimeConfig,
    checkpoint: Option<&qx_strategy::contract::StrategyCheckpointV1>,
) -> Result<Option<Vec<String>>, String> {
    let evidence = if strategy.python_module.is_some() && strategy.python_strategy_spec.is_some() {
        checkpoint.and_then(|checkpoint| checkpoint.adapter_state.get("warmup_completed"))
    } else if strategy.external_executable.is_some() {
        checkpoint.and_then(|checkpoint| checkpoint.adapter_state.get("warmup_completed"))
    } else if strategy.c_abi_library.is_some() {
        checkpoint.and_then(|checkpoint| checkpoint.strategy_state.get("warmup_completed"))
    } else if strategy.python_module.is_some() {
        checkpoint.and_then(|checkpoint| checkpoint.adapter_state.get("warmup_completed"))
    } else {
        // The built-in Rust StrategyRuntime has no user-defined history warmup lifecycle.
        return Ok(None);
    };
    let Some(evidence) = evidence.and_then(serde_json::Value::as_array) else {
        return Ok(Some(Vec::new()));
    };
    if evidence.len() > 4096 {
        return Err(
            "LIVE_READINESS_WARMUP_EVIDENCE_INVALID: too many completed instruments".into(),
        );
    }
    let mut completed_instruments = std::collections::BTreeSet::new();
    for value in evidence {
        let instrument = value.as_str().filter(|value| !value.trim().is_empty()).ok_or_else(|| {
            "LIVE_READINESS_WARMUP_EVIDENCE_INVALID: warmup_completed must contain non-empty instrument strings"
                .to_string()
        })?;
        if !completed_instruments.insert(instrument.to_owned()) {
            return Err(format!(
                "LIVE_READINESS_WARMUP_EVIDENCE_INVALID: duplicate completed instrument {instrument}"
            ));
        }
    }
    Ok(Some(completed_instruments.into_iter().collect()))
}

pub(super) fn runtime_python_warmup_complete(
    strategy: &qx_runtime::StrategyRuntimeConfig,
    checkpoint: Option<&qx_strategy::contract::StrategyCheckpointV1>,
    required_instruments: &[String],
) -> Result<bool, String> {
    let Some(completed_instruments) = runtime_strategy_warmup_evidence(strategy, checkpoint)?
    else {
        return Ok(true);
    };
    let instruments = if required_instruments.is_empty() {
        strategy
            .instrument
            .as_deref()
            .map(str::to_owned)
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        required_instruments.to_vec()
    };
    if instruments.is_empty() {
        return Ok(false);
    }
    let completed_instruments = completed_instruments
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    Ok(instruments
        .iter()
        .all(|instrument| completed_instruments.contains(instrument)))
}

#[cfg(test)]
mod live_command_resolution_tests {
    use super::{require_live_command_resolution, VenueOrderObservation};
    use qx_api::ReconcileReportSnapshot;

    fn report(observed_ts: u64, order_issues: Vec<serde_json::Value>) -> ReconcileReportSnapshot {
        ReconcileReportSnapshot {
            schema_version: 1,
            worker_id: "reconciler-main".into(),
            account_id: "main".into(),
            venue_id: "binance".into(),
            observed_ts,
            order_issues,
            balances_count: 0,
            balance_discrepancies: Vec::new(),
            position_snapshots_count: None,
            funding_rate_snapshots_count: None,
            cashflow_count: None,
        }
    }

    #[test]
    fn failed_command_resolves_only_with_terminal_venue_fact_and_post_failure_clean_report() {
        let clean_after_failure = [report(1_101, Vec::new())];
        assert!(require_live_command_resolution(
            41,
            Some(VenueOrderObservation::Rejected),
            Some(1_100),
            &clean_after_failure,
            "main",
            "binance",
            1_200,
        )
        .is_ok());

        let unresolved = require_live_command_resolution(
            41,
            Some(VenueOrderObservation::Unresolved),
            Some(1_100),
            &clean_after_failure,
            "main",
            "binance",
            1_200,
        )
        .unwrap_err();
        assert!(unresolved.contains("LIVE_VENUE_ORDER_OUTCOME_UNRESOLVED"));

        let report_before_failure = [report(1_100, Vec::new())];
        let stale_boundary = require_live_command_resolution(
            41,
            Some(VenueOrderObservation::Rejected),
            Some(1_100),
            &report_before_failure,
            "main",
            "binance",
            1_200,
        )
        .unwrap_err();
        assert!(stale_boundary.contains("LIVE_READINESS_RECONCILE_NOT_AFTER_FAILURE"));

        let dirty_report = [report(
            1_101,
            vec![serde_json::json!({"kind": "status_mismatch"})],
        )];
        let dirty = require_live_command_resolution(
            41,
            Some(VenueOrderObservation::Rejected),
            Some(1_100),
            &dirty_report,
            "main",
            "binance",
            1_200,
        )
        .unwrap_err();
        assert!(dirty.contains("RECONCILE"));
    }
}

pub(super) fn require_live_checkpoint_restore_ack(
    restore_required: Option<bool>,
    restore_acknowledged: Option<bool>,
) -> Result<(), String> {
    if restore_required == Some(true) && restore_acknowledged != Some(true) {
        return Err("LIVE_READINESS_CHECKPOINT_RESTORE_ACK_MISSING".into());
    }
    Ok(())
}
