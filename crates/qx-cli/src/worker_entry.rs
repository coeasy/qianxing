//! worker 角色分派表与进程监督器。
//!
//! `ccxt-worker` 与 `binance-worker` 两条入口在此共用同一张 `WorkerRole` 分派表，
//! 新增角色只需要在本处登记一次，避免出现两套角色语义。

use std::path::{Path, PathBuf};

use qx_adapter::{CcxtProcessClient, CcxtProcessVenue, CcxtRpc};
use qx_orchestrator::SuperviseFailure;
#[cfg(test)]
use qx_runtime::RuntimeConfig;
use qx_runtime::WorkerConfig;

pub(crate) use qx_service::worker_config::{
    configured_reconcile_worker_id, recovery_worker_venue_id, venue_worker, VenueEntry,
};

use crate::worker_runtime::{run_binance_worker_inner, run_ccxt_worker_inner};
use crate::{
    account_worker_settlement_currency, binance_event_log_name, catch_panic,
    ccxt_error_is_optional_derivatives_capability, load_binance_worker_auth, new_binance_venue,
    pending_spread_recovery_groups_for_account, python_interpreter, read_runtime_config,
    recover_spread_groups_for_venue, recovery_order_validator, required_account_event_log,
    runtime_timestamp_ms, spread_recovery_poll_delay, validate_configured_market_specs,
    PipelineMetricsReporter, PipelineStorage, SpreadRecoveryContext,
};

/// `reconcile` 入口：点名时与 `binance-worker` 走完全相同的四段校验，省略时按上表解析。
pub(crate) fn run_binance_reconcile_once(
    path: &Path,
    worker_id: Option<&str>,
) -> Result<(), String> {
    let worker_id = match worker_id {
        Some(worker_id) => worker_id.to_string(),
        None => configured_reconcile_worker_id(&read_runtime_config(path)?, &VenueEntry::BINANCE)?,
    };
    run_binance_worker(path, &worker_id, true)
}

pub(crate) fn run_binance_spread_recovery_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    let venue_id = recovery_worker_venue_id(&worker)?;
    let runtime_config = read_runtime_config(&runtime_config_path)?;
    let settlement_currency = account_worker_settlement_currency(&runtime_config, &worker)?;
    let log_name = binance_event_log_name(&worker)?;
    let mut metrics = PipelineMetricsReporter::new(&runtime_config, context.id(), &log_name);
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!("binance spread recovery scanning venue={venue_id}"),
        Some(runtime_timestamp_ms()),
    )?;
    metrics.publish_if_due(true, runtime_timestamp_ms());
    let mut recovery_stalls = 0_u32;
    qx_service::worker_host::run_polling_worker(&context, once, || {
        let now = runtime_timestamp_ms();
        let pending_before = pending_spread_recovery_groups_for_account(
            &pipeline_storage.root,
            &venue_id,
            worker.account_id.as_deref(),
        )?;
        if pending_before > 0 {
            let mut pipeline = pipeline_storage
                .open(log_name.clone(), settlement_currency.clone())
                .map_err(|error| format!("打开 Binance 多腿恢复 EventLog 失败: {error}"))?;
            let active_orders = pipeline.orders();
            qx_service::execution_capability_gate::require_active_orders_execution_capability(
                active_orders.iter(),
                &worker,
                &runtime_config.environment,
                &runtime_config_path,
                qx_service::execution_capability_gate::ExecutionConnectorProfile::BinanceSpot,
                true,
                &mut pipeline,
                now,
            )?;
            let auth = load_binance_worker_auth(&worker)?;
            let mut venue = new_binance_venue(&worker, auth)?;
            venue
                .restore_orders(pipeline.orders())
                .map_err(|error| format!("恢复 Binance 多腿订单状态失败: {error:?}"))?;
            let mut source_seq = pipeline
                .log()
                .events()
                .last()
                .map(|event| event.source_seq)
                .unwrap_or(0);
            let validator =
                recovery_order_validator(&worker, &pipeline, Some(&runtime_config_path));
            let (_, diagnostics) = recover_spread_groups_for_venue(
                SpreadRecoveryContext {
                    root: &pipeline_storage.root,
                    account_id: worker.account_id.as_deref(),
                    venue_id: &venue_id,
                    accept_any_venue: false,
                    order_validator: Some(&validator),
                    pipeline: &mut pipeline,
                    worker_id: &worker.id,
                    now,
                    source_seq: &mut source_seq,
                },
                venue,
            )?;
            for message in diagnostics {
                eprintln!("[HedgeRecovery] {message}");
            }
            metrics.absorb(&pipeline);
            // 扫描后仍停在原处才计一次原地不动：分组数少了说明这一轮真的在推进。
            let pending_after = pending_spread_recovery_groups_for_account(
                &pipeline_storage.root,
                &venue_id,
                worker.account_id.as_deref(),
            )?;
            recovery_stalls = if pending_after >= pending_before {
                recovery_stalls.saturating_add(1)
            } else {
                0
            };
        } else {
            recovery_stalls = 0;
        }
        context.heartbeat(now)?;
        metrics.publish_if_due(true, now);
        Ok(spread_recovery_poll_delay(recovery_stalls).as_millis() as u64)
    })?;
    metrics.publish(false, runtime_timestamp_ms());
    Ok(())
}

