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
    recover_spread_groups_for_venue_until, recovery_order_validator, required_account_event_log,
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
            let (_, diagnostics, stopped) = recover_spread_groups_for_venue_until(
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
                || context.should_stop(),
            )?;
            for message in diagnostics {
                eprintln!("[HedgeRecovery] {message}");
            }
            if stopped {
                return Ok(1);
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
            let (_, diagnostics, stopped) = recover_spread_groups_for_venue_until(
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
                || context.should_stop(),
            )?;
            for message in diagnostics {
                eprintln!("[HedgeRecovery] {message}");
            }
            if stopped {
                return Ok(1);
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
    manifest_path: Option<&Path>,
) -> Result<(), String> {
    let interval_ms = timeframe_interval_ms(timeframe)?;
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
    let manifest_path = manifest_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_dataset_manifest_path(output_path));
    if manifest_path == output_path {
        return Err("BarFrame 与 DatasetManifestV2 不能写入同一路径".into());
    }
    let frame_json = serde_json::to_vec_pretty(frame)
        .map_err(|error| format!("编码 OHLCV 快照失败: {error}"))?;
    let instrument = frame
        .get("instrument")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "CCXT OHLCV 响应的 frame.instrument 无效".to_string())?;
    let provider_source = frame
        .get("source")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "CCXT OHLCV 响应的 frame.source 无效".to_string())?;
    let manifest = publish_ccxt_frame_with_manifest(
        &frame_json,
        instrument,
        provider_source,
        timeframe,
        start_ms,
        end_ms,
        output_path,
        &manifest_path,
        interval_ms,
    )?;
    println!(
        "[CCXT · OHLCV] instrument={} timeframe={} rows={} duplicate={} out_of_order={} missing_intervals={} output={} manifest={} ✓",
        instrument,
        timeframe,
        manifest.quality_report.row_count,
        manifest.quality_report.duplicate_rows,
        manifest.quality_report.out_of_order_rows,
        manifest.quality_report.missing_intervals,
        output_path.display(),
        manifest_path.display(),
    );
    for warning in manifest.warnings() {
        eprintln!("[CCXT · OHLCV · 质量提示] {warning}");
    }
    Ok(())
}

fn timeframe_interval_ms(timeframe: &str) -> Result<u64, String> {
    let unit_start = timeframe
        .find(|character: char| !character.is_ascii_digit())
        .ok_or_else(|| format!("不支持的 CCXT timeframe: {timeframe}"))?;
    let amount = timeframe[..unit_start]
        .parse::<u64>()
        .map_err(|_| format!("不支持的 CCXT timeframe: {timeframe}"))?;
    let unit_ms = match &timeframe[unit_start..] {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 60 * 60_000,
        "d" => 24 * 60 * 60_000,
        "w" => 7 * 24 * 60 * 60_000,
        unit => {
            return Err(format!(
                "CCXT timeframe {timeframe} 不是固定长度档位（单位 {unit}），无法生成准确质量清单"
            ));
        }
    };
    if amount == 0 {
        return Err(format!("CCXT timeframe 必须为正整数: {timeframe}"));
    }
    amount
        .checked_mul(unit_ms)
        .ok_or_else(|| format!("CCXT timeframe 毫秒长度溢出: {timeframe}"))
}

fn default_dataset_manifest_path(frame_path: &Path) -> PathBuf {
    let stem = frame_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("bars");
    frame_path.with_file_name(format!("{stem}.manifest.json"))
}

