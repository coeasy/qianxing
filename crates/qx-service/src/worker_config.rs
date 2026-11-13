//! Runtime worker selection and venue-binding validation shared by service entrypoints.

use qx_core::VenueFamily;
use qx_runtime::{RuntimeConfig, WorkerConfig, WorkerRole};

/// A configured venue role accepted by one worker entrypoint.
pub struct VenueEntry {
    name: &'static str,
    is_bound: fn(&WorkerConfig) -> bool,
    venue_hint: &'static str,
}

impl VenueEntry {
    pub const CCXT: Self = Self {
        name: "CCXT",
        is_bound: |worker| {
            VenueFamily::parse_option(worker.venue_id.as_deref()) != Some(VenueFamily::Paper)
        },
        venue_hint: "必须绑定非 paper 的 CCXT venue",
    };

    pub const BINANCE: Self = Self {
        name: "Binance",
        is_bound: |worker| {
            VenueFamily::parse_option(worker.venue_id.as_deref()) == Some(VenueFamily::Binance)
        },
        venue_hint: "必须绑定 Binance Venue",
    };
}

/// Resolve a configured worker only after enabled, role and venue checks pass.
pub fn venue_worker(
    config: &RuntimeConfig,
    worker_id: &str,
    entry: &VenueEntry,
) -> Result<WorkerConfig, String> {
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    if !worker.enabled {
        return Err(format!("worker {worker_id} 未启用"));
    }
    if !worker.role.is_venue_role() {
        let roles = qx_runtime::ALL_WORKER_ROLES
            .iter()
            .filter(|role| role.is_venue_role())
            .map(|role| worker_role_label(*role))
            .collect::<Vec<_>>()
            .join("/");
        return Err(format!(
            "worker {worker_id} 不是 {} 支持的 {roles} 角色",
            entry.name
        ));
    }
    if !(entry.is_bound)(&worker) {
        return Err(format!("worker {worker_id} {}", entry.venue_hint));
    }
    crate::exchange_gateway::ExchangeGatewayRegistry::from_runtime_config(config)?
        .require_worker_binding(worker_id)?;
    Ok(worker)
}

/// Resolve the sole enabled Binance reconciler, or require an explicit worker id.
pub fn configured_reconcile_worker_id(
    config: &RuntimeConfig,
    entry: &VenueEntry,
) -> Result<String, String> {
    let candidates = config
        .workers
        .iter()
        .filter(|worker| {
            worker.enabled && worker.role == WorkerRole::Reconciler && (entry.is_bound)(worker)
        })
        .map(|worker| worker.id.clone())
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(format!(
            "运行时配置没有可解析的 reconciler worker（要求：启用且{}），请用 \
             reconcile <runtime.json> <worker-id> 显式点名",
            entry.venue_hint
        )),
        many => Err(format!(
            "运行时配置有 {} 个候选 reconciler worker（{}），请用 \
             reconcile <runtime.json> <worker-id> 显式点名",
            many.len(),
            many.join(", ")
        )),
    }
}

/// Recovery scans must name their venue; an empty filter can silently hide open exposure.
pub fn recovery_worker_venue_id(worker: &WorkerConfig) -> Result<String, String> {
    worker.venue_id.clone().ok_or_else(|| {
        format!(
            "worker {}（{}）必须配置 venue_id：恢复扫描按此过滤敞口腿，缺省会静默收窄扫描范围、漏掉其他 venue 的裸腿",
            worker.id,
            worker_role_label(worker.role)
        )
    })
}

const fn worker_role_label(role: WorkerRole) -> &'static str {
    match role {
        WorkerRole::Api => "api",
        WorkerRole::MarketData => "market-data",
        WorkerRole::UserStream => "user-stream",
        WorkerRole::Execution => "execution",
        WorkerRole::SpreadRecovery => "spread-recovery",
        WorkerRole::Scheduler => "scheduler",
        WorkerRole::Reconciler => "reconciler",
        WorkerRole::Strategy => "strategy",
        WorkerRole::OutboxRelay => "outbox-relay",
        WorkerRole::EventConsumer => "event-consumer",
    }
}