pub(crate) fn run_binance_worker(path: &Path, worker_id: &str, once: bool) -> Result<(), String> {
    catch_panic("worker-entry", || {
        run_binance_worker_inner(path, worker_id, once)
    })
}

#[cfg(test)]
pub(crate) fn managed_worker_args(
    config: &RuntimeConfig,
    config_path: &Path,
    allow_unmanaged_roles: bool,
) -> Result<Vec<(String, Vec<String>)>, String> {
    Ok(
        qx_orchestrator::plan_workers(config, config_path, allow_unmanaged_roles)?
            .into_iter()
            .map(|launch| (launch.worker_id, launch.args))
            .collect(),
    )
}

/// CLI 配置加载与命令错误适配；服务宿主负责跨平台进程托管。
pub(crate) fn run_process_supervisor(
    path: &Path,
    allow_unmanaged_roles: bool,
) -> Result<(), SuperviseFailure> {
    let config = read_runtime_config(path)?;
    validate_configured_market_specs(path, &config)?;
    let executable =
        std::env::current_exe().map_err(|error| format!("解析 qx-cli 可执行文件失败: {error}"))?;
    let work_dir = std::env::current_dir().map_err(|error| format!("读取工作目录失败: {error}"))?;
    qx_service::worker_host::supervise_processes(
        &config,
        path,
        &executable,
        &work_dir,
        allow_unmanaged_roles,
    )
}

pub(crate) fn run_ccxt_worker(
    path: &Path,
    worker_id: &str,
    ccxt_config_path: &Path,
    once: bool,
) -> Result<(), String> {
    catch_panic("worker-entry", || {
        run_ccxt_worker_inner(path, worker_id, ccxt_config_path, once)
    })
}

pub(crate) fn run_ccxt_spread_recovery_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    ccxt_config_path: String,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    let python = python_interpreter();
    let venue_id = recovery_worker_venue_id(&worker)?;
    let runtime_config = read_runtime_config(&runtime_config_path)?;
    let settlement_currency = account_worker_settlement_currency(&runtime_config, &worker)?;
    let log_name = required_account_event_log(&worker)?;
    let mut metrics = PipelineMetricsReporter::new(&runtime_config, context.id(), &log_name);
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!("ccxt spread recovery scanning venue={venue_id}"),
        Some(runtime_timestamp_ms()),
    )?;
    metrics.publish_if_due(true, runtime_timestamp_ms());
    let mut recovery_stalls = 0_u32;
    qx_service::worker_host::run_polling_worker(&context, once, || {
        let now = runtime_timestamp_ms();
        let pending_before = pending_spread_recovery_groups_for_account(
            &pipeline_storage.root,
            &venue_id,
            worker.account_id.as_deref(),
        )?;
        if pending_before > 0 {
            let mut pipeline = pipeline_storage
                .open(log_name.clone(), settlement_currency.clone())
                .map_err(|error| format!("打开 CCXT 多腿恢复 EventLog 失败: {error}"))?;
            let active_orders = pipeline.orders();
            qx_service::execution_capability_gate::require_active_orders_execution_capability(
                active_orders.iter(),
                &worker,
                &runtime_config.environment,
                &runtime_config_path,
                qx_service::execution_capability_gate::ExecutionConnectorProfile::Ccxt {
                    config_path: Path::new(&ccxt_config_path),
                },
                true,
                &mut pipeline,
                now,
            )?;
            // 每轮扫描起一个 Python 子进程：只有过预算的重生才是有界的（V13 R2 #168c）。
            let client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
                .map_err(|error| format!("启动公共 CCXT 恢复 Worker 失败: {error}"))?;
            let venue = CcxtProcessVenue::new(venue_id.clone(), Box::new(client));
            let mut source_seq = pipeline
                .log()
                .events()
                .last()
                .map(|event| event.source_seq)
                .unwrap_or(0);
            let validator =
                recovery_order_validator(&worker, &pipeline, Some(&runtime_config_path));
            let (_, diagnostics) = recover_spread_groups_for_venue(
                SpreadRecoveryContext {
                    root: &pipeline_storage.root,
                    account_id: worker.account_id.as_deref(),
                    venue_id: &venue_id,
                    accept_any_venue: false,
                    order_validator: Some(&validator),
                    pipeline: &mut pipeline,
                    worker_id: &worker.id,
                    now,
                    source_seq: &mut source_seq,
                },
                venue,
            )?;
            for message in diagnostics {
                eprintln!("[HedgeRecovery] {message}");
            }
            metrics.absorb(&pipeline);
            let pending_after = pending_spread_recovery_groups_for_account(
                &pipeline_storage.root,
                &venue_id,
                worker.account_id.as_deref(),
            )?;
            recovery_stalls = if pending_after >= pending_before {
                recovery_stalls.saturating_add(1)
            } else {
                0
            };
        } else {
            recovery_stalls = 0;
        }
        context.heartbeat(now)?;
        metrics.publish_if_due(true, now);
        Ok(spread_recovery_poll_delay(recovery_stalls).as_millis() as u64)
    })?;
    metrics.publish(false, runtime_timestamp_ms());
    Ok(())
}

