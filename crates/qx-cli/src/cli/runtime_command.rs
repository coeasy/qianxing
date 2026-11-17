//! CLI adapters for runtime diagnostics, status, and unified run entrypoints.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn dispatch_doctor(
    path: Option<PathBuf>,
    json: bool,
    capabilities: bool,
    capability_id: Option<String>,
    venue: Option<String>,
    product: Option<String>,
    account_mode: Option<String>,
    account_model: Option<String>,
    margin_mode: Option<String>,
    operation: Option<String>,
) {
    let result = if capabilities {
        run_doctor_capabilities(
            json,
            capability_id.as_deref(),
            venue.as_deref(),
            product.as_deref(),
            account_mode.as_deref(),
            account_model.as_deref(),
            margin_mode.as_deref(),
            operation.as_deref(),
        )
    } else {
        let path = path.unwrap_or_else(default_runtime_path);
        run_doctor(&path, json)
    };
    if let Err(error) = result {
        eprintln!("[qx-cli · CLI] Doctor 失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_run(action: Option<RunCommand>) {
    let arguments = match action {
        Some(RunCommand::Backtest { arguments }) => run_arguments("backtest", arguments),
        Some(RunCommand::Paper { arguments }) => run_arguments("paper", arguments),
        Some(RunCommand::PaperCheck { arguments }) => run_arguments("paper-check", arguments),
        Some(RunCommand::Doctor { arguments }) => run_arguments("doctor", arguments),
        Some(RunCommand::LiveCheck { arguments }) => run_arguments("live-check", arguments),
        Some(RunCommand::RuntimeCheck { arguments }) => run_arguments("runtime-check", arguments),
        Some(RunCommand::Report { arguments }) => run_arguments("report", arguments),
        None => Vec::new(),
    };
    if let Err(error) = run_unified_command(&arguments) {
        eprintln!("[qx-cli · CLI] 统一运行入口失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_status(path: Option<PathBuf>, json: bool) {
    let path = path.unwrap_or_else(default_runtime_path);
    if let Err(error) = run_status(&path, json) {
        eprintln!("[qx-cli · CLI] 状态查看失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_live_check(path: PathBuf, json: bool) {
    if let Err(error) = run_live_check(&path, json) {
        eprintln!("[qx-cli · CLI] 实盘前置检查失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_runtime_check(path: PathBuf, json: bool) {
    if let Err(error) = run_runtime_check(&path, json) {
        eprintln!("[qx-cli · CLI] 运行时配置校验失败: {error}");
        std::process::exit(2);
    }
}
