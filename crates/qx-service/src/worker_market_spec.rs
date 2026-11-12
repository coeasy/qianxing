//! Frozen market specification loading shared by service worker hosts.

use qx_core::{InstrumentId, TradingInstrumentSpec, VenueFamily};
use qx_runtime::{LiveEventPipeline, RuntimeConfig, WorkerConfig};
use qx_zhenlu::VenueEvent;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
        let client_order_id = report_client_order_id(event);
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

fn report_client_order_id(event: &VenueEvent) -> u64 {
    match event {
        VenueEvent::Accepted {
            client_order_id, ..
        }
        | VenueEvent::Rejected {
            client_order_id, ..
        }
        | VenueEvent::Cancelled {
            client_order_id, ..
        } => *client_order_id,
        VenueEvent::Fill(fill) => fill.order_id,
        VenueEvent::FeeAdjustment { order_id, .. } => *order_id,
    }
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
    let spec = qx_app::market_spec_from_value(instrument, &market).map_err(|error| {
        format!(
            "读取 worker market spec 失败 {}: {}",
            resolved_spec_path.display(),
            error.message()
        )
    })?;
    validate_live_instrument_role(worker, instrument, &market, &spec)?;
    Ok(Some(spec))
}

fn live_execution_requires_role(worker: &WorkerConfig) -> bool {
    worker.role == qx_runtime::WorkerRole::Execution
        && worker
            .venue_id
            .as_deref()
            .is_some_and(|venue| VenueFamily::parse(venue) != VenueFamily::Paper)
}

fn validate_live_instrument_role(
    worker: &WorkerConfig,
    instrument: &InstrumentId,
    market: &Value,
    spec: &TradingInstrumentSpec,
) -> Result<(), String> {
    if !live_execution_requires_role(worker) {
        return Ok(());
    }
    let role_value = market
        .get("dataset_instrument_role")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!(
                "FAIL_CLOSED: live execution worker {} 的 market spec 缺少 dataset_instrument_role ({instrument})",
                worker.id
            )
        })?;
    let role: qx_data::DatasetInstrumentRole = serde_json::from_value(Value::String(
        role_value.to_string(),
    ))
    .map_err(|_| {
        format!(
            "FAIL_CLOSED: live execution worker {} 的 dataset_instrument_role 无效: {role_value} ({instrument})",
            worker.id
        )
    })?;
    if !role.is_tradable() {
        return Err(format!(
            "FAIL_CLOSED: live execution worker {} 拒绝非交易标的角色 {:?} ({instrument})",
            worker.id, role
        ));
    }
    if !role.is_compatible_with_product(spec.product) {
        return Err(format!(
            "FAIL_CLOSED: live execution worker {} 的标的角色 {:?} 与产品 {:?} 冲突 ({instrument})",
            worker.id, role, spec.product
        ));
    }
    let catalog_value = market
        .get("dataset_instrument_role_catalog")
        .ok_or_else(|| {
            format!(
                "FAIL_CLOSED: live execution worker {} 的 market spec 缺少签名角色目录 ({instrument})",
                worker.id
            )
        })?;
    let catalog: qx_data::DatasetInstrumentRoleCatalogV1 =
        serde_json::from_value(catalog_value.clone()).map_err(|error| {
            format!(
                "FAIL_CLOSED: live execution worker {} 的签名角色目录无效: {error} ({instrument})",
                worker.id
            )
        })?;
    let trusted_keys = trusted_dataset_role_keys().map_err(|error| {
        format!(
            "FAIL_CLOSED: live execution worker {} 无法加载角色目录信任根: {error}",
            worker.id
        )
    })?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before UNIX epoch: {error}"))?
        .as_millis()
        .try_into()
        .map_err(|_| "current timestamp exceeds u64".to_owned())?;
    catalog
        .validate_for_instrument(
            &instrument.to_string(),
            role,
            &trusted_keys,
            now_ms,
        )
        .map_err(|error| {
            format!(
                "FAIL_CLOSED: live execution worker {} 角色目录验签/标的校验失败: {error} ({instrument})",
                worker.id
            )
        })?;
    Ok(())
}

