//! Account-scoped composition registry for independently hosted venue ports.
//!
//! The registry describes which configured workers implement each narrow port for a
//! `(venue, account)` session. It does not merge their implementations into a large trait,
//! and a configured worker is not evidence that its connection is healthy or vendor-verified.

use qx_core::{InstrumentId, TradingInstrumentSpec, VenueFamily, VenueId};
use qx_runtime::{
    account_event_log_name, resolve_runtime_relative_path, RuntimeConfig, WorkerConfig, WorkerRole,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const EXCHANGE_GATEWAY_SCHEMA_VERSION: u32 = 1;
pub const INSTRUMENT_RULES_SCHEMA_VERSION: u32 = 1;
pub const CAPABILITY_PROFILE_SCHEMA_VERSION: u32 = 1;

/// Service ports are routed independently. `AccountQuery` workers are the configured execution
/// and reconciliation workers which perform private reads under their own exact preflight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeGatewayPort {
    MarketData,
    InstrumentCatalog,
    AccountQuery,
    VenueExecution,
    PrivateOrderStream,
    Reconciliation,
    SpreadRecovery,
}

impl ExchangeGatewayPort {
    pub(crate) fn metric_label(self) -> &'static str {
        match self {
            Self::MarketData => "market_data",
            Self::InstrumentCatalog => "instrument_catalog",
            Self::AccountQuery => "account_query",
            Self::VenueExecution => "venue_execution",
            Self::PrivateOrderStream => "private_order_stream",
            Self::Reconciliation => "reconciliation",
            Self::SpreadRecovery => "spread_recovery",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GatewayBusinessHealthStatus {
    Unobserved,
    Healthy,
    Degraded,
    Unhealthy,
}

impl GatewayBusinessHealthStatus {
    fn from_metric_value(value: u8) -> Self {
        match value {
            2 => Self::Healthy,
            1 => Self::Degraded,
            0 => Self::Unhealthy,
            _ => Self::Unobserved,
        }
    }

    pub(crate) fn metric_value(self) -> Option<u8> {
        match self {
            Self::Healthy => Some(2),
            Self::Degraded => Some(1),
            Self::Unhealthy => Some(0),
            Self::Unobserved => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GatewayWorkerHealthStatus {
    Healthy,
    Unhealthy,
    Unobserved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GatewayPortHealthStatus {
    Unconfigured,
    Unobserved,
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GatewayWorkerHealthV1 {
    pub worker_id: String,
    pub status: GatewayWorkerHealthStatus,
    pub business_status: GatewayBusinessHealthStatus,
    pub heartbeat_timestamp_seconds: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GatewayPortHealthV1 {
    pub port: ExchangeGatewayPort,
    pub status: GatewayPortHealthStatus,
    pub business_status: GatewayBusinessHealthStatus,
    pub workers: Vec<GatewayWorkerHealthV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExchangeGatewayHealthV1 {
    pub schema_version: u32,
    pub venue_id: VenueId,
    pub account_id: String,
    pub account_event_log: String,
    /// Identity only: this profile does not grant any execution or account-query permission.
    pub capability_profile: CapabilityProfileV1,
    pub ports: Vec<GatewayPortHealthV1>,
}

const EXCHANGE_GATEWAY_PORTS: [ExchangeGatewayPort; 7] = [
    ExchangeGatewayPort::MarketData,
    ExchangeGatewayPort::InstrumentCatalog,
    ExchangeGatewayPort::AccountQuery,
    ExchangeGatewayPort::VenueExecution,
    ExchangeGatewayPort::PrivateOrderStream,
    ExchangeGatewayPort::Reconciliation,
    ExchangeGatewayPort::SpreadRecovery,
];

/// A versioned summary binding a session to its generated capability catalog entry.
///
/// Scope counts and the catalog digest are diagnostic metadata. They do not grant permission:
/// every submit, cancel, and private account read must still pass the operation-specific,
/// product/account/order-shaped preflight in `execution_capability_gate`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityProfileV1 {
    pub schema_version: u32,
    pub capability_id: String,
    pub catalog_fingerprint: String,
    pub execution_scope_count: usize,
    pub account_query_scope_count: usize,
}

impl CapabilityProfileV1 {
    pub fn from_catalog(catalog: &Value, capability_id: &str) -> Result<Self, String> {
        if catalog.get("schema_version").and_then(Value::as_u64) != Some(1) {
            return Err("能力目录 schema_version 不受支持".into());
        }
        let capabilities = catalog
            .get("capabilities")
            .and_then(Value::as_array)
            .ok_or_else(|| "能力目录缺少 capabilities 数组".to_string())?;
        let capability = capabilities
            .iter()
            .find(|entry| entry.get("id").and_then(Value::as_str) == Some(capability_id))
            .ok_or_else(|| format!("能力目录缺少 capability_id={capability_id}"))?;
        let count_scopes = |field: &str| -> Result<usize, String> {
            match capability.get(field) {
                Some(Value::Array(scopes)) => Ok(scopes.len()),
                Some(Value::Null) | None => Ok(0),
                _ => Err(format!("capability {capability_id} 的 {field} 必须是数组")),
            }
        };
        let catalog_fingerprint = qx_strategy::sha256_hex(
            &serde_json::to_vec(catalog).map_err(|error| format!("能力目录序列化失败: {error}"))?,
        );
        Ok(Self {
            schema_version: CAPABILITY_PROFILE_SCHEMA_VERSION,
            capability_id: capability_id.into(),
            catalog_fingerprint,
            execution_scope_count: count_scopes("execution_scopes")?,
            account_query_scope_count: count_scopes("account_query_scopes")?,
        })
    }
}

/// Frozen, auditable wrapper around the existing canonical market-rule model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentRulesV1 {
    pub schema_version: u32,
    pub instrument: InstrumentId,
    pub spec: TradingInstrumentSpec,
    pub source_worker_id: String,
    pub source_path: String,
    pub fingerprint: String,
}

impl InstrumentRulesV1 {
    pub fn from_spec(
        worker: &WorkerConfig,
        instrument: &InstrumentId,
        spec: TradingInstrumentSpec,
        source_path: impl Into<PathBuf>,
    ) -> Result<Self, String> {
        let venue = worker
            .venue_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("worker {} 缺少 venue_id", worker.id))?;
        if worker.id.trim().is_empty()
            || worker
                .account_id
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            || !spec.instrument.eq(instrument)
            || !instrument.venue.as_str().eq_ignore_ascii_case(venue)
        {
            return Err("InstrumentRulesV1 的 worker/account/venue/instrument 身份不一致".into());
        }
        spec.validate()
            .map_err(|error| format!("InstrumentRulesV1 市场规则无效: {error}"))?;
        let fingerprint = spec_fingerprint(&spec)?;
        let rules = Self {
            schema_version: INSTRUMENT_RULES_SCHEMA_VERSION,
            instrument: instrument.clone(),
            spec,
            source_worker_id: worker.id.clone(),
            source_path: source_path.into().to_string_lossy().into_owned(),
            fingerprint,
        };
        rules.validate()?;
        Ok(rules)
    }

    /// Load the exact frozen rules already selected by the worker market-spec resolver.
    pub fn load_for_worker(
        worker: &WorkerConfig,
        instrument: &InstrumentId,
        runtime_config_path: &Path,
    ) -> Result<Self, String> {
        let spec_path = worker
            .instrument_spec_path
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                format!(
                    "FAIL_CLOSED: worker {} 未配置 InstrumentRulesV1 冻结规则文件",
                    worker.id
                )
            })?;
        let spec = crate::worker_market_spec::load_worker_market_spec_for_instrument(
            worker,
            instrument,
            Some(runtime_config_path),
        )?
        .ok_or_else(|| format!("worker {} 未绑定标的 {instrument} 的冻结规则", worker.id))?;
        let source_path = resolve_runtime_relative_path(runtime_config_path, spec_path);
        Self::from_spec(worker, instrument, spec, source_path)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != INSTRUMENT_RULES_SCHEMA_VERSION
            || self.source_worker_id.trim().is_empty()
            || self.source_path.trim().is_empty()
            || self.instrument != self.spec.instrument
        {
            return Err("InstrumentRulesV1 身份或 schema_version 非法".into());
        }
        self.spec
            .validate()
            .map_err(|error| format!("InstrumentRulesV1 市场规则无效: {error}"))?;
        if self.fingerprint != spec_fingerprint(&self.spec)? {
            return Err("InstrumentRulesV1 fingerprint 与冻结规则不一致".into());
        }
        Ok(())
    }
}

fn spec_fingerprint(spec: &TradingInstrumentSpec) -> Result<String, String> {
    let bytes = serde_json::to_vec(spec).map_err(|error| format!("市场规则序列化失败: {error}"))?;
    Ok(qx_strategy::sha256_hex(&bytes))
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GatewaySessionKey {
    venue_id: VenueId,
    account_id: String,
}

/// One exchange account's independently routable worker set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeGatewayV1 {
    pub schema_version: u32,
    pub venue_id: VenueId,
    pub account_id: String,
    /// Canonical EventLog identity for the account ledger read model.
    pub account_event_log: String,
    pub capability_profile: CapabilityProfileV1,
    /// Public market-data workers are venue-scoped and shared across accounts at that venue.
    pub market_data_worker_ids: Vec<String>,
    pub instrument_catalog_worker_ids: Vec<String>,
    pub account_query_worker_ids: Vec<String>,
    pub execution_worker_ids: Vec<String>,
    pub private_order_stream_worker_ids: Vec<String>,
    pub reconciliation_worker_ids: Vec<String>,
    pub spread_recovery_worker_ids: Vec<String>,
}

impl ExchangeGatewayV1 {
    pub fn worker_ids(&self, port: ExchangeGatewayPort) -> &[String] {
        match port {
            ExchangeGatewayPort::MarketData => &self.market_data_worker_ids,
            ExchangeGatewayPort::InstrumentCatalog => &self.instrument_catalog_worker_ids,
            ExchangeGatewayPort::AccountQuery => &self.account_query_worker_ids,
            ExchangeGatewayPort::VenueExecution => &self.execution_worker_ids,
            ExchangeGatewayPort::PrivateOrderStream => &self.private_order_stream_worker_ids,
            ExchangeGatewayPort::Reconciliation => &self.reconciliation_worker_ids,
            ExchangeGatewayPort::SpreadRecovery => &self.spread_recovery_worker_ids,
        }
    }

    pub fn require_port(&self, port: ExchangeGatewayPort) -> Result<(), String> {
        if self.worker_ids(port).is_empty() {
            return Err(format!(
                "EXCHANGE_GATEWAY_PORT_UNBOUND venue={} account={} port={port:?}",
                self.venue_id, self.account_id
            ));
        }
        Ok(())
    }
}

/// Registry of venue/account gateway compositions built from the configured worker topology.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExchangeGatewayRegistry {
    gateways: BTreeMap<GatewaySessionKey, ExchangeGatewayV1>,
    worker_bindings: BTreeMap<String, Vec<(VenueId, Option<String>, ExchangeGatewayPort)>>,
}

impl ExchangeGatewayRegistry {
    pub fn from_runtime_config(config: &RuntimeConfig) -> Result<Self, String> {
        let catalog = qx_api::product_capability_catalog_document()?;
        Self::from_workers(&config.workers, &catalog)
    }

    pub fn from_workers(workers: &[WorkerConfig], catalog: &Value) -> Result<Self, String> {
        let mut registry = Self::default();
        let mut seen_ids = BTreeSet::new();
        let mut market_data_by_venue: BTreeMap<VenueId, Vec<String>> = BTreeMap::new();

        for worker in workers.iter().filter(|worker| worker.enabled) {
            if !matches!(
                worker.role,
                WorkerRole::MarketData
                    | WorkerRole::Execution
                    | WorkerRole::UserStream
                    | WorkerRole::Reconciler
                    | WorkerRole::SpreadRecovery
            ) {
                continue;
            }
            if worker.id.trim().is_empty() || !seen_ids.insert(worker.id.as_str()) {
                return Err(format!(
                    "ExchangeGateway worker id 为空或重复: {}",
                    worker.id
                ));
            }
            let venue_name = worker
                .venue_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("worker {} 缺少 ExchangeGateway venue_id", worker.id))?;
            let venue_id = VenueId::new(venue_name);

            if worker.role == WorkerRole::MarketData {
                market_data_by_venue
                    .entry(venue_id.clone())
                    .or_default()
                    .push(worker.id.clone());
                registry
                    .worker_bindings
                    .entry(worker.id.clone())
                    .or_default()
                    .push((venue_id, None, ExchangeGatewayPort::MarketData));
                continue;
            }

            let account_id = worker
                .account_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("worker {} 缺少 ExchangeGateway account_id", worker.id))?
                .to_owned();
            let log_name = account_event_log_name(&account_id, venue_name)
                .ok_or_else(|| format!("worker {} 无法构造账户 EventLog 身份", worker.id))?;
            let capability_id = capability_id_for_venue(&venue_id);
            let capability_profile = CapabilityProfileV1::from_catalog(catalog, capability_id)?;
            let key = GatewaySessionKey {
                venue_id: venue_id.clone(),
                account_id: account_id.clone(),
            };
            let gateway = registry
                .gateways
                .entry(key)
                .or_insert_with(|| ExchangeGatewayV1 {
                    schema_version: EXCHANGE_GATEWAY_SCHEMA_VERSION,
                    venue_id: venue_id.clone(),
                    account_id: account_id.clone(),
                    account_event_log: log_name.clone(),
                    capability_profile: capability_profile.clone(),
                    market_data_worker_ids: Vec::new(),
                    instrument_catalog_worker_ids: Vec::new(),
                    account_query_worker_ids: Vec::new(),
                    execution_worker_ids: Vec::new(),
                    private_order_stream_worker_ids: Vec::new(),
                    reconciliation_worker_ids: Vec::new(),
                    spread_recovery_worker_ids: Vec::new(),
                });
            if gateway.account_event_log != log_name
                || gateway.capability_profile != capability_profile
            {
                return Err(format!(
                    "ExchangeGateway 注册身份冲突 venue={} account={}",
                    venue_id, account_id
                ));
            }
            let port = match worker.role {
                WorkerRole::Execution => {
                    gateway.execution_worker_ids.push(worker.id.clone());
                    gateway.account_query_worker_ids.push(worker.id.clone());
                    if worker.instrument_spec_path.is_some() {
                        gateway
                            .instrument_catalog_worker_ids
                            .push(worker.id.clone());
                    }
                    ExchangeGatewayPort::VenueExecution
                }
                WorkerRole::UserStream => {
                    gateway
                        .private_order_stream_worker_ids
                        .push(worker.id.clone());
                    ExchangeGatewayPort::PrivateOrderStream
                }
                WorkerRole::Reconciler => {
                    gateway.reconciliation_worker_ids.push(worker.id.clone());
                    gateway.account_query_worker_ids.push(worker.id.clone());
                    if worker.instrument_spec_path.is_some() {
                        gateway
                            .instrument_catalog_worker_ids
                            .push(worker.id.clone());
                    }
                    ExchangeGatewayPort::Reconciliation
                }
                WorkerRole::SpreadRecovery => {
                    gateway.spread_recovery_worker_ids.push(worker.id.clone());
                    if worker.instrument_spec_path.is_some() {
                        gateway
                            .instrument_catalog_worker_ids
                            .push(worker.id.clone());
                    }
                    ExchangeGatewayPort::SpreadRecovery
                }
                _ => unreachable!("venue role set checked above"),
            };
            registry
                .worker_bindings
                .entry(worker.id.clone())
                .or_default()
                .push((venue_id, Some(account_id), port));
        }

        for gateway in registry.gateways.values_mut() {
            gateway.market_data_worker_ids = market_data_by_venue
                .get(&gateway.venue_id)
                .cloned()
                .unwrap_or_default();
            normalize_ids(&mut gateway.market_data_worker_ids);
            normalize_ids(&mut gateway.instrument_catalog_worker_ids);
            normalize_ids(&mut gateway.account_query_worker_ids);
            normalize_ids(&mut gateway.execution_worker_ids);
            normalize_ids(&mut gateway.private_order_stream_worker_ids);
            normalize_ids(&mut gateway.reconciliation_worker_ids);
            normalize_ids(&mut gateway.spread_recovery_worker_ids);
        }
        for bindings in registry.worker_bindings.values_mut() {
            bindings.sort();
            bindings.dedup();
        }
        Ok(registry)
    }

    pub fn gateway(&self, venue_id: &str, account_id: &str) -> Option<&ExchangeGatewayV1> {
        let key = GatewaySessionKey {
            venue_id: VenueId::new(venue_id.trim()),
            account_id: account_id.trim().to_owned(),
        };
        self.gateways.get(&key)
    }

    pub fn gateways(&self) -> impl Iterator<Item = &ExchangeGatewayV1> {
        self.gateways.values()
    }

    pub fn worker_bindings(
        &self,
        worker_id: &str,
    ) -> Option<&[(VenueId, Option<String>, ExchangeGatewayPort)]> {
        self.worker_bindings.get(worker_id).map(Vec::as_slice)
    }

    /// Build an account/venue health view from the same worker metrics served by `/metrics`.
    /// Missing metrics remain `unobserved`; configuration alone never implies health.
    pub(crate) fn health_snapshots(
        &self,
        metrics: &str,
        now_ms: u64,
        stale_after_ms: u64,
    ) -> Vec<ExchangeGatewayHealthV1> {
        let worker_ids = self
            .gateways
            .values()
            .flat_map(|gateway| {
                EXCHANGE_GATEWAY_PORTS
                    .iter()
                    .flat_map(|port| gateway.worker_ids(*port))
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        let worker_facts = parse_worker_metrics(metrics, &worker_ids);
        self.gateways
            .values()
            .map(|gateway| {
                let ports = EXCHANGE_GATEWAY_PORTS
                    .iter()
                    .map(|port| {
                        let workers = gateway
                            .worker_ids(*port)
                            .iter()
                            .map(|worker_id| {
                                let fact = worker_facts.get(worker_id);
                                let heartbeat_timestamp_seconds =
                                    fact.and_then(|fact| fact.heartbeat_timestamp_seconds);
                                let fresh = heartbeat_timestamp_seconds.is_some_and(|seconds| {
                                    metric_timestamp_is_fresh(seconds, now_ms, stale_after_ms)
                                });
                                let status = match fact.and_then(|fact| fact.up) {
                                    Some(true) if fresh => GatewayWorkerHealthStatus::Healthy,
                                    Some(_) => GatewayWorkerHealthStatus::Unhealthy,
                                    None => GatewayWorkerHealthStatus::Unobserved,
                                };
                                let business_status = fact
                                    .and_then(|fact| fact.business.get(port))
                                    .map(|probe| {
                                        let fresh =
                                            probe.timestamp_seconds.is_some_and(|seconds| {
                                                metric_timestamp_is_fresh(
                                                    seconds,
                                                    now_ms,
                                                    stale_after_ms,
                                                )
                                            });
                                        match (probe.status, fresh) {
                                            (Some(value), true) => {
                                                GatewayBusinessHealthStatus::from_metric_value(
                                                    value,
                                                )
                                            }
                                            _ => GatewayBusinessHealthStatus::Unobserved,
                                        }
                                    })
                                    .unwrap_or(GatewayBusinessHealthStatus::Unobserved);
                                GatewayWorkerHealthV1 {
                                    worker_id: worker_id.clone(),
                                    status,
                                    business_status,
                                    heartbeat_timestamp_seconds,
                                }
                            })
                            .collect::<Vec<_>>();
                        GatewayPortHealthV1 {
                            port: *port,
                            status: aggregate_port_health(&workers),
                            business_status: aggregate_business_health(&workers),
                            workers,
                        }
                    })
                    .collect();
                ExchangeGatewayHealthV1 {
                    schema_version: EXCHANGE_GATEWAY_SCHEMA_VERSION,
                    venue_id: gateway.venue_id.clone(),
                    account_id: gateway.account_id.clone(),
                    account_event_log: gateway.account_event_log.clone(),
                    capability_profile: gateway.capability_profile.clone(),
                    ports,
                }
            })
            .collect()
    }

    pub fn require_worker_binding(&self, worker_id: &str) -> Result<(), String> {
        let bindings = self
            .worker_bindings(worker_id)
            .filter(|bindings| !bindings.is_empty())
            .ok_or_else(|| format!("EXCHANGE_GATEWAY_WORKER_UNBOUND worker={worker_id}"))?;
        for (venue_id, account_id, port) in bindings {
            let Some(account_id) = account_id.as_deref() else {
                if *port != ExchangeGatewayPort::MarketData {
                    return Err(format!(
                        "EXCHANGE_GATEWAY_ACCOUNT_REQUIRED worker={worker_id} port={port:?}"
                    ));
                }
                continue;
            };
            let gateway = self
                .gateway(venue_id.as_str(), account_id)
                .ok_or_else(|| {
                    format!(
                        "EXCHANGE_GATEWAY_SESSION_UNBOUND worker={worker_id} venue={venue_id} account={account_id}"
                    )
                })?;
            gateway.require_port(*port)?;
            if !gateway
                .worker_ids(*port)
                .iter()
                .any(|bound_worker_id| bound_worker_id == worker_id)
            {
                return Err(format!(
                    "EXCHANGE_GATEWAY_PORT_WORKER_UNBOUND worker={worker_id} venue={venue_id} account={account_id} port={port:?}"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct WorkerMetricFact {
    up: Option<bool>,
    heartbeat_timestamp_seconds: Option<u64>,
    business: BTreeMap<ExchangeGatewayPort, WorkerBusinessMetricFact>,
}

#[derive(Clone, Copy, Debug, Default)]
struct WorkerBusinessMetricFact {
    status: Option<u8>,
    timestamp_seconds: Option<u64>,
}

fn parse_worker_metrics(
    metrics: &str,
    expected_workers: &BTreeSet<String>,
) -> BTreeMap<String, WorkerMetricFact> {
    let mut result = BTreeMap::<String, WorkerMetricFact>::new();
    for line in metrics.lines() {
        let metric = if line.starts_with("qx_worker_up{") {
            "up"
        } else if line.starts_with("qx_worker_heartbeat_timestamp_seconds{") {
            "heartbeat"
        } else if line.starts_with("qx_gateway_port_business_status{") {
            "business_status"
        } else if line.starts_with("qx_gateway_port_business_timestamp_seconds{") {
            "business_timestamp"
        } else {
            continue;
        };
        let Some(worker_id) = expected_workers.iter().find(|worker_id| {
            line.contains(&format!(
                "worker=\"{}\"",
                crate::worker_metrics::prometheus_label(worker_id)
            ))
        }) else {
            continue;
        };
        let value = line
            .split_whitespace()
            .last()
            .and_then(|raw| raw.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value >= 0.0);
        let fact = result.entry(worker_id.clone()).or_default();
        match metric {
            "up" => fact.up = value.map(|value| value == 1.0),
            "heartbeat" => fact.heartbeat_timestamp_seconds = value.map(|value| value as u64),
            "business_status" | "business_timestamp" => {
                let Some(port) = EXCHANGE_GATEWAY_PORTS
                    .iter()
                    .copied()
                    .find(|port| line.contains(&format!("port=\"{}\"", port.metric_label())))
                else {
                    continue;
                };
                let probe = fact.business.entry(port).or_default();
                match metric {
                    "business_status" => probe.status = value.and_then(metric_status_code),
                    "business_timestamp" => {
                        probe.timestamp_seconds = value.map(|value| value as u64)
                    }
                    _ => unreachable!("business metric kind is selected from known prefixes"),
                }
            }
            _ => unreachable!("metric kind is selected from known worker metric prefixes"),
        }
    }
    result
}

fn aggregate_port_health(workers: &[GatewayWorkerHealthV1]) -> GatewayPortHealthStatus {
    if workers.is_empty() {
        return GatewayPortHealthStatus::Unconfigured;
    }
    if workers
        .iter()
        .all(|worker| worker.status == GatewayWorkerHealthStatus::Unobserved)
    {
        return GatewayPortHealthStatus::Unobserved;
    }
    if workers
        .iter()
        .all(|worker| worker.status == GatewayWorkerHealthStatus::Healthy)
    {
        return GatewayPortHealthStatus::Healthy;
    }
    if workers
        .iter()
        .all(|worker| worker.status == GatewayWorkerHealthStatus::Unhealthy)
    {
        return GatewayPortHealthStatus::Unhealthy;
    }
    GatewayPortHealthStatus::Degraded
}

fn aggregate_business_health(workers: &[GatewayWorkerHealthV1]) -> GatewayBusinessHealthStatus {
    if workers.is_empty()
        || workers
            .iter()
            .all(|worker| worker.business_status == GatewayBusinessHealthStatus::Unobserved)
    {
        return GatewayBusinessHealthStatus::Unobserved;
    }
    if workers
        .iter()
        .all(|worker| worker.business_status == GatewayBusinessHealthStatus::Healthy)
    {
        return GatewayBusinessHealthStatus::Healthy;
    }
    if workers
        .iter()
        .all(|worker| worker.business_status == GatewayBusinessHealthStatus::Unhealthy)
    {
        return GatewayBusinessHealthStatus::Unhealthy;
    }
    GatewayBusinessHealthStatus::Degraded
}

fn metric_timestamp_is_fresh(timestamp_seconds: u64, now_ms: u64, stale_after_ms: u64) -> bool {
    let Some(timestamp_ms) = timestamp_seconds.checked_mul(1_000) else {
        return false;
    };
    now_ms
        .checked_sub(timestamp_ms)
        .is_some_and(|age_ms| age_ms <= stale_after_ms)
}

fn metric_status_code(value: f64) -> Option<u8> {
    match value {
        0.0 => Some(0),
        1.0 => Some(1),
        2.0 => Some(2),
        _ => None,
    }
}

/// Add the account-scoped gateway health projection to the existing operational readiness layer.
/// The projection is diagnostic and never upgrades execution capability evidence.
pub(crate) fn attach_gateway_health_to_readiness(
    readiness: &mut qx_api::ApiReadiness,
    config: &RuntimeConfig,
    metrics: &str,
    now_ms: u64,
    stale_after_ms: u64,
) {
    let gateway_health = match ExchangeGatewayRegistry::from_runtime_config(config) {
        Ok(registry) => serde_json::json!({
            "schema_version": EXCHANGE_GATEWAY_SCHEMA_VERSION,
            "source": "worker_metrics",
            "gateways": registry.health_snapshots(metrics, now_ms, stale_after_ms),
        }),
        Err(error) => serde_json::json!({
            "schema_version": EXCHANGE_GATEWAY_SCHEMA_VERSION,
            "status": "unavailable",
            "error_code": "EXCHANGE_GATEWAY_REGISTRY_INVALID",
            "detail": error,
        }),
    };
    let layer = readiness
        .readiness_layers
        .entry("O".into())
        .or_insert_with(|| serde_json::json!({}));
    if !layer.is_object() {
        *layer = serde_json::json!({});
    }
    if let Some(fields) = layer.as_object_mut() {
        fields.insert("exchange_gateways".into(), gateway_health);
    }
}

fn capability_id_for_venue(venue_id: &VenueId) -> &'static str {
    match VenueFamily::parse(venue_id.as_str()) {
        VenueFamily::Paper => "qx.venue.paper",
        VenueFamily::Binance => "qx.venue.binance_spot",
        VenueFamily::Other => "qx.venue.ccxt_process",
    }
}

fn normalize_ids(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(
        id: &str,
        role: WorkerRole,
        venue_id: &str,
        account_id: Option<&str>,
        instrument_spec_path: Option<&str>,
    ) -> WorkerConfig {
        WorkerConfig {
            id: id.into(),
            role,
            enabled: true,
            account_id: account_id.map(str::to_owned),
            venue_id: Some(venue_id.into()),
            endpoint: None,
            symbols: Vec::new(),
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: instrument_spec_path.map(str::to_owned),
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        }
    }

    fn catalog() -> Value {
        serde_json::json!({
            "schema_version": 1,
            "capabilities": [
                {
                    "id": "qx.venue.ccxt_process",
                    "execution_scopes": [{"operations": ["submit", "cancel"]}],
                    "account_query_scopes": [{"operations": ["balance.read"]}]
                },
                {
                    "id": "qx.venue.binance_spot",
                    "execution_scopes": [{"operations": ["submit"]}],
                    "account_query_scopes": []
                },
                {
                    "id": "qx.venue.paper",
                    "execution_scopes": [],
                    "account_query_scopes": []
                }
            ]
        })
    }

    #[test]
    fn registry_keeps_same_venue_accounts_and_cross_venue_accounts_separate() {
        let workers = vec![
            worker("okx-market", WorkerRole::MarketData, "OKX", None, None),
            worker(
                "okx-a-exec",
                WorkerRole::Execution,
                "OKX",
                Some("alpha"),
                Some("a.json"),
            ),
            worker(
                "okx-a-recon",
                WorkerRole::Reconciler,
                "OKX",
                Some("alpha"),
                None,
            ),
            worker(
                "okx-b-exec",
                WorkerRole::Execution,
                "OKX",
                Some("beta"),
                Some("b.json"),
            ),
            worker(
                "binance-a-exec",
                WorkerRole::Execution,
                "BINANCE",
                Some("alpha"),
                Some("c.json"),
            ),
        ];
        let registry = ExchangeGatewayRegistry::from_workers(&workers, &catalog()).unwrap();

        let okx_alpha = registry.gateway("okx", "alpha").unwrap();
        assert_eq!(okx_alpha.account_event_log, "ccxt-alpha-okx-events");
        assert_eq!(okx_alpha.market_data_worker_ids, ["okx-market".to_owned()]);
        assert_eq!(okx_alpha.execution_worker_ids, ["okx-a-exec".to_owned()]);
        assert_eq!(
            okx_alpha.account_query_worker_ids,
            ["okx-a-exec".to_owned(), "okx-a-recon".to_owned()]
        );
        assert_eq!(okx_alpha.capability_profile.execution_scope_count, 1);
        assert_eq!(
            registry
                .gateway("OKX", "beta")
                .unwrap()
                .execution_worker_ids,
            ["okx-b-exec".to_owned()]
        );
        assert_eq!(
            registry
                .gateway("binance", "alpha")
                .unwrap()
                .execution_worker_ids,
            ["binance-a-exec".to_owned()]
        );
        assert_ne!(
            okx_alpha.account_event_log,
            registry.gateway("OKX", "beta").unwrap().account_event_log
        );
    }

    #[test]
    fn registry_rejects_missing_private_identity_and_missing_capability_profile() {
        let missing_account = worker("bad-exec", WorkerRole::Execution, "OKX", None, None);
        assert!(
            ExchangeGatewayRegistry::from_workers(&[missing_account], &catalog())
                .unwrap_err()
                .contains("account_id")
        );

        let no_capabilities = serde_json::json!({"schema_version": 1, "capabilities": []});
        let worker_without_profile =
            worker("okx-exec", WorkerRole::Execution, "OKX", Some("a"), None);
        assert!(
            ExchangeGatewayRegistry::from_workers(&[worker_without_profile], &no_capabilities)
                .unwrap_err()
                .contains("capability_id")
        );
    }

    #[test]
    fn gateway_health_uses_fresh_worker_metrics_and_keeps_missing_ports_unobserved() {
        let workers = vec![
            worker("okx-market", WorkerRole::MarketData, "OKX", None, None),
            worker(
                "okx-exec",
                WorkerRole::Execution,
                "OKX",
                Some("alpha"),
                Some("rules.json"),
            ),
            worker(
                "okx-user-stream",
                WorkerRole::UserStream,
                "OKX",
                Some("alpha"),
                None,
            ),
            worker(
                "okx-reconciler",
                WorkerRole::Reconciler,
                "OKX",
                Some("alpha"),
                None,
            ),
        ];
        let registry = ExchangeGatewayRegistry::from_workers(&workers, &catalog()).unwrap();
        let metrics = concat!(
            "qx_worker_up{worker=\"okx-market\",account=\"\"} 1\n",
            "qx_worker_heartbeat_timestamp_seconds{worker=\"okx-market\",account=\"\"} 90\n",
            "qx_gateway_port_business_status{worker=\"okx-market\",account=\"\",port=\"market_data\"} 2\n",
            "qx_gateway_port_business_timestamp_seconds{worker=\"okx-market\",account=\"\",port=\"market_data\"} 90\n",
            "qx_worker_up{worker=\"okx-exec\",account=\"alpha\"} 1\n",
            "qx_worker_heartbeat_timestamp_seconds{worker=\"okx-exec\",account=\"alpha\"} 90\n",
            "qx_gateway_port_business_status{worker=\"okx-exec\",account=\"alpha\",port=\"venue_execution\"} 1\n",
            "qx_gateway_port_business_timestamp_seconds{worker=\"okx-exec\",account=\"alpha\",port=\"venue_execution\"} 90\n",
            "qx_worker_up{worker=\"okx-user-stream\",account=\"alpha\"} 0\n",
            "qx_worker_heartbeat_timestamp_seconds{worker=\"okx-user-stream\",account=\"alpha\"} 90\n",
            "qx_gateway_port_business_status{worker=\"okx-user-stream\",account=\"alpha\",port=\"private_order_stream\"} 0\n",
            "qx_gateway_port_business_timestamp_seconds{worker=\"okx-user-stream\",account=\"alpha\",port=\"private_order_stream\"} 90\n",
            "qx_worker_up{worker=\"okx-reconciler\",account=\"alpha\"} 1\n",
            "qx_worker_heartbeat_timestamp_seconds{worker=\"okx-reconciler\",account=\"alpha\"} 90\n",
            "qx_gateway_port_business_status{worker=\"okx-reconciler\",account=\"alpha\",port=\"account_query\"} 2\n",
            "qx_gateway_port_business_timestamp_seconds{worker=\"okx-reconciler\",account=\"alpha\",port=\"account_query\"} 90\n",
            "qx_gateway_port_business_status{worker=\"okx-reconciler\",account=\"alpha\",port=\"reconciliation\"} 1\n",
            "qx_gateway_port_business_timestamp_seconds{worker=\"okx-reconciler\",account=\"alpha\",port=\"reconciliation\"} 90\n"
        );

        let snapshots = registry.health_snapshots(metrics, 100_000, 20_000);
        let snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.account_id == "alpha")
            .unwrap();
        let status = |port| {
            snapshot
                .ports
                .iter()
                .find(|health| health.port == port)
                .unwrap()
                .status
        };
        let business_status = |port| {
            snapshot
                .ports
                .iter()
                .find(|health| health.port == port)
                .unwrap()
                .business_status
        };
        assert_eq!(
            status(ExchangeGatewayPort::MarketData),
            GatewayPortHealthStatus::Healthy
        );
        assert_eq!(
            status(ExchangeGatewayPort::VenueExecution),
            GatewayPortHealthStatus::Healthy
        );
        assert_eq!(
            status(ExchangeGatewayPort::InstrumentCatalog),
            GatewayPortHealthStatus::Healthy
        );
        assert_eq!(
            status(ExchangeGatewayPort::PrivateOrderStream),
            GatewayPortHealthStatus::Unhealthy
        );
        assert_eq!(
            status(ExchangeGatewayPort::Reconciliation),
            GatewayPortHealthStatus::Healthy
        );
        assert_eq!(
            status(ExchangeGatewayPort::AccountQuery),
            GatewayPortHealthStatus::Healthy
        );
        assert_eq!(
            status(ExchangeGatewayPort::SpreadRecovery),
            GatewayPortHealthStatus::Unconfigured
        );
        assert_eq!(
            business_status(ExchangeGatewayPort::MarketData),
            GatewayBusinessHealthStatus::Healthy
        );
        assert_eq!(
            business_status(ExchangeGatewayPort::VenueExecution),
            GatewayBusinessHealthStatus::Degraded
        );
        assert_eq!(
            business_status(ExchangeGatewayPort::PrivateOrderStream),
            GatewayBusinessHealthStatus::Unhealthy
        );
        assert_eq!(
            business_status(ExchangeGatewayPort::Reconciliation),
            GatewayBusinessHealthStatus::Degraded
        );
        assert_eq!(
            business_status(ExchangeGatewayPort::AccountQuery),
            GatewayBusinessHealthStatus::Degraded
        );

        let stale = registry.health_snapshots(metrics, 111_000, 20_000);
        let stale_execution = stale[0]
            .ports
            .iter()
            .find(|health| health.port == ExchangeGatewayPort::VenueExecution)
            .unwrap();
        assert_eq!(stale_execution.status, GatewayPortHealthStatus::Unhealthy);
        assert_eq!(
            stale_execution.business_status,
            GatewayBusinessHealthStatus::Unobserved
        );
    }

    #[test]
    fn future_worker_or_business_timestamps_do_not_become_fresh_health_evidence() {
        let workers = vec![
            worker(
                "binance-market",
                WorkerRole::MarketData,
                "BINANCE",
                None,
                None,
            ),
            worker(
                "binance-exec",
                WorkerRole::Execution,
                "BINANCE",
                Some("main"),
                None,
            ),
        ];
        let registry = ExchangeGatewayRegistry::from_workers(&workers, &catalog()).unwrap();
        let metrics = concat!(
            "qx_worker_up{worker=\"binance-market\",account=\"\"} 1\n",
            "qx_worker_heartbeat_timestamp_seconds{worker=\"binance-market\",account=\"\"} 101\n",
            "qx_gateway_port_business_status{worker=\"binance-market\",account=\"\",port=\"market_data\"} 2\n",
            "qx_gateway_port_business_timestamp_seconds{worker=\"binance-market\",account=\"\",port=\"market_data\"} 101\n"
        );
        let snapshot = &registry.health_snapshots(metrics, 100_000, 20_000)[0];
        let market_data = snapshot
            .ports
            .iter()
            .find(|health| health.port == ExchangeGatewayPort::MarketData)
            .unwrap();
        assert_eq!(market_data.status, GatewayPortHealthStatus::Unhealthy);
        assert_eq!(
            market_data.business_status,
            GatewayBusinessHealthStatus::Unobserved
        );
    }

    #[test]
    fn instrument_rules_snapshot_binds_identity_and_detects_mutation() {
        let instrument = InstrumentId::parse("PEPE/USDT.OKX").unwrap();
        let worker = worker(
            "okx-exec",
            WorkerRole::Execution,
            "OKX",
            Some("main"),
            Some("rules.json"),
        );
        let spec = TradingInstrumentSpec::spot_default(&instrument, "USDT");
        let rules = InstrumentRulesV1::from_spec(&worker, &instrument, spec, "rules.json").unwrap();
        rules.validate().unwrap();
        assert_eq!(rules.fingerprint.len(), 64);

        let mut mutated = rules;
        mutated.spec.qty_step += 1;
        assert!(mutated.validate().unwrap_err().contains("fingerprint"));
    }

    #[test]
    fn instrument_rules_snapshot_rejects_cross_venue_or_wrong_instrument() {
        let instrument = InstrumentId::parse("PEPE/USDT.OKX").unwrap();
        let worker = worker(
            "binance-exec",
            WorkerRole::Execution,
            "BINANCE",
            Some("main"),
            Some("rules.json"),
        );
        let spec = TradingInstrumentSpec::spot_default(&instrument, "USDT");
        assert!(
            InstrumentRulesV1::from_spec(&worker, &instrument, spec, "rules.json")
                .unwrap_err()
                .contains("身份不一致")
        );
    }
}
