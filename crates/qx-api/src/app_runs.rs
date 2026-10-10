//! Asynchronous lifecycle routes for the shared application use cases.

use super::*;
use qx_app::{RunExperimentResult, RunHandle, RunStatus};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const APP_RUN_CAPACITY: usize = 1024;
pub(crate) const RUN_START_BACKTEST_ROUTE: &str = "/app/backtest/start";
pub(crate) const RUN_STATUS_ROUTE_TEMPLATE: &str = "/app/runs/{run_id}";
pub(crate) const RUN_CANCEL_ROUTE_TEMPLATE: &str = "/app/runs/{run_id}/cancel";
pub(crate) const RUN_START_DEPTH_ROUTE: &str = "/app/depth-backtest/start";
pub(crate) const RUN_START_EXPERIMENT_ROUTE: &str = "/app/run-experiment/start";

pub(crate) fn app_run_route(
    method: &str,
    route: &str,
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> Option<ApiResponse> {
    let response = match (method, route) {
        ("POST", RUN_START_BACKTEST_ROUTE) => start_run_backtest(body, paths, registry),
        ("POST", RUN_START_DEPTH_ROUTE) => start_depth_backtest(body, paths, registry),
        ("POST", RUN_START_EXPERIMENT_ROUTE) => start_experiment(body, paths, registry),
        ("GET", route) => status_route_id(route).map(|run_id| registry.status(run_id))?,
        ("POST", route) => cancel_route_id(route).map(|run_id| registry.cancel(run_id))?,
        _ => return None,
    };
    Some(response)
}

/// In-memory registry for async app runs exposed over HTTP. Terminal results are
/// retained for polling until capacity pressure evicts completed entries.
#[derive(Clone, Default)]
pub(crate) struct AppRunRegistry {
    entries: Arc<Mutex<BTreeMap<String, AppRunEntry>>>,
}

enum AppRun {
    Bar(RunHandle<BacktestOutcome>),
    Depth(RunHandle<DepthBacktestOutcome>),
    Experiment(RunHandle<RunExperimentResult>),
}

struct AppRunEntry {
    run_id: String,
    run: Option<AppRun>,
    cancel_requested: bool,
    result: Option<String>,
    error: Option<String>,
}

impl AppRunRegistry {
    fn reserve(&self, run_id: String) -> Result<(), AppError> {
        let mut entries = self.entries.lock().map_err(|_| {
            AppError::new(AppErrorCategory::InternalInvariant, "应用运行注册表锁中毒")
        })?;
        if entries.contains_key(&run_id) {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("run_id 已在本进程运行或保留: {run_id}"),
            ));
        }
        if entries.len() >= APP_RUN_CAPACITY {
            let completed = entries.iter_mut().find_map(|(run_id, entry)| {
                (entry.run.is_some() && is_terminal(entry.status())).then(|| run_id.clone())
            });
            if let Some(oldest) = completed {
                entries.remove(&oldest);
            } else {
                return Err(AppError::new(
                    AppErrorCategory::Timeout,
                    "应用运行注册表已满，所有保留任务仍在运行",
                ));
            }
        }
        entries.insert(
            run_id.clone(),
            AppRunEntry {
                run_id,
                run: None,
                cancel_requested: false,
                result: None,
                error: None,
            },
        );
        Ok(())
    }

    fn attach(&self, run_id: &str, run: AppRun) -> Result<(), AppError> {
        self.with_entry(run_id, |entry| {
            entry.run = Some(run);
            if entry.cancel_requested {
                entry.cancel();
            }
        })
    }

    fn with_entry<T>(
        &self,
        run_id: &str,
        action: impl FnOnce(&mut AppRunEntry) -> T,
    ) -> Result<T, AppError> {
        let mut entries = self.entries.lock().map_err(|_| {
            AppError::new(AppErrorCategory::InternalInvariant, "应用运行注册表锁中毒")
        })?;
        let entry = entries.get_mut(run_id).ok_or_else(|| {
            AppError::new(
                AppErrorCategory::DataUnavailable,
                format!("未知 run_id: {run_id}"),
            )
        })?;
        Ok(action(entry))
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
}

impl AppRunEntry {
    fn status(&mut self) -> RunStatus {
        match &mut self.run {
            None => RunStatus::Running,
            Some(AppRun::Bar(handle)) => handle.status(),
            Some(AppRun::Depth(handle)) => handle.status(),
            Some(AppRun::Experiment(handle)) => handle.status(),
        }
    }

    fn cancel(&mut self) {
        self.cancel_requested = true;
        match &mut self.run {
            Some(AppRun::Bar(handle)) => handle.cancel(),
            Some(AppRun::Depth(handle)) => handle.cancel(),
            Some(AppRun::Experiment(handle)) => handle.cancel(),
            None => {}
        }
    }