fn trusted_dataset_role_keys() -> Result<BTreeMap<String, qx_data::DatasetRoleTrustedKeyV1>, String>
{
    const ENV_NAME: &str = "QX_DATASET_ROLE_TRUSTED_PUBLIC_KEYS";
    let payload = std::env::var(ENV_NAME)
        .map_err(|_| format!("required environment variable {ENV_NAME} is unset"))?;
    let keys: BTreeMap<String, qx_data::DatasetRoleTrustedKeyV1> = serde_json::from_str(&payload)
        .map_err(|error| {
        format!("{ENV_NAME} must map key_id to supplier_id/public_key_hex records: {error}")
    })?;
    if keys.is_empty()
        || keys.iter().any(|(key_id, trusted)| {
            key_id.len() != 64
                || !key_id.bytes().all(|byte| byte.is_ascii_hexdigit())
                || trusted.supplier_id.trim().is_empty()
                || trusted.public_key_hex.len() != 64
                || !trusted
                    .public_key_hex
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Err(format!(
            "{ENV_NAME} must contain SHA-256 key ids mapped to supplier_id and 32-byte public_key_hex records"
        ));
    }
    Ok(keys)
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
        return Ok((symbol == instrument.symbol() && exchange_matches).then_some(document));
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
        let spec = qx_app::market_spec_from_value(instrument, market)
            .map_err(|error| error.message().to_string())?;
        validate_live_instrument_role(worker, instrument, market, &spec)?;
        return Ok(());
    }
    let symbol_matches = market
        .get("symbol")
        .and_then(Value::as_str)
        .is_some_and(|symbol| symbol == instrument.symbol());
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
    let spec = qx_app::market_spec_from_value(instrument, market)
        .map_err(|error| error.message().to_string())?;
    validate_live_instrument_role(worker, instrument, market, &spec)?;
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
        if live_execution_requires_role(worker) {
            failures.push(format!(
                "worker {} live execution requires instrument_spec_path with dataset_instrument_role evidence",
                worker.id
            ));
        }
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
    if live_execution_requires_role(worker) && worker.symbols.is_empty() {
        // Shared market-spec sets validate every keyed role above. A single-market spec
        // without an explicit symbol list cannot prove which live instruments it covers.
        let document = std::fs::read_to_string(&resolved)
            .ok()
            .and_then(|payload| serde_json::from_str::<Value>(&payload).ok());
        if document.as_ref().is_some_and(|value| {
            value.get("kind").and_then(Value::as_str) != Some(WORKER_MARKET_SPEC_SET_KIND)
        }) {
            failures.push(format!(
                "worker {} live execution must list symbols when using a single-market spec so role evidence can be bound",
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
        const SEED: [u8; 32] = [0x5a; 32];
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is after the Unix epoch")
            .as_millis() as u64;
        let role = qx_data::DatasetInstrumentRole::Future;
        let public_key_hex = qx_data::DatasetInstrumentRoleCatalogV1::ed25519_public_key_hex(&SEED)
            .expect("test role signer key is valid");
        let mut roles = BTreeMap::new();
        roles.insert(instrument.to_string(), role);
        let catalog =
            qx_data::DatasetInstrumentRoleCatalogV1::new_unsigned_dataset_role_catalog_v1(
                format!("qx-service-test-{}", instrument),
                "qx-service-test-supplier".into(),
                "fixture-v1".into(),
                now_ms,
                now_ms.saturating_sub(1_000).max(1),
                now_ms.saturating_add(86_400_000),
                roles,
            )
            .sign_with_ed25519_seed(&SEED)
            .expect("test role catalog signs");
        let trusted = BTreeMap::from([(
            catalog.key_id.clone(),
            qx_data::DatasetRoleTrustedKeyV1 {
                supplier_id: "qx-service-test-supplier".into(),
                public_key_hex,
            },
        )]);
        std::env::set_var(
            "QX_DATASET_ROLE_TRUSTED_PUBLIC_KEYS",
            serde_json::to_string(&trusted).expect("serialize test trust root"),
        );
        serde_json::json!({
            "instrument": instrument,
            "product": "perpetual",
            "dataset_instrument_role": "future",
            "dataset_instrument_role_catalog": catalog,
            "base_currency": instrument.symbol().split('/').next().unwrap(),
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

    #[test]
    fn rejected_venue_report_keeps_its_client_order_identity() {
        let event = VenueEvent::Rejected {
            client_order_id: 9_001,
            reason: "exchange rejected order".into(),
            ts: 123,
        };
        assert_eq!(report_client_order_id(&event), 9_001);
    }
}