fn publish_ccxt_frame_with_manifest(
    frame_json: &[u8],
    instrument: &str,
    provider_source: &str,
    timeframe: &str,
    start_ms: u64,
    end_ms: u64,
    frame_path: &Path,
    manifest_path: &Path,
    interval_ms: u64,
) -> Result<qx_data::DatasetManifestV2, String> {
    if frame_path == manifest_path {
        return Err("BarFrame 与 DatasetManifestV2 不能写入同一路径".into());
    }
    static TEMP_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nonce = TEMP_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let output_parent = frame_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(output_parent)
        .map_err(|error| format!("创建 CCXT 数据输出目录失败: {error}"))?;
    let temporary_root =
        output_parent.join(format!(".qx-ccxt-bars-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&temporary_root)
        .map_err(|error| format!("创建 CCXT 临时数据目录失败: {error}"))?;
    let result = (|| {
        let temporary_frame = temporary_root.join("bars.json");
        let temporary_manifest = temporary_root.join("bars.manifest.json");
        std::fs::write(&temporary_frame, frame_json)
            .map_err(|error| format!("写入 CCXT 临时 BarFrame 失败: {error}"))?;
        let dataset_key = format!("{provider_source}\n{instrument}\n{timeframe}");
        let dataset_digest = qx_strategy::sha256_hex(dataset_key.as_bytes());
        let dataset_id = format!("ccxt-{}", &dataset_digest[..16]);
        let spec = qx_app::DatasetManifestValidationSpec::new(
            temporary_frame.to_string_lossy(),
            dataset_id,
            format!("{timeframe}-{start_ms}-{end_ms}"),
            interval_ms,
            "UTC",
            temporary_manifest.to_string_lossy(),
        );
        let context = qx_app::RunContext::new(
            qx_app::CallerCapability::Research,
            format!("ccxt-fetch-{timeframe}-{start_ms}-{end_ms}"),
        );
        let validation = qx_app::validate_dataset_manifest(&spec, &context)
            .map_err(|error| format!("CCXT OHLCV 质量验证失败: {error}"))?;
        let manifest_bytes = std::fs::read(&temporary_manifest)
            .map_err(|error| format!("读取 CCXT 临时质量清单失败: {error}"))?;
        // Check both immutable destinations before publishing either file. This prevents a
        // known conflict on one sidecar from leaving its matching BarFrame behind (or vice
        // versa). Each final rename remains atomic; cross-process pair transactions are not
        // claimed here.
        ensure_immutable_target(frame_path, frame_json)?;
        ensure_immutable_target(manifest_path, &manifest_bytes)?;
        qx_app::artifact_store::sink::publish_bytes(
            frame_path,
            frame_json,
            qx_app::artifact_store::sink::PublishMode::Idempotent,
        )
        .map_err(|error| format!("发布 CCXT BarFrame {} 失败: {error}", frame_path.display()))?;
        qx_app::artifact_store::sink::publish_bytes(
            manifest_path,
            &manifest_bytes,
            qx_app::artifact_store::sink::PublishMode::Idempotent,
        )
        .map_err(|error| {
            format!(
                "发布 CCXT DatasetManifestV2 {} 失败: {error}",
                manifest_path.display()
            )
        })?;
        Ok(validation.manifest)
    })();
    let _ = std::fs::remove_dir_all(&temporary_root);
    result
}

fn ensure_immutable_target(path: &Path, payload: &[u8]) -> Result<(), String> {
    match std::fs::read(path) {
        Ok(existing) if existing == payload => Ok(()),
        Ok(_) => Err(format!(
            "同一数据集产物路径已存在不同内容: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "检查已有数据集产物 {} 失败: {error}",
            path.display()
        )),
    }
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
        let role = match object
            .get("market_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
        {
            "spot" | "margin" => "crypto_spot",
            "swap" | "perpetual" | "future" | "futures" => "future",
            _ => "unknown",
        };
        object.insert(
            "dataset_instrument_role".into(),
            serde_json::Value::String(role.into()),
        );
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
    fn fixed_ccxt_timeframes_have_exact_intervals_and_calendar_months_are_rejected() {
        assert_eq!(timeframe_interval_ms("15m").unwrap(), 900_000);
        assert_eq!(timeframe_interval_ms("2h").unwrap(), 7_200_000);
        assert_eq!(timeframe_interval_ms("1w").unwrap(), 604_800_000);
        assert!(timeframe_interval_ms("1M")
            .unwrap_err()
            .contains("固定长度档位"));
        assert!(timeframe_interval_ms("0m").is_err());
        assert!(timeframe_interval_ms("18446744073709551615w").is_err());
    }

    #[test]
    fn ccxt_frame_and_manifest_publish_as_immutable_quality_checked_inputs() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("target/qx-cli-test-tmp")
            .join(format!("ccxt-manifest-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let frame_path = root.join("bars.json");
        let manifest_path = default_dataset_manifest_path(&frame_path);
        let payload = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "instrument": "BTCUSDT.BINANCE",
            "source": "ccxt:binance:BTC/USDT:1m",
            "ts": [60_000, 120_000, 180_000],
            "open_raw": [10, 11, 12],
            "high_raw": [12, 13, 14],
            "low_raw": [9, 10, 11],
            "close_raw": [11, 12, 13],
            "volume_raw": [100, 101, 102]
        }))
        .unwrap();

        let manifest = publish_ccxt_frame_with_manifest(
            &payload,
            "BTCUSDT.BINANCE",
            "ccxt:binance:BTC/USDT:1m",
            "1m",
            60_000,
            180_000,
            &frame_path,
            &manifest_path,
            60_000,
        )
        .unwrap();
        assert_eq!(manifest.quality_report.row_count, 3);
        assert_eq!(manifest.quality_report.duplicate_rows, 0);
        assert_eq!(manifest.quality_report.missing_intervals, 0);
        assert_eq!(
            qx_data::DatasetManifestV2::from_json(
                &std::fs::read_to_string(&manifest_path).unwrap()
            )
            .unwrap(),
            manifest
        );

        let changed = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "instrument": "BTCUSDT.BINANCE",
            "source": "ccxt:binance:BTC/USDT:1m",
            "ts": [60_000, 120_000, 180_000],
            "open_raw": [10, 11, 12],
            "high_raw": [12, 13, 14],
            "low_raw": [9, 10, 11],
            "close_raw": [11, 12, 14],
            "volume_raw": [100, 101, 102]
        }))
        .unwrap();
        assert!(publish_ccxt_frame_with_manifest(
            &changed,
            "BTCUSDT.BINANCE",
            "ccxt:binance:BTC/USDT:1m",
            "1m",
            60_000,
            180_000,
            &frame_path,
            &manifest_path,
            60_000,
        )
        .is_err());
        assert_eq!(std::fs::read(&frame_path).unwrap(), payload);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ccxt_manifest_conflict_does_not_leave_a_partial_bar_frame() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("target/qx-cli-test-tmp")
            .join(format!(
                "ccxt-manifest-conflict-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let frame_path = root.join("bars.json");
        let manifest_path = default_dataset_manifest_path(&frame_path);
        std::fs::write(&manifest_path, b"different existing manifest").unwrap();
        let payload = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "instrument": "BTCUSDT.BINANCE",
            "source": "ccxt:binance:BTC/USDT:1m",
            "ts": [60_000, 120_000],
            "open_raw": [10, 11],
            "high_raw": [12, 13],
            "low_raw": [9, 10],
            "close_raw": [11, 12],
            "volume_raw": [100, 101]
        }))
        .unwrap();

        let error = publish_ccxt_frame_with_manifest(
            &payload,
            "BTCUSDT.BINANCE",
            "ccxt:binance:BTC/USDT:1m",
            "1m",
            60_000,
            120_000,
            &frame_path,
            &manifest_path,
            60_000,
        )
        .unwrap_err();

        assert!(error.contains("不同内容"));
        assert!(!frame_path.exists());
        assert_eq!(
            std::fs::read(&manifest_path).unwrap(),
            b"different existing manifest"
        );
        std::fs::remove_dir_all(root).unwrap();
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