/// 下载公共 CCXT OHLCV 快照，输出为 qianxing_bridge.BarFrame JSON，作为回测
/// 的不可变输入；回测运行期间不再访问交易所。
pub(crate) fn run_ccxt_fetch_ohlcv(
    ccxt_config_path: &Path,
    instrument: &str,
    timeframe: &str,
    start_ms: u64,
    end_ms: u64,
    output_path: &Path,
) -> Result<(), String> {
    let python = python_interpreter();
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path.to_string_lossy(), None)?;
    let result = client
        .call(serde_json::json!({
            "op": "fetch_ohlcv",
            "instrument": instrument,
            "timeframe": timeframe,
            "start_ms": start_ms,
            "end_ms": end_ms,
        }))
        .map_err(|error| format!("CCXT OHLCV 查询失败: {error}"))?;
    let frame = result
        .get("frame")
        .ok_or_else(|| "CCXT OHLCV 响应缺少 frame".to_string())?;
    std::fs::write(
        output_path,
        serde_json::to_string_pretty(frame)
            .map_err(|error| format!("编码 OHLCV 快照失败: {error}"))?,
    )
    .map_err(|error| format!("写入 OHLCV 快照失败 {}: {error}", output_path.display()))?;
    println!(
        "[CCXT · OHLCV] instrument={} timeframe={} output={} ✓",
        instrument,
        timeframe,
        output_path.display()
    );
    Ok(())
}

pub(crate) fn run_ccxt_market_spec(
    ccxt_config_path: &Path,
    instrument: &str,
    output_path: &Path,
) -> Result<(), String> {
    let python = python_interpreter();
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path.to_string_lossy(), None)?;
    let result = client
        .call(serde_json::json!({
            "op": "resolve_market",
            "instrument": instrument,
        }))
        .map_err(|error| format!("CCXT market spec 查询失败: {error}"))?;
    let mut market = result
        .get("market")
        .cloned()
        .ok_or_else(|| "CCXT market spec 响应缺少 market".to_string())?;
    let tiers = match client.call(serde_json::json!({
        "op": "fetch_leverage_tiers",
        "instrument": instrument,
    })) {
        Ok(value) => value
            .get("tiers")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
        Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {
            serde_json::json!([])
        }
        Err(error) => return Err(format!("CCXT leverage tiers 查询失败: {error}")),
    };
    if let Some(object) = market.as_object_mut() {
        object.insert("leverage_tiers".into(), tiers);
    }
    std::fs::write(
        output_path,
        serde_json::to_string_pretty(&market)
            .map_err(|error| format!("编码 market spec 失败: {error}"))?,
    )
    .map_err(|error| format!("写入 market spec 失败 {}: {error}", output_path.display()))?;
    println!(
        "[CCXT · MarketSpec] instrument={} output={} ✓",
        instrument,
        output_path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_runtime::WorkerConfig;
    use qx_runtime::WorkerRole;

    fn worker(venue_id: Option<String>) -> WorkerConfig {
        WorkerConfig {
            id: "w1".into(),
            role: WorkerRole::SpreadRecovery,
            enabled: true,
            account_id: Some("acct".into()),
            venue_id,
            endpoint: None,
            symbols: Vec::new(),
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        }
    }

    #[test]
    fn recovery_worker_requires_an_explicit_venue_id() {
        // 缺 venue_id 必须当场拒启动：恢复扫描按 venue 过滤敞口腿，静默收窄范围
        // 等于让其他 venue 的裸腿永久挂着，而运维看不到任何告警。
        let error = recovery_worker_venue_id(&worker(None))
            .expect_err("缺 venue_id 的恢复 worker 不得启动");
        assert!(
            error.contains("w1") && error.contains("venue_id"),
            "报错要能定位到 worker 与缺失的字段，实际 {error}"
        );
        assert_eq!(
            recovery_worker_venue_id(&worker(Some("binance".into()))).as_deref(),
            Ok("binance")
        );
    }
}
