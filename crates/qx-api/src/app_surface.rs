//! `POST /app/*`：应用层用例的 HTTP 门面。
//!
//! HTTP、CLI 的 `app` 子命令和 Python SDK **调的是同一组 `qx-app` 用例**，
//! 请求体与响应体就是用例的 spec / 结果 JSON。所以「同一 use case 从三个入口调，结果哈希相同、
//! 错误 code 与 correlation id 相同」是构造性的：这里没有第二份装配、没有第二份错误映射。
//!
//! ## 为什么是 `POST` 而不是 `GET`
//!
//! 每个入口的输入是一份**文档**（如 `DatasetSpec` / `BacktestSpec` / `BacktestOutcome` / `CompareRunsSpec`），不是
//! 几个标量键。把它摊成查询串会逼出一个"HTTP 侧独有的输入形状"，那正是 §15.2 要消灭的分叉。
//! 所以走请求体。`validate-dataset` 与 `verify` 本身是纯读；`backtest` 会在**服务进程的**
//! 文件系统上落四份产物——这是 R 档（研究，无外部账户副作用），但对服务面而言是**重**入口，
//! 它的定位是本地/受信运维，不是公网批量调用面。这些入口都不在策略白名单的免鉴权名单里，
//! 所以配了 `policy` 的部署里它们与 `/control/commands` 同一把锁。
//!
//! ## 状态码只从类别派生
//!
//! [`respond`] 里那一张 `match` 是「应用层类别 → HTTP 状态」的**唯一**一份映射表。调用方按
//! 响应体里的类别字段分支（响应体就是 [`AppError::to_json`]），状态码只是给不读正文的中间件
//! 的提示，两处不会各说各话。
//!
//! 那一张 `match` 刻意写成 `ApiResponse::json(<字面量>, …)` 而不是"先算一个 u16 再传进去"：
//! `crates/qx-cli/src/tests/api_endpoint_table_routes.rs` 的 `emitted_statuses()` 就是按
//! 那个形态取数的，写成变量会让接口文档里承诺的状态码在判据眼里**不存在**——于是"文档说会回
//! 422"与"实现真的回 422"之间就没有东西守着了。

use crate::ApiResponse;
use qx_app::{
    compare_runs, run_backtest, validate_dataset, verify_run, AppError, AppErrorCategory,
    BacktestOutcome, BacktestSpec, CallerCapability, CompareRunsSpec, DatasetSpec, RunContext,
};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct AppPaths {
    pub data_root: PathBuf,
    pub artifact_root: PathBuf,
}

impl AppPaths {
    pub(crate) fn from_environment() -> Self {
        let working_directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            data_root: std::env::var_os("QX_API_DATA_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| working_directory.join(".qianxing").join("api-data")),
            artifact_root: std::env::var_os("QX_API_ARTIFACT_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("qianxing-api-runs")),
        }
    }
}

/// 用例结果与失败都走这一条出口：成功回 200 + 结果文档，失败回「类别派生状态码 + 错误文档」。
///
/// 错误文档就是 [`AppError::to_json`] 的原文——与 CLI 的 stderr 那行、Python 的异常载荷逐字节相同。
/// 状态码表见模块文档（为什么必须写成字面量）。
fn respond(result: Result<String, AppError>) -> ApiResponse {
    match result {
        Ok(payload) => ApiResponse::json(200, payload),
        Err(error) => {
            let document = error.to_json();
            match error.category() {
                AppErrorCategory::InvalidInput => ApiResponse::json(400, document),
                AppErrorCategory::DataUnavailable => ApiResponse::json(404, document),
                AppErrorCategory::FidelityInsufficient => ApiResponse::json(422, document),
                AppErrorCategory::PermissionDenied => ApiResponse::json(403, document),
                AppErrorCategory::Conflict => ApiResponse::json(409, document),
                AppErrorCategory::Timeout => ApiResponse::json(503, document),
                AppErrorCategory::StorageFailure => ApiResponse::json(503, document),
                AppErrorCategory::InternalInvariant => ApiResponse::json(500, document),
            }
        }
    }
}

/// 调用上下文：与 CLI/Python 两侧同形（R 档 + 身份串当 correlation id）。
///
/// `code_commit` 用 `qx-app` 自己的包版本而不是服务的构建身份：这一层拿不到 CLI 的
/// `QX_GIT_COMMIT`，写一个看起来像提交号的字符串只会让产物里那一格撒谎。
fn context(correlation_id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, correlation_id)
}

pub(crate) fn post_validate_dataset(body: &str, data_root: &Path) -> ApiResponse {
    let mut spec = match DatasetSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    if let Err(error) = resolve_input_path(&mut spec.bars_path, data_root) {
        return respond(Err(error));
    }
    respond(
        validate_dataset(&spec, &context(&spec.dataset_id)).and_then(|verdict| verdict.to_json()),
    )
}

pub(crate) fn post_run_backtest(body: &str, data_root: &Path, artifact_root: &Path) -> ApiResponse {
    let mut spec = match BacktestSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    if let Err(error) = spec.validate() {
        return respond(Err(error));
    }
    if let Err(error) = resolve_input_path(&mut spec.bars_path, data_root) {
        return respond(Err(error));
    }
    // HTTP 调用方不能指定服务端任意写入路径。run_id 已由 spec.validate() 限定为安全文件名，
    // 产物根目录由服务部署者配置，HTTP 请求里的 output_dir 只用于兼容共享 schema。
    spec.output_dir = artifact_root
        .join(&spec.run_id)
        .to_string_lossy()
        .into_owned();
    respond(run_backtest(&spec, &context(&spec.run_id)).and_then(|outcome| outcome.to_json()))
}

/// 将 HTTP 输入文件限制在服务配置的数据根目录内；绝对路径只在根目录之下接受，
/// `..`、盘符跳转和指向根外的符号链接一律拒绝。
fn resolve_input_path(path: &mut String, data_root: &Path) -> Result<(), AppError> {
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
                "HTTP 数据路径必须位于服务配置的数据根目录内",
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
            "HTTP 数据路径必须是数据根目录下的相对文件路径",
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
                "HTTP 数据文件不能通过符号链接越出服务配置的数据根目录",
            ));
        }
        *path = resolved.to_string_lossy().into_owned();
    } else {
        *path = candidate.to_string_lossy().into_owned();
    }
    Ok(())
}

pub(crate) fn post_verify_run(body: &str) -> ApiResponse {
    let outcome = match BacktestOutcome::from_json(body) {
        Ok(outcome) => outcome,
        Err(error) => return respond(Err(error)),
    };
    respond(verify_run(&outcome, &context(&outcome.run_id)).and_then(|result| result.to_json()))
}

pub(crate) fn post_compare_runs(body: &str) -> ApiResponse {
    let spec = match CompareRunsSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    respond(compare_runs(&spec, &context("compare-runs")).and_then(|result| result.to_json()))
}