    fn collect_result(&mut self) {
        let status = self.status();
        if !is_terminal(status) || self.result.is_some() || self.error.is_some() {
            return;
        }
        let outcome = match &mut self.run {
            None => return,
            Some(AppRun::Bar(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::Depth(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::Experiment(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
        };
        if let Some(outcome) = outcome {
            match outcome {
                Ok(document) => self.result = Some(document),
                Err(error) => self.error = Some(error.to_json()),
            }
        }
    }

    fn document(&mut self) -> String {
        self.collect_result();
        let status = match self.status() {
            RunStatus::Running => "running",
            RunStatus::Cancelling => "cancelling",
            RunStatus::Succeeded => "succeeded",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Failed => "failed",
        };
        serde_json::json!({
            "run_id": self.run_id,
            "status": if self.run.is_none() { "starting" } else { status },
            "result": self.result.as_ref().and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok()),
            "error": self.error.as_ref().and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok()),
        })
        .to_string()
    }
}

fn is_terminal(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Succeeded | RunStatus::Cancelled | RunStatus::Failed
    )
}

pub(crate) fn status_route_id(route: &str) -> Option<&str> {
    let prefix = RUN_STATUS_ROUTE_TEMPLATE.strip_suffix("{run_id}")?;
    route
        .strip_prefix(prefix)
        .filter(|id| valid_run_route_id(id))
}

pub(crate) fn cancel_route_id(route: &str) -> Option<&str> {
    let (prefix, suffix) = RUN_CANCEL_ROUTE_TEMPLATE.split_once("{run_id}")?;
    route
        .strip_prefix(prefix)
        .and_then(|run_id| run_id.strip_suffix(suffix))
        .filter(|id| valid_run_route_id(id))
}

fn valid_run_route_id(run_id: &str) -> bool {
    !run_id.is_empty()
        && !run_id.contains('/')
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(crate) fn start_run_backtest(
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> ApiResponse {
    let mut spec = match BacktestSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    if let Err(error) = spec.validate() {
        return respond(Err(error));
    }
    if let Err(error) = resolve_input_path(&mut spec.bars_path, &paths.data_root) {
        return respond(Err(error));
    }
    let run_id = spec.run_id.clone();
    spec.output_dir = paths
        .artifact_root
        .join(&run_id)
        .to_string_lossy()
        .into_owned();
    if let Err(error) = registry.reserve(run_id.clone()) {
        return respond(Err(error));
    }
    let handle = spec.start(context(&run_id));
    match registry.attach(&run_id, AppRun::Bar(handle)) {
        Ok(()) => registry.status(&run_id),
        Err(error) => respond(Err(error)),
    }
}

/// 将 HTTP 输入文件限制在服务配置的数据根目录内；绝对路径只在根目录之下接受，
/// `..`、盘符跳转和指向根外的符号链接一律拒绝。
pub(super) fn resolve_input_path(path: &mut String, data_root: &Path) -> Result<(), AppError> {
    use std::path::Component;

    std::fs::create_dir_all(data_root).map_err(|error| {
        AppError::from_io(
            &format!("创建 API 数据根目录 {}", data_root.display()),
            &error,
        )
    })?;
    let root = std::fs::canonicalize(data_root).map_err(|error| {
        AppError::from_io(
            &format!("解析 API 数据根目录 {}", data_root.display()),
            &error,
        )
    })?;
    let input = Path::new(path);
    let lexical_root = if data_root.is_absolute() {
        data_root.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| AppError::from_io("解析 API 工作目录", &error))?
            .join(data_root)
    };
    let relative = if input.is_absolute() {
        input.strip_prefix(&lexical_root).map_err(|_| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                "HTTP 文件路径必须位于服务配置的根目录内",
            )
        })?
    } else {
        input
    };
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "HTTP 文件路径必须是配置根目录下的相对路径",
        ));
    }
    let candidate = root.join(relative);
    if candidate.exists() {
        let resolved = std::fs::canonicalize(&candidate).map_err(|error| {
            AppError::from_io(
                &format!("解析 API 数据文件 {}", candidate.display()),
                &error,
            )
        })?;
        if !resolved.starts_with(&root) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "HTTP 文件不能通过符号链接越出服务配置的根目录",
            ));
        }
        *path = resolved.to_string_lossy().into_owned();
    } else {
        *path = candidate.to_string_lossy().into_owned();
    }
    Ok(())
}

pub(crate) fn start_depth_backtest(
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> ApiResponse {
    let mut spec = match DepthBacktestSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    if let Err(error) = spec.validate() {
        return respond(Err(error));
    }
    if let Err(error) = resolve_input_path(&mut spec.depth_path, &paths.data_root) {
        return respond(Err(error));
    }
    let run_id = spec.run_id.clone();
    spec.output_dir = paths
        .artifact_root
        .join(&run_id)
        .to_string_lossy()
        .into_owned();
    if let Err(error) = registry.reserve(run_id.clone()) {
        return respond(Err(error));
    }
    let handle = spec.start(context(&run_id));
    match registry.attach(&run_id, AppRun::Depth(handle)) {
        Ok(()) => registry.status(&run_id),
        Err(error) => respond(Err(error)),
    }
}

pub(crate) fn start_experiment(
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> ApiResponse {
    let mut spec = match RunExperimentSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    if let Err(error) = spec.validate() {
        return respond(Err(error));
    }
    if let Err(error) = resolve_input_path(&mut spec.base.bars_path, &paths.data_root) {
        return respond(Err(error));
    }
    let run_id = spec.experiment_id.clone();
    spec.base.output_dir = paths
        .artifact_root
        .join(&run_id)
        .to_string_lossy()
        .into_owned();
    if let Err(error) = registry.reserve(run_id.clone()) {
        return respond(Err(error));
    }
    let handle = spec.start(context(&run_id));
    match registry.attach(&run_id, AppRun::Experiment(handle)) {
        Ok(()) => registry.status(&run_id),
        Err(error) => respond(Err(error)),
    }
}
