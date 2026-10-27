//! Frozen market specification loading shared by service worker hosts.

use qx_core::{InstrumentId, TradingInstrumentSpec};
use qx_runtime::{LiveEventPipeline, RuntimeConfig, WorkerConfig};
use qx_zhenlu::VenueEvent;
use serde_json::Value;
use std::path::{Path, PathBuf};

const WORKER_MARKET_SPEC_SET_KIND: &str = "qianxing_market_spec_set";
const WORKER_MARKET_SPEC_SET_VERSION: u32 = 1;

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
            VenueEvent::FeeAdjustment { order_id, .. } => *order_id,
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
    let document: Value = serde_json::from_str(&payload).map_err(|error| {
        format!(
            "worker market spec JSON 无效 {}: {error}",
            resolved_spec_path.display()
        )
    })?;
    let market = selected_market_payload(&document, worker, instrument)?
        .cloned()
        .ok_or_else(|| {
            format!(
                "FAIL_CLOSED: worker {} 的冻结 market spec 未绑定标的 {instrument}",
                worker.id
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

/// Check whether a worker's configured market-spec document binds this instrument.
/// This lets risk resolution choose among several worker files without treating a
/// different legacy single-market file as a configuration error.
pub fn worker_market_spec_matches(
    worker: &WorkerConfig,
    instrument: &InstrumentId,
    runtime_config_path: Option<&Path>,
) -> Result<bool, String> {
    let Some(spec_path) = worker.instrument_spec_path.as_deref() else {
        return Ok(false);
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
    let document: Value = serde_json::from_str(&payload).map_err(|error| {
        format!(
            "worker market spec JSON 无效 {}: {error}",
            resolved_spec_path.display()
        )
    })?;
    Ok(selected_market_payload(&document, worker, instrument)?.is_some())
}

fn selected_market_payload<'a>(
    document: &'a Value,
    worker: &WorkerConfig,
    instrument: &InstrumentId,
) -> Result<Option<&'a Value>, String> {
    if let Some(markets) = validate_market_spec_set_document(worker, document)? {
        let Some(market) = markets.get(&instrument.to_string()) else {
            return Ok(None);
        };
        return Ok(Some(market));
    }

    if document.get("base_currency").is_some() {
        let declared_instrument = document.get("instrument").and_then(|value| {
            value
                .as_str()
                .and_then(InstrumentId::parse)
                .or_else(|| serde_json::from_value::<InstrumentId>(value.clone()).ok())
        });
        let Some(declared_instrument) = declared_instrument else {
            return Err(format!(
                "FAIL_CLOSED: worker {} 的 market spec 缺少可解析的 instrument，无法验证 {instrument} 对应的规格",
                worker.id
            ));
        };
        return Ok((declared_instrument == *instrument).then_some(document));
    }

    if let Some(symbol) = document.get("symbol").and_then(Value::as_str) {
        let exchange_matches = document
            .get("exchange_id")
            .and_then(Value::as_str)
            .is_none_or(|exchange| exchange.eq_ignore_ascii_case(instrument.venue.as_str()));
        return Ok((symbol == instrument.symbol && exchange_matches).then_some(document));
    }
    if worker.symbols.len() != 1 {
        return Err(format!(
            "FAIL_CLOSED: worker {} 的 CCXT market spec 没有 symbol 字段且绑定了多个标的，无法证明 {instrument} 对应的规格",
            worker.id
        ));
    }
    Ok(Some(document))
}

/// Validate every entry in a shared set, even if the current worker does not bind a
/// symbol list. This keeps wildcard workers from bypassing startup preflight.
fn validate_market_spec_set_document<'a>(
    worker: &WorkerConfig,
    document: &'a Value,
) -> Result<Option<&'a serde_json::Map<String, Value>>, String> {
    let Some(kind_value) = document.get("kind") else {
        return Ok(None);
    };
    let Some(kind) = kind_value.as_str() else {
        return Err(format!(
            "worker {} 的 market spec kind 必须是字符串",
            worker.id
        ));
    };
    if kind != WORKER_MARKET_SPEC_SET_KIND {
        return Err(format!(
            "worker {} 的 market spec kind 不支持: {kind}",
            worker.id
        ));
    }
    let version = document
        .get("schema_version")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            format!(
                "worker {} 的 market spec set 缺少有效 schema_version",
                worker.id
            )
        })?;
    if version != WORKER_MARKET_SPEC_SET_VERSION {
        return Err(format!(
            "worker {} 的 market spec set 版本不支持: {version}",
            worker.id
        ));
    }
    let markets = document
        .get("markets")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("worker {} 的 market spec set 缺少 markets 对象", worker.id))?;
    if markets.is_empty() {
        return Err(format!(
            "worker {} 的 market spec set markets 不能为空",
            worker.id
        ));
    }
    for (key, market) in markets {
        let entry_instrument = InstrumentId::parse(key).ok_or_else(|| {
            format!(
                "worker {} 的 market spec set 含有非法 InstrumentId 键 {key}",
                worker.id
            )
        })?;
        if entry_instrument.to_string() != *key {
            return Err(format!(
                "worker {} 的 market spec set 键 {key} 不是规范 InstrumentId",
                worker.id
            ));
        }
        validate_market_set_entry(worker, &entry_instrument, market)?;
    }
    Ok(Some(markets))
}

