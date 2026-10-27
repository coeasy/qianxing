//! API service host policies shared by command and daemon facades.

use qx_api::{ApiService, ApiState, ControlSubmitError};
use qx_control::{CommandKind, ControlCommand};
use qx_core::{DecimalRounding, ExactDecimal, InstrumentId, TradingInstrumentSpec, TradingProduct};
use qx_runtime::{
    configured_account_event_logs, owns_account_event_log, worker_settlement_currency,
    LiveEventPipeline, OutboxRecovery, RuntimeConfig, WorkerConfig, WorkerRole,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::api_read_model::default_account_event_log;
use crate::api_read_model::{
    load_reconcile_reports, project_api_account_snapshot_with_portfolio_and_market_specs,
};
use crate::configured_backends::{configured_command_queue, configured_control_store};
use crate::http_host::{api_service_from_runtime, configure_api_host_policies};
use crate::market_bridges::{ccxt_market_event_log_name, worker_scoped_event_log_name};
use crate::pipeline::{event_log_exists, PipelineStorage};

const PORTFOLIO_FX_MAX_QUOTE_AGE_MS: u64 = 30_000;

/// Assemble the API service from one immutable runtime snapshot.
///
/// The CLI facade supplies only policies that depend on its process boundary (readiness,
/// algorithm execution and submit-payload validation). Control-plane persistence, command
/// enqueueing, query projections, provider setup, and storage-specific API policy stay here.
pub fn build_configured_api_service(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
    python_interpreter: &str,
    now_ms: fn() -> u64,
    readiness_provider: impl Fn(crate::configured_backends::ControlStateBackend) -> qx_api::ApiReadiness
        + Send
        + Sync
        + 'static,
    algo_execution_provider: impl Fn(&str, &str) -> Result<String, (u16, String)>
        + Send
        + Sync
        + 'static,
    validate_control_payload: impl Fn(&ControlCommand) -> Result<(), ControlSubmitError>
        + Send
        + Sync
        + 'static,
) -> Result<ApiService, String> {
    std::fs::create_dir_all(&config.storage.data_dir)
        .map_err(|error| format!("创建运行时 data_dir 失败: {error}"))?;
    let control_root = Path::new(&config.storage.data_dir).to_path_buf();
    let control_store = configured_control_store(config)?;
    let control = control_store.load()?;
    let command_queue = configured_command_queue(config, &control_root)?;
    let mut state = ApiState::default();
    state.control = control;
    let portfolio_fx_specs = configured_portfolio_fx_specs(config, runtime_config_path)?;
    hydrate_api_initial_state_with_market_specs(
        config,
        &control_root,
        &mut state,
        &portfolio_fx_specs,
    )?;

    let readiness_store = control_store.clone();
    let metrics_dir = crate::worker_metrics::worker_metrics_dir(config);
    let gateway_metrics_dir = metrics_dir.clone();
    let metrics_stale_after_ms = config.messaging.worker_stale_after_ms;
    let metrics_now_ms = now_ms;
    let gateway_config = config.clone();
    let mut service = api_service_from_runtime(state, config)
        .with_worker_metrics_provider(move || {
            crate::worker_metrics::read_worker_metrics(
                &metrics_dir,
                metrics_now_ms(),
                metrics_stale_after_ms,
            )
        })
        .with_readiness_provider(move || {
            let mut readiness = readiness_provider(readiness_store.clone());
            let now_ms = metrics_now_ms();
            let metrics = crate::worker_metrics::read_worker_metrics(
                &gateway_metrics_dir,
                now_ms,
                metrics_stale_after_ms,
            );
            crate::exchange_gateway::attach_gateway_health_to_readiness(
                &mut readiness,
                &gateway_config,
                &metrics,
                now_ms,
                metrics_stale_after_ms,
            );
            readiness
        })
        .with_algo_execution_provider(algo_execution_provider)
        .with_control_plane_provider({
            let store = control_store.clone();
            move || store.load()
        })
        .with_control_submitter({
            let store = control_store.clone();
            move |command, granted, ts| {
                if matches!(
                    &command.kind,
                    CommandKind::SubmitOrder | CommandKind::CancelOrder
                ) {
                    validate_control_payload(&command)?;
                }
                let (plane, result) = store
                    .transact(|plane| {
                        plane
                            .submit_as(command, granted, ts)
                            .map_err(ControlSubmitError::Rejected)
                    })
                    .map_err(ControlSubmitError::Unavailable)?;
                result.map(|audit| (plane, audit))
            }
        })
        .with_command_enqueuer({
            let queue = Arc::clone(&command_queue);
            move |command, ts| {
                queue
                    .enqueue_command(command, qx_runtime::lease_clock(ts))
                    .map(|_| ())
                    .map_err(|error| format!("写入控制命令队列失败: {error:?}"))
            }
        });
    service.set_portfolio_fx_market_specs(portfolio_fx_specs)?;
    service = attach_query_models_provider(service, config);
    if let Some(registry) = crate::provider_registry::from_runtime_config(
        config,
        runtime_config_path,
        python_interpreter,
    )? {
        service = service.with_provider_registry(registry);
    }
    configure_api_host_policies(service, config, &control_root)
}

fn configured_portfolio_fx_specs(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
) -> Result<BTreeMap<InstrumentId, TradingInstrumentSpec>, String> {
    let mut specs =
        crate::api_read_model::configured_market_specs(config, Some(runtime_config_path))?;
    specs.retain(|_, spec| spec.product == TradingProduct::Spot);
    Ok(specs)
}

fn portfolio_fx_rates(
    specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
    quotes: &BTreeMap<InstrumentId, (ExactDecimal, ExactDecimal, u64)>,
    now_ms: u64,
) -> BTreeMap<String, BTreeMap<String, ExactDecimal>> {
    let mut marks = BTreeMap::new();
    for (instrument, (bid, ask, timestamp)) in quotes {
        if now_ms
            .checked_sub(*timestamp)
            .is_none_or(|age| age > PORTFOLIO_FX_MAX_QUOTE_AGE_MS)
            || !bid.is_positive()
            || !ask.is_positive()
            || bid
                .checked_cmp(*ask)
                .is_none_or(|ordering| ordering.is_gt())
        {
            continue;
        }
        let Some(mid) = bid.checked_add(*ask).and_then(|sum| {
            sum.checked_div_at_scale(
                ExactDecimal::from_parts(2, 0).ok()?,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::TowardZero,
            )
        }) else {
            continue;
        };
        if mid.is_positive() {
            marks.insert(instrument.clone(), mid);
        }
    }
    let currencies = specs
        .values()
        .filter(|spec| spec.product == TradingProduct::Spot)
        .flat_map(|spec| {
            [
                spec.base_currency.trim().to_ascii_uppercase(),
                spec.quote_currency.trim().to_ascii_uppercase(),
            ]
        })
        .filter(|currency| !currency.is_empty())
        .collect::<BTreeSet<_>>();
    let mut by_reporting = BTreeMap::new();
    for reporting in &currencies {
        let mut rates = BTreeMap::from([(reporting.clone(), ExactDecimal::ONE)]);
        for source in currencies.iter().filter(|source| *source != reporting) {
            if let Ok(resolved) = crate::risk_host::spot_fx_exact_rates_to_reporting(
                specs,
                &marks,
                [source.clone()],
                reporting,
            ) {
                if let Some(rate) = resolved.get(source) {
                    rates.insert(source.clone(), *rate);
                }
            }
        }
        by_reporting.insert(reporting.clone(), rates);
    }
    by_reporting
}

fn wall_clock_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn hydrate_api_initial_state_with_market_specs(
    config: &RuntimeConfig,
    data_root: &Path,
    state: &mut ApiState,
    market_specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
) -> Result<(), String> {
    let models = crate::api_read_model::query_api_models(config)?;
    state.job_runs = models.job_runs;
    state.ledger_entries = models.ledger_entries;
    state.reconcile_reports = models
        .reconcile_reports
        .into_iter()
        .map(|report| (report.worker_id.clone(), report))
        .collect();

    // Keep boot's unscoped compatibility events in sync with the same default account used by
    // snapshots and the live projection bridge.
    let default_log = default_account_event_log(config, data_root)?;
    let mut storage = PipelineStorage::from_config(config)?;
    storage.root = data_root.to_path_buf();
    for (account_id, venue_id, log_name, currency) in configured_account_event_logs(config)? {
        if crate::pipeline::event_log_exists(config, data_root, &log_name)? {
            let pipeline = storage
                .open_with(log_name.clone(), currency, OutboxRecovery::ReadOnly)
                .map_err(|error| format!("读取 API 事件投影失败: {error}"))?;
            state
                .project_account_event_log(&account_id, &venue_id, pipeline.log())
                .map_err(|error| format!("初始化 API 账户事件投影失败: {error}"))?;
            if default_log.as_deref() == Some(log_name.as_str()) {
                state
                    .project_event_log(pipeline.log())
                    .map_err(|error| format!("初始化 API 全局事件投影失败: {error}"))?;
            }
        }
    }

    // Only the configured default account feeds the unscoped compatibility snapshot.
    for snapshot in
        crate::api_read_model::load_api_account_snapshots_with_market_specs(config, market_specs)?
    {
        let identity = qx_runtime::account_event_log_name(
            &snapshot.header.account_id,
            &snapshot.header.venue_id,
        );
        let loaded = if identity.is_some() && identity == default_log {
            state.publish_snapshot(snapshot)
        } else {
            state.publish_snapshot_for(
                snapshot.header.account_id.clone(),
                snapshot.header.venue_id.clone(),
                snapshot,
            )
        };
        loaded.map_err(|error| format!("装载 API 账户查询快照失败: {error}"))?;
    }
    Ok(())
}

/// Start the read-only account EventLog projection loop owned by the API service.
///
/// Transient open/refresh failures are retried. Projection conflicts are permanently retired for
/// this process because replaying the same facts cannot repair a sequence or identity conflict.
pub fn spawn_api_projection_bridge(
    config: &RuntimeConfig,
    service: ApiService,
    stop: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    let sources = match configured_account_event_logs(config) {
        Ok(sources) => sources,
        Err(error) => {
            eprintln!("[运行时 · API] 账户 EventLog 投影桥未启动: {error}");
            return None;
        }
    };
    let fx_specs = match service.portfolio_fx_market_specs() {
        Ok(specs) => specs,
        Err(error) => {
            eprintln!("[运行时 · API] 读取组合 FX 规格失败: {error}");
            BTreeMap::new()
        }
    };
    let mut market_sources = Vec::<(WorkerConfig, String)>::new();
    for worker in config.workers.iter().filter(|worker| {
        worker.enabled && worker.role == WorkerRole::MarketData && !worker.symbols.is_empty()
    }) {
        let has_fx_instrument = worker.symbols.iter().any(|symbol| {
            InstrumentId::parse(symbol).is_some_and(|instrument| fx_specs.contains_key(&instrument))
        });
        if !has_fx_instrument {
            continue;
        }
        market_sources.push((worker.clone(), worker_scoped_event_log_name("", &worker.id)));
        market_sources.push((worker.clone(), ccxt_market_event_log_name(worker)));
    }
    if sources.is_empty() && market_sources.is_empty() {
        return None;
    }
    let storage = match PipelineStorage::from_config(config) {
        Ok(storage) => storage,
        Err(error) => {
            eprintln!("[运行时 · API] 初始化 EventLog 投影桥失败: {error}");
            return None;
        }
    };
    let default_log = match default_account_event_log(config, Path::new(&config.storage.data_dir)) {
        Ok(name) => name,
        Err(error) => {
            eprintln!("[运行时 · API] 解析默认账户 EventLog 失败: {error}");
            None
        }
    };
    let snapshot_workers = config
        .workers
        .iter()
        .filter(|worker| owns_account_event_log(worker))
        .filter_map(|worker| {
            let (account_id, venue_id) =
                (worker.account_id.as_deref()?, worker.venue_id.as_deref()?);
            Some((
                (account_id.to_string(), venue_id.to_string()),
                worker.clone(),
            ))
        })
        .collect::<BTreeMap<(String, String), WorkerConfig>>();
    let portfolio_ids = config
        .accounts
        .iter()
        .map(|account| {
            (
                account.id.clone(),
                account
                    .portfolio_id
                    .clone()
                    .unwrap_or_else(|| account.id.clone()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let data_root = Path::new(&config.storage.data_dir).to_path_buf();
    let runtime_config = config.clone();
    let poll_interval = Duration::from_millis(250);
    Some(thread::spawn(move || {
        let mut pipelines = BTreeMap::<(String, String), LiveEventPipeline>::new();
        // A projection conflict is not an open/refresh transient. Retire it permanently so the
        // polling loop cannot reopen the same ledger and repeat the same failure every 250 ms.
        let mut permanently_retired = BTreeSet::<(String, String)>::new();
        let mut market_pipelines = BTreeMap::<String, LiveEventPipeline>::new();
        let mut market_cursors = BTreeMap::<String, u64>::new();
        let mut market_next_probe_ms = BTreeMap::<String, u64>::new();
        let mut latest_market_quotes =
            BTreeMap::<InstrumentId, (ExactDecimal, ExactDecimal, u64)>::new();
        let mut next_fx_refresh_ms = 0_u64;
        while !stop.load(Ordering::Acquire) {
            for (account_id, venue_id, log_name, currency) in &sources {
                let pipeline_key = (account_id.clone(), venue_id.clone());
                if permanently_retired.contains(&pipeline_key) {
                    continue;
                }
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    pipelines.entry(pipeline_key.clone())
                {
                    match storage.open_read_only(log_name.clone(), currency.clone()) {
                        Ok(opened) => {
                            entry.insert(opened);
                        }
                        Err(error) => {
                            eprintln!(
                                "[运行时 · API] 打开账户 EventLog 投影源失败 account={} venue={}: {error}",
                                account_id, venue_id
                            );
                            continue;
                        }
                    }
                }
                let Some(current) = pipelines.get_mut(&pipeline_key) else {
                    continue;
                };
                if let Err(error) = current.refresh() {
                    eprintln!(
                        "[运行时 · API] 刷新账户 EventLog 投影源失败 account={} venue={}: {error:?}",
                        account_id, venue_id
                    );
                    pipelines.remove(&pipeline_key);
                    continue;
                }
                // Commit the account view first. If it fails, the default global view must not
                // advance independently from the same account's scoped `/events` result.
                let projected = match service.project_account_event_log(
                    account_id,
                    venue_id,
                    current.log(),
                ) {
                    Ok(projected) => projected,
                    Err(error) => {
                        eprintln!(
                            "[运行时 · API] 写入账户查询投影失败（不会自愈，停止重试）account={} venue={}: {error}",
                            account_id, venue_id
                        );
                        permanently_retired.insert(pipeline_key.clone());
                        pipelines.remove(&pipeline_key);
                        continue;
                    }
                };
                // Refresh the scoped snapshot on new account facts as well as the event stream.
                // Without this, `/account/portfolio` and `/account/balances` would keep showing
                // the API boot-time balances while their event cursors had already advanced.
                if projected > 0 {
                    if let Some(worker) = snapshot_workers.get(&pipeline_key) {
                        let portfolio_id = portfolio_ids
                            .get(account_id)
                            .map(String::as_str)
                            .unwrap_or(account_id);
                        match load_reconcile_reports(&data_root) {
                            Ok(reports) => {
                                match project_api_account_snapshot_with_portfolio_and_market_specs(
                                    worker,
                                    log_name,
                                    current,
                                    &reports,
                                    portfolio_id,
                                    &fx_specs,
                                ) {
                                    Ok(snapshot) => {
                                        let publish =
                                            if default_log.as_deref() == Some(log_name.as_str()) {
                                                service.publish_snapshot(snapshot)
                                            } else {
                                                service.publish_snapshot_for(
                                                    account_id, venue_id, snapshot,
                                                )
                                            };
                                        if let Err(error) = publish {
                                            eprintln!(
                                                "[运行时 · API] 发布账户查询快照失败 account={} venue={}: {error}",
                                                account_id, venue_id
                                            );
                                        }
                                    }
                                    Err(error) => eprintln!(
                                        "[运行时 · API] 刷新账户查询快照失败 account={} venue={}: {error}",
                                        account_id, venue_id
                                    ),
                                }
                            }
                            Err(error) => eprintln!(
                                "[运行时 · API] 读取快照对账报告失败 account={} venue={}: {error}",
                                account_id, venue_id
                            ),
                        }
                    }
                }
                // The unscoped compatibility endpoints represent only the configured default
                // account; all other accounts remain available through identity-scoped queries.
                if default_log.as_deref() == Some(log_name.as_str()) {
                    if let Err(error) = service.project_event_log(current.log()) {
                        eprintln!(
                            "[运行时 · API] 写入全局查询投影失败（不会自愈，停止重试）account={} venue={}: {error}",
                            account_id, venue_id
                        );
                        permanently_retired.insert(pipeline_key.clone());
                        pipelines.remove(&pipeline_key);
                    }
                }
            }

            let market_now_ms = wall_clock_ms();
            for (worker, log_name) in &market_sources {
                if !market_pipelines.contains_key(log_name)
                    && market_now_ms
                        >= market_next_probe_ms
                            .get(log_name)
                            .copied()
                            .unwrap_or_default()
                {
                    market_next_probe_ms
                        .insert(log_name.clone(), market_now_ms.saturating_add(1_000));
                    match event_log_exists(&runtime_config, &data_root, log_name) {
                        Ok(true) => match storage
                            .open_read_only(log_name.clone(), worker_settlement_currency(worker))
                        {
                            Ok(opened) => {
                                market_pipelines.insert(log_name.clone(), opened);
                            }
                            Err(error) => eprintln!(
                                "[运行时 · API] 打开行情 EventLog 失败 source={log_name}: {error}"
                            ),
                        },
                        Ok(false) => {}
                        Err(error) => eprintln!(
                            "[运行时 · API] 检查行情 EventLog 失败 source={log_name}: {error}"
                        ),
                    }
                }
                let Some(current) = market_pipelines.get_mut(log_name) else {
                    continue;
                };
                if let Err(error) = current.refresh() {
                    eprintln!("[运行时 · API] 刷新行情 EventLog 失败 source={log_name}: {error:?}");
                    market_pipelines.remove(log_name);
                    market_cursors.remove(log_name);
                    continue;
                }
                let cursor = market_cursors.get(log_name).copied().unwrap_or_default();
                for event in current
                    .log()
                    .events()
                    .iter()
                    .filter(|event| event.seq > cursor)
                {
                    let qx_core::EventKind::MarketQuote {
                        instrument,
                        bid,
                        ask,
                        bid_exact,
                        ask_exact,
                        ..
                    } = &event.kind
                    else {
                        continue;
                    };
                    if !fx_specs.contains_key(instrument) {
                        continue;
                    }
                    let bid_exact =
                        bid_exact.or_else(|| ExactDecimal::from_parts(bid.raw(), 9).ok());
                    let ask_exact =
                        ask_exact.or_else(|| ExactDecimal::from_parts(ask.raw(), 9).ok());
                    let (Some(bid_exact), Some(ask_exact)) = (bid_exact, ask_exact) else {
                        continue;
                    };
                    if !bid_exact.is_positive()
                        || !ask_exact.is_positive()
                        || bid_exact
                            .checked_cmp(ask_exact)
                            .is_none_or(|ordering| ordering.is_gt())
                    {
                        continue;
                    }
                    let replace = latest_market_quotes
                        .get(instrument)
                        .is_none_or(|(_, _, existing_ts)| event.ts >= *existing_ts);
                    if replace {
                        latest_market_quotes
                            .insert(instrument.clone(), (bid_exact, ask_exact, event.ts));
                    }
                }
                if let Some(last_seq) = current.log().events().last().map(|event| event.seq) {
                    market_cursors.insert(log_name.clone(), last_seq);
                }
            }
            if !fx_specs.is_empty() && market_now_ms >= next_fx_refresh_ms {
                let now_ms = wall_clock_ms();
                next_fx_refresh_ms = now_ms.saturating_add(1_000);
                let rates = portfolio_fx_rates(&fx_specs, &latest_market_quotes, now_ms);
                if let Err(error) =
                    service.publish_portfolio_fx_rates(now_ms, PORTFOLIO_FX_MAX_QUOTE_AGE_MS, rates)
                {
                    eprintln!("[运行时 · API] 发布组合 FX 汇率失败: {error}");
                }
            }
            thread::sleep(poll_interval);
        }
    }))
}

/// Attach the live query-model callback using the same runtime snapshot used at service boot.
pub fn attach_query_models_provider(service: ApiService, config: &RuntimeConfig) -> ApiService {
    let config = config.clone();
    service.with_query_models_provider(move || crate::api_read_model::query_api_models(&config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_host::{api_service_from_runtime, configure_api_host_policies};
    use qx_api::ApiState;
    use qx_core::{InstrumentId, Price, Quantity, VenueId};
    use qx_guanxing::QuoteTick;
    use qx_runtime::RuntimeEventEnvelope;
    use std::time::Instant;

    #[test]
    fn invalid_connection_limit_fails_api_host_configuration() {
        let mut config = RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.production.example.json"
        ))
        .expect("production runtime example is valid");
        config.api.max_concurrent_connections = Some(0);
        let root = std::env::temp_dir().join(format!("qx-service-api-host-{}", std::process::id()));
        let service = ApiService::new(ApiState::default());
        let error = match configure_api_host_policies(service, &config, &root) {
            Ok(_) => panic!("zero concurrent connections is invalid"),
            Err(error) => error,
        };
        assert!(
            error.contains("max_concurrent_connections"),
            "unexpected error: {error}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_operator_policy_is_applied_when_api_service_is_created() {
        let config = RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.production.example.json"
        ))
        .expect("production runtime example is valid");
        let service = api_service_from_runtime(ApiState::default(), &config);
        let body = include_str!("../../../deploy/qianxing.submit-order.example.json");
        let response = service.handle("POST", "/control/commands", body, 7);
        assert_eq!(
            response.status, 403,
            "configured mTLS operators require an authenticated identity"
        );
        assert!(response.body.contains("authenticated_operator_required"));
    }

    #[test]
    fn ready_response_exposes_account_scoped_gateway_port_health() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let root = workspace_root
            .join("target")
            .join(format!("qx-service-gateway-ready-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create gateway readiness fixture root");
        let mut config = RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .expect("paper runtime fixture is valid");
        config.storage.data_dir = root.to_string_lossy().into_owned();
        crate::worker_metrics::write_worker_metrics(
            &crate::worker_metrics::worker_metrics_path(&config, "paper-execution"),
            concat!(
                "qx_worker_up{worker=\"paper-execution\",account=\"main-paper-events\"} 1\n",
                "qx_worker_heartbeat_timestamp_seconds{worker=\"paper-execution\",account=\"main-paper-events\"} 90\n",
                "qx_gateway_port_business_status{worker=\"paper-execution\",account=\"main-paper-events\",port=\"venue_execution\"} 2\n",
                "qx_gateway_port_business_timestamp_seconds{worker=\"paper-execution\",account=\"main-paper-events\",port=\"venue_execution\"} 90\n"
            ),
        )
        .expect("publish worker health fixture");
        let runtime_path =
            workspace_root.join("deploy/qianxing.runtime.paper-strategy.example.json");
        let service = build_configured_api_service(
            &config,
            &runtime_path,
            "python",
            || 100_000,
            |_| qx_api::ApiReadiness {
                schema_version: qx_api::API_READINESS_SCHEMA_VERSION,
                ready: true,
                detail: "fixture_ready".into(),
                readiness_layers: BTreeMap::from([(
                    "O".into(),
                    serde_json::json!({
                        "status": "pass",
                        "scope": "api_service_dependencies",
                        "evidence": [],
                        "detail": "fixture"
                    }),
                )]),
            },
            |_, _| Ok("{}".into()),
            |_| Ok(()),
        )
        .expect("assemble configured API service");
        let response = service.handle("GET", "/ready", "", 100_000);
        assert_eq!(response.status, 200, "{}", response.body);
        let body: serde_json::Value =
            serde_json::from_str(&response.body).expect("readiness JSON is valid");
        let gateway = body["readiness_layers"]["O"]["exchange_gateways"]["gateways"]
            .as_array()
            .expect("gateway list is exposed")
            .iter()
            .find(|gateway| gateway["venue_id"] == "PAPER" && gateway["account_id"] == "main")
            .unwrap_or_else(|| {
                panic!(
                    "paper main account gateway is present; gateways={}",
                    body["readiness_layers"]["O"]["exchange_gateways"]["gateways"]
                )
            });
        let ports = gateway["ports"].as_array().expect("ports are exposed");
        let port_status = |name: &str| {
            ports
                .iter()
                .find(|port| port["port"] == name)
                .and_then(|port| port["status"].as_str())
                .expect("gateway port status exists")
        };
        let port_business_status = |name: &str| {
            ports
                .iter()
                .find(|port| port["port"] == name)
                .and_then(|port| port["business_status"].as_str())
                .expect("gateway business port status exists")
        };
        assert_eq!(port_status("venue_execution"), "healthy");
        assert_eq!(port_status("account_query"), "healthy");
        assert_eq!(port_status("market_data"), "unconfigured");
        assert_eq!(port_business_status("venue_execution"), "healthy");
        assert_eq!(port_business_status("account_query"), "unobserved");
        assert_eq!(port_business_status("market_data"), "unobserved");
        assert_eq!(gateway["capability_profile"]["schema_version"], 1);
        std::fs::remove_dir_all(&root).expect("remove gateway readiness fixture root");
    }

    #[test]
    fn projection_bridge_publishes_account_and_default_events_then_stops() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .join("target")
            .join(format!("qx-service-api-projection-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create API projection fixture root");
        let mut config = RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .expect("paper runtime fixture is valid");
        config.storage.data_dir = root.to_string_lossy().into_owned();

        let mut pipeline = PipelineStorage::from_config(&config)
            .expect("configured file storage")
            .open("paper-main-paper-events", "USDT")
            .expect("open account ledger writer");
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                InstrumentId::new("BTCUSDT", VenueId::new("BINANCE")),
                QuoteTick::new(
                    10,
                    Price::from_i64(99),
                    Quantity::from_i64(2),
                    Price::from_i64(100),
                    Quantity::from_i64(3),
                    1,
                ),
                11,
                1,
                "api-projection-bridge-test",
            ))
            .expect("append source event");
        drop(pipeline);

        let service = ApiService::new(ApiState::default());
        let stop = Arc::new(AtomicBool::new(false));
        let mut projection = spawn_api_projection_bridge(&config, service.clone(), stop.clone());
        let deadline = Instant::now() + Duration::from_secs(2);
        let account_events = loop {
            let response = service.handle("GET", "/events?account_id=main&venue_id=paper", "", 12);
            if response.status == 200 {
                let events: Vec<serde_json::Value> =
                    serde_json::from_str(&response.body).expect("account events are JSON");
                if !events.is_empty() {
                    break events;
                }
            }
            assert!(
                Instant::now() < deadline,
                "projection bridge did not publish account events"
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(account_events.len(), 1);
        let default_events = service.handle("GET", "/events", "", 12);
        let default_events: Vec<serde_json::Value> =
            serde_json::from_str(&default_events.body).expect("default events are JSON");
        assert_eq!(
            default_events.len(),
            1,
            "default account feeds the global projection"
        );

        stop.store(true, Ordering::Release);
        projection
            .take()
            .expect("projection thread is started")
            .join()
            .expect("projection thread stops cleanly");
        std::fs::remove_dir_all(root).expect("remove API projection fixture root");
    }

    #[test]
    fn initial_api_state_hydrates_persisted_account_events() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .join("target")
            .join(format!(
                "qx-service-api-initial-state-{}",
                std::process::id()
            ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create API initial-state fixture root");
        let mut config = RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .expect("paper runtime fixture is valid");
        config.storage.data_dir = root.to_string_lossy().into_owned();
        let mut pipeline = PipelineStorage::from_config(&config)
            .expect("configured file storage")
            .open("paper-main-paper-events", "USDT")
            .expect("open account ledger writer");
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                InstrumentId::new("BTCUSDT", VenueId::new("BINANCE")),
                QuoteTick::new(
                    20,
                    Price::from_i64(199),
                    Quantity::from_i64(4),
                    Price::from_i64(200),
                    Quantity::from_i64(5),
                    1,
                ),
                21,
                1,
                "api-initial-state-test",
            ))
            .expect("append persisted account event");
        drop(pipeline);

        let mut state = ApiState::default();
        hydrate_api_initial_state_with_market_specs(&config, &root, &mut state, &BTreeMap::new())
            .expect("hydrate boot read models");
        let service = ApiService::new(state);
        let scoped = service.handle("GET", "/events?account_id=main&venue_id=paper", "", 22);
        let events: Vec<serde_json::Value> =
            serde_json::from_str(&scoped.body).expect("boot account projection is JSON");
        assert_eq!(events.len(), 1);
        let global = service.handle("GET", "/events", "", 22);
        let events: Vec<serde_json::Value> =
            serde_json::from_str(&global.body).expect("boot global projection is JSON");
        assert_eq!(events.len(), 1);
        let snapshot = service.handle(
            "GET",
            "/account/snapshot?account_id=main&venue_id=paper",
            "",
            22,
        );
        assert_eq!(
            snapshot.status, 200,
            "boot snapshot is available: {}",
            snapshot.body
        );
        std::fs::remove_dir_all(root).expect("remove API initial-state fixture root");
    }
}
