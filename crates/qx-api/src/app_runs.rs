//! Asynchronous lifecycle routes for the shared application use cases.

use super::*;
#[path = "app_runs/entry.rs"]
mod entry;
pub(super) use entry::{cancel_route_id, is_terminal, status_route_id, valid_run_route_id};
use qx_app::{
    MultiAccountBacktestResult, MultiAccountBacktestSpec, MultiAccountDepthBacktestResult,
    MultiAccountDepthBacktestSpec, MultiLegBacktestOutcome, MultiLegBacktestSpec,
    MultiMarketBacktestOutcome, MultiMarketBacktestSpec, MultiMarketDepthBacktestOutcome,
    MultiMarketDepthBacktestSpec, RunDepthExperimentResult, RunDepthExperimentSpec, RunEvent,
    RunExperimentResult, RunHandle, RunMultiLegExperimentResult, RunMultiLegExperimentSpec,
    RunMultiMarketDepthExperimentResult, RunMultiMarketExperimentResult,
    RunMultiMarketExperimentSpec, RunStatus,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[path = "app_runs/depth_experiment.rs"]
mod depth_experiment;
#[path = "app_runs/events.rs"]
mod events;
#[path = "app_runs/path_resolution.rs"]
mod path_resolution;
#[path = "app_runs/persistence.rs"]
mod persistence;
#[path = "app_runs/starts.rs"]
mod starts;
pub(crate) use events::{accepts_run_event_query, events_route_cursor};
pub(super) use path_resolution::resolve_input_path;

const APP_RUN_CAPACITY: usize = 1024;
const APP_RUN_WATCHER_CAPACITY: usize = 64;
static APP_RUN_WATCHERS: AtomicUsize = AtomicUsize::new(0);
pub(crate) const RUN_START_BACKTEST_ROUTE: &str = "/app/backtest/start";
pub(crate) const RUN_STATUS_ROUTE_TEMPLATE: &str = "/app/runs/{run_id}";
pub(crate) const RUN_EVENTS_ROUTE_TEMPLATE: &str = "/app/runs/{run_id}/events";
pub(crate) const RUN_CANCEL_ROUTE_TEMPLATE: &str = "/app/runs/{run_id}/cancel";
pub(crate) const RUN_START_DEPTH_ROUTE: &str = "/app/depth-backtest/start";
pub(crate) const RUN_START_DEPTH_EXPERIMENT_ROUTE: &str = "/app/run-depth-experiment/start";
pub(crate) const RUN_START_EXPERIMENT_ROUTE: &str = "/app/run-experiment/start";
pub(crate) const RUN_START_MULTI_LEG_ROUTE: &str = "/app/multi-leg-backtest/start";
pub(crate) const RUN_START_MULTI_ACCOUNT_ROUTE: &str = "/app/multi-account-backtest/start";
pub(crate) const RUN_START_MULTI_ACCOUNT_DEPTH_ROUTE: &str =
    "/app/multi-account-depth-backtest/start";
pub(crate) const RUN_START_MULTI_MARKET_ROUTE: &str = "/app/multi-market-backtest/start";
pub(crate) const RUN_START_MARKET_DEPTH_ROUTE: &str = "/app/multi-market-depth-backtest/start";
pub(crate) const RUN_START_MULTI_LEG_EXPERIMENT_ROUTE: &str = "/app/run-multi-leg-experiment/start";
pub(crate) const RUN_START_MARKET_EXPERIMENT_ROUTE: &str = "/app/run-multi-market-experiment/start";
pub(crate) const RUN_START_MARKET_DEPTH_EXPERIMENT_ROUTE: &str =
    "/app/run-multi-market-depth-experiment/start";

pub(crate) fn app_run_route(
    method: &str,
    route: &str,
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> Option<ApiResponse> {
    app_run_route_with_query(method, route, "", body, paths, registry)
}

pub(crate) fn app_run_route_with_query(
    method: &str,
    route: &str,
    query: &str,
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> Option<ApiResponse> {
    let response = match (method, route) {
        ("POST", RUN_START_BACKTEST_ROUTE) => starts::start_run_backtest(body, paths, registry),
        ("POST", RUN_START_DEPTH_ROUTE) => starts::start_depth_backtest(body, paths, registry),
        ("POST", RUN_START_DEPTH_EXPERIMENT_ROUTE) => {
            depth_experiment::start(body, paths, registry)
        }
        ("POST", RUN_START_EXPERIMENT_ROUTE) => starts::start_experiment(body, paths, registry),
        ("POST", RUN_START_MULTI_LEG_ROUTE) => {
            starts::start_multi_leg_backtest(body, paths, registry)
        }
        ("POST", RUN_START_MULTI_ACCOUNT_ROUTE) => {
            starts::start_multi_account_backtest(body, paths, registry)
        }
        ("POST", RUN_START_MULTI_ACCOUNT_DEPTH_ROUTE) => {
            starts::start_multi_account_depth_backtest(body, paths, registry)
        }
        ("POST", RUN_START_MULTI_MARKET_ROUTE) => {
            starts::start_multi_market_backtest(body, paths, registry)
        }
        ("POST", RUN_START_MARKET_DEPTH_ROUTE) => {
            starts::start_multi_market_depth_backtest(body, paths, registry)
        }
        ("POST", RUN_START_MULTI_LEG_EXPERIMENT_ROUTE) => {
            starts::start_multi_leg_experiment(body, paths, registry)
        }
        ("POST", RUN_START_MARKET_EXPERIMENT_ROUTE) => {
            starts::start_multi_market_experiment(body, paths, registry)
        }
        ("POST", RUN_START_MARKET_DEPTH_EXPERIMENT_ROUTE) => {
            starts::start_multi_market_depth_experiment(body, paths, registry)
        }
        ("GET", route) => {
            let full_route = if query.is_empty() {
                route.to_string()
            } else {
                format!("{route}?{query}")
            };
            if let Some((run_id, cursor)) = events_route_cursor(&full_route) {
                registry.events(run_id, cursor)
            } else {
                status_route_id(route).map(|run_id| registry.status(run_id))?
            }
        }
        ("POST", route) => cancel_route_id(route).map(|run_id| registry.cancel(run_id))?,
        _ => return None,
    };
    Some(response)
}

/// Async application runs exposed over HTTP. Durable snapshots retain terminal
/// results across service restarts; recovered active work is marked interrupted.
#[derive(Clone, Default)]
pub(crate) struct AppRunRegistry {
    entries: Arc<Mutex<BTreeMap<String, AppRunEntry>>>,
    persistence_root: Option<Arc<PathBuf>>,
    initialization_error: Option<AppError>,
}

enum AppRun {
    Bar(RunHandle<BacktestOutcome>),
    Depth(RunHandle<DepthBacktestOutcome>),
    DepthExperiment(RunHandle<RunDepthExperimentResult>),
    Experiment(RunHandle<RunExperimentResult>),
    MultiLeg(RunHandle<MultiLegBacktestOutcome>),
    MultiLegExperiment(RunHandle<RunMultiLegExperimentResult>),
    MultiMarketExperiment(RunHandle<RunMultiMarketExperimentResult>),
    MultiAccount(RunHandle<MultiAccountBacktestResult>),
    MultiAccountDepth(RunHandle<MultiAccountDepthBacktestResult>),
    MultiMarket(RunHandle<MultiMarketBacktestOutcome>),
    MultiMarketDepth(RunHandle<MultiMarketDepthBacktestOutcome>),
    MultiMarketDepthExperiment(RunHandle<RunMultiMarketDepthExperimentResult>),
}

struct AppRunEntry {
    run_id: String,
    run: Option<AppRun>,
    cancel_requested: bool,
    watcher_slot_reserved: bool,
    result: Option<String>,
    error: Option<String>,
    restored_status: Option<String>,
    events: Vec<RunEvent>,
}

impl AppRunRegistry {
    pub(crate) fn from_environment() -> Self {
        let paths = AppPaths::from_environment();
        Self::open(paths.artifact_root.join(".app-runs"))
    }

    pub(crate) fn open(root: PathBuf) -> Self {
        match persistence::load(&root) {
            Ok(entries) => Self {
                entries: Arc::new(Mutex::new(entries)),
                persistence_root: Some(Arc::new(root)),
                initialization_error: None,
            },
            Err(error) => Self {
                entries: Arc::new(Mutex::new(BTreeMap::new())),
                persistence_root: Some(Arc::new(root)),
                initialization_error: Some(error),
            },
        }
    }

    fn reserve(&self, run_id: String) -> Result<(), AppError> {
        self.ensure_storage_ready()?;
        let mut entries = self.entries.lock().map_err(|_| {
            AppError::new(AppErrorCategory::InternalInvariant, "应用运行注册表锁中毒")
        })?;
        if entries.contains_key(&run_id) {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("run_id 已在本进程运行或保留: {run_id}"),
            ));
        }
        let completed_to_evict = if entries.len() >= APP_RUN_CAPACITY {
            entries.iter_mut().find_map(|(run_id, entry)| {
                let terminal = match entry.restored_status.as_deref() {
                    Some(status) => matches!(status, "succeeded" | "cancelled" | "failed"),
                    None => entry.run.is_some() && is_terminal(entry.status()),
                };
                terminal.then(|| run_id.clone())
            })
        } else {
            None
        };
        if entries.len() >= APP_RUN_CAPACITY && completed_to_evict.is_none() {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                "应用运行注册表已满，所有保留任务仍在运行",
            ));
        }
        let watcher_slot_reserved = if self.persistence_root.is_some() {
            persistence::reserve_limited_slot(&APP_RUN_WATCHERS, APP_RUN_WATCHER_CAPACITY)
        } else {
            false
        };
        if self.persistence_root.is_some() && !watcher_slot_reserved {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                "异步运行持久化观察器已满；任务未启动，请稍后重试",
            ));
        }
        if let Some(oldest) = completed_to_evict {
            if let Some(root) = &self.persistence_root {
                if let Err(error) = persistence::remove(root, &oldest) {
                    if watcher_slot_reserved {
                        APP_RUN_WATCHERS.fetch_sub(1, Ordering::AcqRel);
                    }
                    return Err(error);
                }
            }
            entries.remove(&oldest);
        }
        entries.insert(
            run_id.clone(),
            AppRunEntry {
                run_id: run_id.clone(),
                run: None,
                cancel_requested: false,
                watcher_slot_reserved,
                result: None,
                error: None,
                restored_status: None,
                events: Vec::new(),
            },
        );
        if let Some(root) = &self.persistence_root {
            if let Err(error) = persistence::reserve(root, entries.get_mut(&run_id).unwrap()) {
                entries.remove(&run_id);
                if watcher_slot_reserved {
                    APP_RUN_WATCHERS.fetch_sub(1, Ordering::AcqRel);
                }
                return Err(error);
            }
        }
        Ok(())
    }

    fn attach(&self, run_id: &str, run: AppRun) -> Result<(), AppError> {
        let watcher_slot_reserved = match self.with_entry(run_id, |entry| {
            entry.run = Some(run);
            if entry.cancel_requested {
                entry.cancel();
            }
            entry.watcher_slot_reserved
        }) {
            Ok(reserved) => reserved,
            Err(error) => {
                if self.persistence_root.is_some() {
                    APP_RUN_WATCHERS.fetch_sub(1, Ordering::AcqRel);
                }
                return Err(error);
            }
        };
        if let Some(root) = self.persistence_root.clone() {
            if watcher_slot_reserved {
                let entries = Arc::clone(&self.entries);
                let run_id = run_id.to_string();
                let watcher_run_id = run_id.clone();
                let spawn = std::thread::Builder::new()
                    .name(format!("qx-run-persist-{}", run_id))
                    .spawn(move || {
                        let _watcher_slot = persistence::WatcherSlot;
                        let mut failed_terminal_writes = 0_u8;
                        let mut last_snapshot = None;
                        loop {
                            let state = match entries.lock() {
                                Ok(mut entries) => match entries.get_mut(&watcher_run_id) {
                                    Some(entry) => {
                                        entry.collect_result();
                                        let terminal = match entry.restored_status.as_deref() {
                                            Some(status) => {
                                                matches!(
                                                    status,
                                                    "succeeded" | "cancelled" | "failed"
                                                )
                                            }
                                            None => is_terminal(entry.status()),
                                        };
                                        let persisted = persistence::save_if_changed(
                                            &root,
                                            entry,
                                            &mut last_snapshot,
                                        )
                                        .is_ok();
                                        Some((terminal, persisted))
                                    }
                                    None => None,
                                },
                                Err(_) => None,
                            };
                            match state {
                                None | Some((true, true)) => break,
                                Some((true, false)) => {
                                    failed_terminal_writes =
                                        failed_terminal_writes.saturating_add(1);
                                    if failed_terminal_writes >= 100 {
                                        break;
                                    }
                                }
                                Some((false, _)) => failed_terminal_writes = 0,
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    });
                if let Err(error) = spawn {
                    APP_RUN_WATCHERS.fetch_sub(1, Ordering::AcqRel);
                    let _ = self.with_entry(&run_id, |entry| {
                        entry.watcher_slot_reserved = false;
                        entry.cancel();
                    });
                    return Err(AppError::from_io(
                        "启动异步运行结果持久化观察器；任务已请求取消",
                        &error,
                    ));
                }
            } else {
                return Err(AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "持久化运行缺少已预留的观察器名额",
                ));
            }
        }
        Ok(())
    }

    fn with_entry<T>(
        &self,
        run_id: &str,
        action: impl FnOnce(&mut AppRunEntry) -> T,
    ) -> Result<T, AppError> {
        self.ensure_storage_ready()?;
        let mut entries = self.entries.lock().map_err(|_| {
            AppError::new(AppErrorCategory::InternalInvariant, "应用运行注册表锁中毒")
        })?;
        let entry = entries.get_mut(run_id).ok_or_else(|| {
            AppError::new(
                AppErrorCategory::DataUnavailable,
                format!("未知 run_id: {run_id}"),
            )
        })?;
        let value = action(entry);
        if let Some(root) = &self.persistence_root {
            persistence::save(root, entry)?;
        }
        Ok(value)
    }

    fn ensure_storage_ready(&self) -> Result<(), AppError> {
        self.initialization_error.clone().map_or(Ok(()), Err)
    }

    pub(crate) fn status(&self, run_id: &str) -> ApiResponse {
        match self.with_entry(run_id, AppRunEntry::document) {
            Ok(document) => ApiResponse::json(200, document),
            Err(error) => respond(Err(error)),
        }
    }

    pub(crate) fn cancel(&self, run_id: &str) -> ApiResponse {
        match self.with_entry(run_id, |entry| {
            entry.cancel();
            entry.document()
        }) {
            Ok(document) => ApiResponse::json(202, document),
            Err(error) => respond(Err(error)),
        }
    }

    pub(crate) fn events(&self, run_id: &str, cursor: u64) -> ApiResponse {
        match self.with_entry(run_id, |entry| entry.events_document(cursor)) {
            Ok(Ok(document)) => ApiResponse::json(200, document),
            Ok(Err(error)) | Err(error) => respond(Err(error)),
        }
    }
}
