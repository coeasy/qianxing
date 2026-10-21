//! Process-specific API policies supplied to the qx-service assembly.

use crate::{
    collect_doctor_report_for_config, configured_api_readiness, order_from_submit_command,
    runtime_timestamp_ms,
};
use qx_api::{ApiService, ControlSubmitError};
use qx_control::ControlError;
use qx_runtime::{RuntimeConfig, RuntimeSupervisor};
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
        |command| {
            order_from_submit_command(command)
                .map(|_| ())
                .map_err(|error| {
                    ControlSubmitError::Rejected(ControlError::Invalid(format!(
                        "SubmitOrder 载荷非法: {error:?}"
                    )))
                })
        },
    )
}