fn validate_market_set_entry(
    worker: &WorkerConfig,
    instrument: &InstrumentId,
    market: &Value,
) -> Result<(), String> {
    if market.get("base_currency").is_some() {
        let declared_instrument = market.get("instrument").and_then(|value| {
            value
                .as_str()
                .and_then(InstrumentId::parse)
                .or_else(|| serde_json::from_value::<InstrumentId>(value.clone()).ok())
        });
        if declared_instrument.as_ref() != Some(instrument) {
            return Err(format!(
                "worker {} 的 market spec set 键 {instrument} 与产品规格 instrument 不一致",
                worker.id
            ));
        }
        return Ok(());
    }
    let symbol_matches = market
        .get("symbol")
        .and_then(Value::as_str)
        .is_some_and(|symbol| symbol == instrument.symbol);
    let exchange_matches = market
        .get("exchange_id")
        .and_then(Value::as_str)
        .is_none_or(|exchange| exchange.eq_ignore_ascii_case(instrument.venue.as_str()));
    if !symbol_matches || !exchange_matches {
        return Err(format!(
            "worker {} 的 market spec set 键 {instrument} 与 CCXT market symbol/exchange_id 不一致",
            worker.id
        ));
    }
    Ok(())
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
    match std::fs::read_to_string(&resolved)
        .map_err(|error| format!("读取 worker market spec 失败: {error}"))
        .and_then(|payload| {
            serde_json::from_str::<Value>(&payload)
                .map_err(|error| format!("worker market spec JSON 无效: {error}"))
        })
        .and_then(|document| validate_market_spec_set_document(worker, &document).map(|_| ()))
    {
        Ok(()) => {}
        Err(error) => {
            failures.push(format!(
                "worker {} instrument_spec_path 集合预检失败: {error}",
                worker.id
            ));
            return failures;
        }
    }
    for symbol in &worker.symbols {
        let Some(instrument) = qx_core::InstrumentId::parse(symbol) else {
            failures.push(format!(
                "worker {} instrument_spec_path 无法绑定非法标的 {symbol}",
                worker.id
            ));
            continue;
        };
        match load_worker_market_spec_for_instrument(worker, &instrument, Some(runtime_path)) {
            Ok(Some(_)) => {}
            Ok(None) => failures.push(format!(
                "worker {} instrument_spec_path 未提供标的 {symbol} 的市场规格",
                worker.id
            )),
            Err(error) => failures.push(format!(
                "worker {} instrument_spec_path 与标的 {symbol} 预检失败: {error}",
                worker.id
            )),
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_runtime::WorkerRole;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn instrument(symbol: &str) -> InstrumentId {
        InstrumentId::parse(&format!("{symbol}.OKX")).unwrap()
    }

    fn product_spec(instrument: &InstrumentId, qty_step: i128) -> Value {
        serde_json::json!({
            "instrument": instrument,
            "product": "perpetual",
            "base_currency": instrument.symbol.split('/').next().unwrap(),
            "quote_currency": "USDT",
            "settlement_currency": "USDT",
            "contract_size": 1_000_000,
            "linear": true,
            "inverse": false,
            "price_tick": 100_000_000,
            "qty_step": qty_step,
            "min_qty": qty_step,
            "max_leverage": 100,
            "maintenance_margin_bps": 500,
            "valid_from": 0,
            "valid_to": null
        })
    }

    fn worker(spec_path: &str, symbols: &[String]) -> WorkerConfig {
        WorkerConfig {
            id: "okx-main-execution".into(),
            role: WorkerRole::Execution,
            enabled: true,
            account_id: Some("main".into()),
            venue_id: Some("OKX".into()),
            endpoint: None,
            symbols: symbols.to_vec(),
            settlement_currency: Some("USDT".into()),
            credential_env: None,
            credential_files: None,
            instrument_spec_path: Some(spec_path.into()),
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        }
    }

    fn temp_runtime() -> (PathBuf, PathBuf) {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("qx-worker-market-spec-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runtime_path = dir.join("runtime.json");
        let set_path = dir.join("market-spec-set.json");
        (runtime_path, set_path)
    }

    #[test]
    fn one_worker_loads_a_distinct_frozen_spec_for_each_market() {
        let btc = instrument("BTC/USDT:USDT");
        let eth = instrument("ETH/USDT:USDT");
        let symbols = vec![btc.to_string(), eth.to_string()];
        let worker = worker("market-spec-set.json", &symbols);
        let (runtime_path, set_path) = temp_runtime();
        let markets = serde_json::json!({
            btc.to_string(): product_spec(&btc, 1_000_000_000),
            eth.to_string(): product_spec(&eth, 100_000_000),
        });
        std::fs::write(
            &set_path,
            serde_json::to_vec(&serde_json::json!({
                "kind": WORKER_MARKET_SPEC_SET_KIND,
                "schema_version": WORKER_MARKET_SPEC_SET_VERSION,
                "markets": markets,
            }))
            .unwrap(),
        )
        .unwrap();

        assert!(validate_worker_market_specs(&runtime_path, &worker).is_ok());
        assert!(worker_market_spec_matches(&worker, &btc, Some(&runtime_path)).unwrap());
        assert!(worker_market_spec_matches(&worker, &eth, Some(&runtime_path)).unwrap());
        assert_eq!(
            load_worker_market_spec_for_instrument(&worker, &btc, Some(&runtime_path))
                .unwrap()
                .unwrap()
                .qty_step,
            1_000_000_000
        );
        assert_eq!(
            load_worker_market_spec_for_instrument(&worker, &eth, Some(&runtime_path))
                .unwrap()
                .unwrap()
                .qty_step,
            100_000_000
        );
        std::fs::remove_dir_all(runtime_path.parent().unwrap()).unwrap();
    }

    #[test]
    fn multi_market_spec_set_fails_closed_for_missing_or_miskeyed_entries() {
        let btc = instrument("BTC/USDT:USDT");
        let eth = instrument("ETH/USDT:USDT");
        let symbols = vec![btc.to_string(), eth.to_string()];
        let worker = worker("market-spec-set.json", &symbols);
        let (runtime_path, set_path) = temp_runtime();

        std::fs::write(
            &set_path,
            serde_json::to_vec(&serde_json::json!({
                "kind": WORKER_MARKET_SPEC_SET_KIND,
                "schema_version": WORKER_MARKET_SPEC_SET_VERSION,
                "markets": {
                    btc.to_string(): product_spec(&btc, 1_000_000_000),
                },
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(validate_worker_market_specs(&runtime_path, &worker)
            .unwrap_err()
            .contains("ETH/USDT:USDT.OKX"));

        std::fs::write(
            &set_path,
            serde_json::to_vec(&serde_json::json!({
                "kind": WORKER_MARKET_SPEC_SET_KIND,
                "schema_version": WORKER_MARKET_SPEC_SET_VERSION,
                "markets": {
                    btc.to_string(): product_spec(&eth, 1_000_000_000),
                    eth.to_string(): product_spec(&eth, 100_000_000),
                },
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(
            load_worker_market_spec_for_instrument(&worker, &btc, Some(&runtime_path))
                .unwrap_err()
                .contains("不一致")
        );
        std::fs::remove_dir_all(runtime_path.parent().unwrap()).unwrap();
    }

    #[test]
    fn preflight_validates_unselected_entries_in_a_shared_market_spec_set() {
        let btc = instrument("BTC/USDT:USDT");
        let eth = instrument("ETH/USDT:USDT");
        let worker = worker("market-spec-set.json", &[btc.to_string()]);
        let (runtime_path, set_path) = temp_runtime();

        std::fs::write(
            &set_path,
            serde_json::to_vec(&serde_json::json!({
                "kind": WORKER_MARKET_SPEC_SET_KIND,
                "schema_version": WORKER_MARKET_SPEC_SET_VERSION,
                "markets": {
                    btc.to_string(): product_spec(&btc, 1_000_000_000),
                    eth.to_string(): product_spec(&btc, 100_000_000),
                },
            }))
            .unwrap(),
        )
        .unwrap();

        assert!(validate_worker_market_specs(&runtime_path, &worker)
            .unwrap_err()
            .contains("与产品规格 instrument 不一致"));
        std::fs::remove_dir_all(runtime_path.parent().unwrap()).unwrap();
    }

    #[test]
    fn wildcard_worker_cannot_skip_invalid_market_spec_set_preflight() {
        let worker = worker("market-spec-set.json", &[]);
        let (runtime_path, set_path) = temp_runtime();
        std::fs::write(
            &set_path,
            serde_json::to_vec(&serde_json::json!({
                "kind": WORKER_MARKET_SPEC_SET_KIND,
                "schema_version": WORKER_MARKET_SPEC_SET_VERSION,
                "markets": {},
            }))
            .unwrap(),
        )
        .unwrap();

        assert!(validate_worker_market_specs(&runtime_path, &worker)
            .unwrap_err()
            .contains("markets 不能为空"));

        std::fs::write(
            &set_path,
            serde_json::to_vec(&serde_json::json!({
                "kind": 1,
                "schema_version": WORKER_MARKET_SPEC_SET_VERSION,
                "markets": {"BTC/USDT:USDT.OKX": {"symbol": "BTC/USDT:USDT"}},
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(validate_worker_market_specs(&runtime_path, &worker)
            .unwrap_err()
            .contains("kind 必须是字符串"));
        std::fs::remove_dir_all(runtime_path.parent().unwrap()).unwrap();
    }

    #[test]
    fn legacy_single_market_spec_remains_supported_but_cannot_cover_two_symbols() {
        let btc = instrument("BTC/USDT:USDT");
        let symbols = vec![btc.to_string(), instrument("ETH/USDT:USDT").to_string()];
        let worker = worker("market-spec-set.json", &symbols);
        let (runtime_path, set_path) = temp_runtime();
        std::fs::write(
            &set_path,
            serde_json::to_vec(&product_spec(&btc, 1_000_000_000)).unwrap(),
        )
        .unwrap();

        assert!(
            load_worker_market_spec_for_instrument(&worker, &btc, Some(&runtime_path))
                .unwrap()
                .is_some()
        );
        assert!(load_worker_market_spec_for_instrument(
            &worker,
            &instrument("ETH/USDT:USDT"),
            Some(&runtime_path)
        )
        .is_err());
        std::fs::remove_dir_all(runtime_path.parent().unwrap()).unwrap();
    }
}
