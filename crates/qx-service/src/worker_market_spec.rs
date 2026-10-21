//! Frozen market specification loading shared by service worker hosts.

use qx_core::{InstrumentId, TradingInstrumentSpec};
use qx_runtime::{LiveEventPipeline, RuntimeConfig, WorkerConfig};
use qx_zhenlu::VenueEvent;
use std::path::{Path, PathBuf};

/// Resolve the single frozen specification covering one batch of venue reports.
pub fn worker_report_spec(
    worker: &WorkerConfig,
    events: &[VenueEvent],
    pipeline: &LiveEventPipeline,
    runtime_config_path: Option<&Path>,
) -> Result<Option<TradingInstrumentSpec>, String> {
    if worker.instrument_spec_path.is_none() {
        return Ok(None);
    }
    let orders = pipeline.orders();
    let mut resolved: Option<TradingInstrumentSpec> = None;
    for event in events {
        let client_order_id = match event {
            VenueEvent::Accepted {
                client_order_id, ..
            }
            | VenueEvent::Cancelled {
                client_order_id, ..
            } => *client_order_id,
            VenueEvent::Fill(fill) => fill.order_id,
        };
        let order = orders
            .iter()
            .find(|order| order.client_id == client_order_id)
            .ok_or_else(|| {
                format!(
                    "worker {} 回报订单 {client_order_id} 不在本地 EventLog，无法解析产品规格",
                    worker.id
                )
            })?;
        let Some(spec) =
            load_worker_market_spec_for_instrument(worker, &order.instrument, runtime_config_path)?
        else {
            return Ok(None);
        };
        match &resolved {
            None => resolved = Some(spec),
            Some(existing) if existing == &spec => {}
            Some(_) => {
                return Err(format!(
                    "worker {} 的一批回报跨多份产品规格，拒绝归约",
                    worker.id
                ));
            }
        }
    }
    Ok(resolved)
}

/// Load the frozen market spec configured for a worker and verify its instrument identity.
pub fn load_worker_market_spec_for_instrument(
    worker: &WorkerConfig,
    instrument: &InstrumentId,
    runtime_config_path: Option<&Path>,
) -> Result<Option<TradingInstrumentSpec>, String> {
    let Some(spec_path) = worker.instrument_spec_path.as_deref() else {
        return Ok(None);
    };
    let resolved_spec_path = runtime_config_path
        .map(|path| qx_runtime::resolve_runtime_relative_path(path, spec_path))
        .unwrap_or_else(|| PathBuf::from(spec_path));
    let payload = std::fs::read_to_string(&resolved_spec_path).map_err(|error| {
        format!(
            "读取 worker market spec 失败 {}: {error}",
            resolved_spec_path.display()
        )
    })?;
    let market: serde_json::Value = serde_json::from_str(&payload).map_err(|error| {
        format!(
            "worker market spec JSON 无效 {}: {error}",
            resolved_spec_path.display()
        )
    })?;
    qx_app::market_spec_from_value(instrument, &market)
        .map(Some)
        .map_err(|error| {
            format!(
                "读取 worker market spec 失败 {}: {}",
                resolved_spec_path.display(),
                error.message()
            )
        })
}

pub fn validate_worker_market_specs(
    runtime_path: &Path,
    worker: &qx_runtime::WorkerConfig,
) -> Result<(), String> {
    let failures = worker_market_spec_failures(runtime_path, worker);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("；"))
    }
}

pub fn validate_configured_market_specs(
    runtime_path: &Path,
    config: &RuntimeConfig,
) -> Result<(), String> {
    let failures = config
        .workers
        .iter()
        .filter(|worker| worker.enabled)
        .flat_map(|worker| worker_market_spec_failures(runtime_path, worker))
        .collect::<Vec<_>>();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("；"))
    }
}

pub fn worker_market_spec_failures(
    runtime_path: &Path,
    worker: &qx_runtime::WorkerConfig,
) -> Vec<String> {
    let mut failures = Vec::new();
    let Some(spec_path) = worker.instrument_spec_path.as_deref() else {
        return failures;
    };
    let resolved = qx_runtime::resolve_runtime_relative_path(runtime_path, spec_path);
    if !resolved.is_file() {
        failures.push(format!(
            "worker {} instrument_spec_path 文件不存在: {} (configured={spec_path})",
            worker.id,
            resolved.display()
        ));
        return failures;
    }
    for symbol in &worker.symbols {
        let Some(instrument) = qx_core::InstrumentId::parse(symbol) else {
            failures.push(format!(
                "worker {} instrument_spec_path 无法绑定非法标的 {symbol}",
                worker.id
            ));
            continue;
        };
        if let Err(error) =
            load_worker_market_spec_for_instrument(worker, &instrument, Some(runtime_path))
        {
            failures.push(format!(
                "worker {} instrument_spec_path 与标的 {symbol} 预检失败: {error}",
                worker.id
            ));
        }
    }
    failures
}
