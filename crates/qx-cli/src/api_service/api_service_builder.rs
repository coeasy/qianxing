//! Process-specific API policies supplied to the qx-service assembly.

use crate::{
    collect_doctor_report_for_config, configured_api_readiness, order_from_submit_command,
    runtime_timestamp_ms,
};
use qx_api::{ApiService, ControlSubmitError};
use qx_control::{cancel_order_target_from_command, CommandKind, ControlError};
use qx_core::VenueFamily;
use qx_runtime::{RuntimeConfig, RuntimeSupervisor, WorkerRole};
use std::path::Path;

pub(super) fn build_configured_api_service_inner(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
) -> Result<ApiService, String> {
    // API 会受理 Trading 权限的 SubmitOrder；A 股规则在本入口没有订单形状转换落点，
    // 因此必须在 qx-service 创建目录、打开存储或启动 Provider 之前拒绝。
    crate::reject_ashare_rules_on_submit_path(runtime_config_path, None, "serve")?;

    let readiness_config = config.clone();
    let readiness_runtime_config_path = runtime_config_path.to_path_buf();
    let readiness_static_report = collect_doctor_report_for_config(
        runtime_config_path,
        config,
        RuntimeSupervisor::new(config.clone()).map(|_| ()),
    )?;
    let readiness_metrics_dir = qx_service::worker_metrics::worker_metrics_dir(config);
    let worker_metrics_stale_after_ms = config.messaging.worker_stale_after_ms;
    let algo_execution_config = config.clone();
    let cancel_routes = config
        .workers
        .iter()
        .filter(|worker| worker.enabled && worker.role == WorkerRole::Execution)
        .filter_map(|worker| {
            let venue_id = worker.venue_id.as_deref()?;
            // Control-plane CancelOrder is currently dispatched by live Binance/CCXT
            // execution workers. Paper execution is a separate simulated command path.
            if !matches!(
                VenueFamily::parse_option(Some(venue_id)),
                Some(VenueFamily::Binance | VenueFamily::Other)
            ) {
                return None;
            }
            Some((
                worker.account_id.as_deref()?.to_owned(),
                venue_id.to_owned(),
            ))
        })
        .collect::<Vec<_>>();

    qx_service::api_host::build_configured_api_service(
        config,
        runtime_config_path,
        &crate::python_interpreter(),
        runtime_timestamp_ms,
        move |readiness_store| {
            configured_api_readiness(
                &readiness_config,
                &readiness_runtime_config_path,
                &readiness_static_report,
                &readiness_store,
                &readiness_metrics_dir,
                runtime_timestamp_ms(),
                worker_metrics_stale_after_ms,
            )
        },
        move |operation, body| {
            crate::api_algo_execution::handle_algo_execution_request(
                &algo_execution_config,
                operation,
                body,
            )
        },
        move |command| match &command.kind {
            CommandKind::SubmitOrder => {
                order_from_submit_command(command)
                    .map(|_| ())
                    .map_err(|error| {
                        ControlSubmitError::Rejected(ControlError::Invalid(format!(
                            "SubmitOrder 载荷非法: {error:?}"
                        )))
                    })
            }
            CommandKind::CancelOrder => {
                let target = cancel_order_target_from_command(command).map_err(|error| {
                    ControlSubmitError::Rejected(ControlError::Invalid(format!(
                        "CancelOrder 载荷非法: {error:?}"
                    )))
                })?;
                if !cancel_routes.iter().any(|(account_id, venue_id)| {
                    account_id == &target.account_id
                        && venue_id.eq_ignore_ascii_case(&target.venue_id)
                }) {
                    return Err(ControlSubmitError::Rejected(ControlError::Invalid(
                        "CancelOrder 未匹配到启用的账户/交易所 Execution worker".into(),
                    )));
                }
                Ok(())
            }
            _ => Ok(()),
        },
    )
}
